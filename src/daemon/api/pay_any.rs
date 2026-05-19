use std::str::FromStr;
use std::time::Duration;

use bitcoin_payment_instructions::amount::Amount as InstructionAmount;
use bitcoin_payment_instructions::http_resolver::HTTPHrnResolver;
use bitcoin_payment_instructions::{
    ConfigurableAmountPaymentInstructions, FixedAmountPaymentInstructions, ParseError,
    PaymentInstructions, PaymentMethod,
};
use hex::DisplayHex;
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning::offers::offer::Offer;
use ldk_node::lightning_invoice::Bolt11Invoice;
use ldk_node::payment::{PaymentDetails, PaymentKind, PaymentStatus};
use log::error;
use tokio::sync::broadcast;

use mdk::types::MdkEvent;

use crate::daemon::api::error::AppError;
use crate::daemon::api::AppState;
use crate::daemon::types::{PayRequest, PayResponse, PayStatus};

/// Hard cap on caller-supplied wait. Higher values would routinely
/// exceed reverse-proxy idle timeouts (ALB/CloudFront default 60s).
const MAX_WAIT_SECS: u64 = 50;
const DEFAULT_WAIT_SECS: u64 = 30;

enum PaymentTarget {
    Bolt11 {
        invoice: Bolt11Invoice,
        amount_msat: u64,
    },
    Bolt12 {
        offer: Box<Offer>,
        amount_msat: u64,
    },
}

pub async fn handle_pay(state: AppState, req: &PayRequest) -> Result<PayResponse, AppError> {
    if let Some(amount_sat) = req.amount_sat {
        if amount_sat == 0 {
            return Err(AppError::BadRequest(
                "amountSat must be greater than zero".into(),
            ));
        }
    }

    let wait_secs = req.wait_for_payment_secs.unwrap_or(DEFAULT_WAIT_SECS);
    if wait_secs > MAX_WAIT_SECS {
        return Err(AppError::BadRequest(format!(
            "waitForPaymentSecs must be <= {MAX_WAIT_SECS}"
        )));
    }

    let amount_msat = match req.amount_sat {
        Some(s) => Some(
            s.checked_mul(1000)
                .ok_or_else(|| AppError::BadRequest("amountSat overflow".into()))?,
        ),
        None => None,
    };

    let network = state.node.config().network;
    let resolver = HTTPHrnResolver::with_client(state.mdk_client.http_client().clone());
    let instructions = PaymentInstructions::parse(req.destination.trim(), network, &resolver, true)
        .await
        .map_err(map_parse_error)?;

    let target = match instructions {
        PaymentInstructions::FixedAmount(fixed) => {
            resolve_fixed(fixed, amount_msat, &req.destination)?
        }
        PaymentInstructions::ConfigurableAmount(configurable) => {
            let requested_msat = amount_msat.ok_or_else(|| {
                AppError::BadRequest(
                    "amountSat is required for variable-amount destinations".into(),
                )
            })?;
            resolve_configurable(configurable, requested_msat, &resolver).await?
        }
    };

    if matches!(target, PaymentTarget::Bolt11 { .. }) {
        if req.payer_note.is_some() {
            return Err(AppError::BadRequest(
                "payerNote is only supported on BOLT12 destinations".into(),
            ));
        }
        if req.quantity.is_some() {
            return Err(AppError::BadRequest(
                "quantity is only supported on BOLT12 destinations".into(),
            ));
        }
    }

    if wait_secs == 0 {
        let payment_id = dispatch(&state, &target, req)?;
        let details = state.node.payment(&payment_id);
        return Ok(build_response(
            payment_id,
            details,
            &target,
            PayStatus::Pending,
        ));
    }

    // Subscribe BEFORE dispatch to avoid losing a fast-failing event.
    let mut rx = state.mdk_client.subscribe();
    let payment_id = dispatch(&state, &target, req)?;
    let payment_id_hex = payment_id.0.to_lower_hex_string();

    let _ = tokio::time::timeout(Duration::from_secs(wait_secs), async {
        loop {
            match rx.recv().await {
                Ok(MdkEvent::PaymentSuccessful { ref payment_id, .. })
                | Ok(MdkEvent::PaymentFailed { ref payment_id, .. })
                    if *payment_id == payment_id_hex =>
                {
                    return;
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                // Sender dropped — treat as pending; the payment may still settle.
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
    .await;

    let details = state.node.payment(&payment_id);
    let fallback = match &details {
        Some(d) => map_status(d.status),
        None => PayStatus::Pending,
    };
    Ok(build_response(payment_id, details, &target, fallback))
}

fn resolve_fixed(
    fixed: FixedAmountPaymentInstructions,
    requested_msat: Option<u64>,
    destination: &str,
) -> Result<PaymentTarget, AppError> {
    let invoice_msat = fixed
        .ln_payment_amount()
        .ok_or_else(|| AppError::BadRequest("destination has no lightning amount".into()))?
        .milli_sats();

    if let Some(requested) = requested_msat {
        if requested != invoice_msat {
            return Err(AppError::BadRequest(format!(
                "amountSat ({}) does not match invoice amount ({} sat)",
                requested / 1000,
                invoice_msat / 1000
            )));
        }
    }

    pick_method(fixed.methods(), invoice_msat, destination)
}

async fn resolve_configurable(
    configurable: ConfigurableAmountPaymentInstructions,
    requested_msat: u64,
    resolver: &HTTPHrnResolver,
) -> Result<PaymentTarget, AppError> {
    let amount = InstructionAmount::from_milli_sats(requested_msat)
        .map_err(|_| AppError::BadRequest("amountSat exceeds maximum".into()))?;
    let fixed = configurable
        .set_amount(amount, resolver)
        .await
        .map_err(map_set_amount_error)?;

    let target = pick_method(fixed.methods(), requested_msat, "")?;

    // Mirror lightning-js's malicious-LNURL check. The library already
    // enforces this internally on resolve_lnurl_to_invoice, but we keep
    // an explicit check as a defense-in-depth gate.
    if let PaymentTarget::Bolt11 { invoice, .. } = &target {
        if let Some(inv_msat) = invoice.amount_milli_satoshis() {
            if inv_msat != requested_msat {
                return Err(AppError::BadRequest(format!(
                    "resolved invoice amount ({inv_msat}msat) does not match requested ({requested_msat}msat)"
                )));
            }
        }
    }

    Ok(target)
}

fn pick_method(
    methods: &[PaymentMethod],
    amount_msat: u64,
    _destination: &str,
) -> Result<PaymentTarget, AppError> {
    // Prefer BOLT11 over BOLT12 when both are present (lower routing latency).
    let bolt11 = methods.iter().find_map(|m| match m {
        PaymentMethod::LightningBolt11(inv) => Some(inv),
        _ => None,
    });

    if let Some(inv) = bolt11 {
        // Convert across crate-version boundary via serialization. The
        // bitcoin-payment-instructions crate pins a different rust-lightning
        // git rev than ldk-node, so the two `Bolt11Invoice` types are
        // distinct even though the wire format is identical.
        let parsed = Bolt11Invoice::from_str(&inv.to_string())
            .map_err(|e| AppError::Internal(format!("failed to re-parse resolved bolt11: {e}")))?;
        return Ok(PaymentTarget::Bolt11 {
            invoice: parsed,
            amount_msat,
        });
    }

    let bolt12 = methods.iter().find_map(|m| match m {
        PaymentMethod::LightningBolt12(offer) => Some(offer),
        _ => None,
    });

    if let Some(offer) = bolt12 {
        let parsed = Offer::from_str(&offer.to_string()).map_err(|e| {
            AppError::Internal(format!("failed to re-parse resolved bolt12: {e:?}"))
        })?;
        return Ok(PaymentTarget::Bolt12 {
            offer: Box::new(parsed),
            amount_msat,
        });
    }

    Err(AppError::BadRequest(
        "no supported lightning payment method (need BOLT11 or BOLT12)".into(),
    ))
}

fn dispatch(
    state: &AppState,
    target: &PaymentTarget,
    req: &PayRequest,
) -> Result<PaymentId, AppError> {
    match target {
        PaymentTarget::Bolt11 {
            invoice,
            amount_msat,
        } => {
            let bolt11 = state.node.bolt11_payment();
            match invoice.amount_milli_satoshis() {
                Some(_) => bolt11
                    .send(invoice, None)
                    .map_err(|e| AppError::Internal(format!("pay failed: {e}"))),
                None => bolt11
                    .send_using_amount(invoice, *amount_msat, None)
                    .map_err(|e| AppError::Internal(format!("pay failed: {e}"))),
            }
        }
        PaymentTarget::Bolt12 { offer, amount_msat } => state
            .node
            .bolt12_payment()
            .send_using_amount(
                offer,
                *amount_msat,
                req.quantity,
                req.payer_note.clone(),
                None,
            )
            .map_err(|e| AppError::Internal(format!("pay failed: {e}"))),
    }
}

fn build_response(
    payment_id: PaymentId,
    details: Option<PaymentDetails>,
    target: &PaymentTarget,
    fallback_status: PayStatus,
) -> PayResponse {
    // The BOLT11 invoice always knows its hash; surface it even if LDK
    // hasn't recorded the payment yet.
    let target_hash = match target {
        PaymentTarget::Bolt11 { invoice, .. } => Some(invoice.payment_hash().to_string()),
        PaymentTarget::Bolt12 { .. } => None,
    };

    let payment_id_hex = payment_id.0.to_lower_hex_string();

    match details {
        Some(d) => {
            let details_hash = extract_hash(&d.kind);
            let preimage = extract_preimage(&d.kind);
            let status = map_status(d.status);
            let reason = if matches!(status, PayStatus::Failed) {
                Some("payment failed".to_string())
            } else {
                None
            };
            PayResponse {
                payment_id: payment_id_hex,
                payment_hash: target_hash.or(details_hash),
                preimage,
                fee_sat: d.fee_paid_msat.map(|m| m / 1000),
                status,
                reason,
            }
        }
        None => PayResponse {
            payment_id: payment_id_hex,
            payment_hash: target_hash,
            preimage: None,
            fee_sat: None,
            status: fallback_status,
            reason: None,
        },
    }
}

fn map_status(status: PaymentStatus) -> PayStatus {
    match status {
        PaymentStatus::Succeeded => PayStatus::Succeeded,
        PaymentStatus::Failed => PayStatus::Failed,
        PaymentStatus::Pending => PayStatus::Pending,
    }
}

fn extract_hash(kind: &PaymentKind) -> Option<String> {
    match kind {
        PaymentKind::Bolt11 { hash, .. } | PaymentKind::Bolt11Jit { hash, .. } => {
            Some(hash.to_string())
        }
        PaymentKind::Spontaneous { hash, .. } => Some(hash.to_string()),
        PaymentKind::Bolt12Offer { hash, .. } | PaymentKind::Bolt12Refund { hash, .. } => {
            hash.map(|h| h.to_string())
        }
        PaymentKind::Onchain { .. } => None,
    }
}

fn extract_preimage(kind: &PaymentKind) -> Option<String> {
    match kind {
        PaymentKind::Bolt11 { preimage, .. }
        | PaymentKind::Bolt11Jit { preimage, .. }
        | PaymentKind::Bolt12Offer { preimage, .. }
        | PaymentKind::Bolt12Refund { preimage, .. }
        | PaymentKind::Spontaneous { preimage, .. } => preimage.map(|p| p.to_string()),
        PaymentKind::Onchain { .. } => None,
    }
}

fn map_parse_error(err: ParseError) -> AppError {
    match err {
        // Resolver transport classification: if the resolver returned a
        // "fetch"-style message, treat as 500. Other resolver responses
        // (bad tag, bad metadata, etc.) are caller-controllable input.
        // The library lumps reqwest send errors and JSON parse errors
        // under the same "Failed to fetch ..." string, so unparseable JSON
        // also lands here as 500.
        ParseError::HrnResolutionError(msg) if is_transport_msg(msg) => {
            error!("LNURL/HRN resolver transport failure: {msg}");
            AppError::Internal(format!("destination resolver failed: {msg}"))
        }
        ParseError::HrnResolutionError(msg) => {
            AppError::BadRequest(format!("destination resolver returned invalid data: {msg}"))
        }
        e => AppError::BadRequest(format!("invalid destination: {e:?}")),
    }
}

fn map_set_amount_error(msg: &'static str) -> AppError {
    if is_transport_msg(msg) {
        error!("LNURL callback transport failure: {msg}");
        AppError::Internal(format!("destination resolver failed: {msg}"))
    } else if msg.contains("wrong amount") {
        AppError::BadRequest(format!("malicious LNURL: {msg}"))
    } else {
        AppError::BadRequest(format!("destination resolver returned invalid data: {msg}"))
    }
}

fn is_transport_msg(msg: &str) -> bool {
    msg.contains("Failed to fetch") || msg.contains("callback failed")
}
