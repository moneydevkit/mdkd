use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::Path;
use axum::Json;
use chrono::{DateTime, SecondsFormat};
use hex::FromHex;
use ldk_node::bitcoin::hashes::sha256;
use ldk_node::bitcoin::hashes::Hash as _;
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description, Sha256};
use ldk_node::payment::{PaymentDetails, PaymentKind, PaymentStatus};
use ldk_node::Node;
use log::{error, info};

use crate::api::error::AppError;
use crate::mdk::client::MdkApiClient;
use crate::mdk::types::{CheckoutCustomer, CreateCheckoutRequest, RegisterInvoiceRequest};
use crate::store::invoice_metadata::{InvoiceMetadata, InvoiceMetadataStore};
use crate::types::{
    CreateInvoiceRequest, CreateInvoiceResponse, IncomingPaymentResponse, ListPaymentsRequest,
};

/// Cap to keep BOLT11 invoices compact (smaller QR codes).
/// Not a spec limit. Use `descriptionHash` for longer descriptions.
const MAX_DESCRIPTION_LEN: usize = 128;

const DEFAULT_EXPIRY_SECS: u32 = 3600;

fn parse_description(req: &CreateInvoiceRequest) -> Result<Bolt11InvoiceDescription, AppError> {
    match (&req.description, &req.description_hash) {
        (Some(desc), None) => {
            if desc.len() > MAX_DESCRIPTION_LEN {
                return Err(AppError::BadRequest(format!(
                    "description too long (max {MAX_DESCRIPTION_LEN} characters)"
                )));
            }
            let description = Description::new(desc.clone())
                .map_err(|e| AppError::BadRequest(format!("Invalid description: {e}")))?;
            Ok(Bolt11InvoiceDescription::Direct(description))
        }
        (None, Some(hash_hex)) => {
            let bytes = <[u8; 32]>::from_hex(hash_hex)
                .map_err(|e| AppError::BadRequest(format!("Invalid descriptionHash: {e}")))?;
            Ok(Bolt11InvoiceDescription::Hash(Sha256(
                sha256::Hash::from_byte_array(bytes),
            )))
        }
        _ => Err(AppError::BadRequest(
            "Must provide either description or descriptionHash".into(),
        )),
    }
}

pub async fn handle_create_invoice(
    node: Arc<Node>,
    metadata_store: Arc<InvoiceMetadataStore>,
    mdk_client: Arc<MdkApiClient>,
    req: &CreateInvoiceRequest,
) -> Result<Json<CreateInvoiceResponse>, AppError> {
    let description = parse_description(req)?;
    let expiry_secs = req.expiry_seconds.unwrap_or(DEFAULT_EXPIRY_SECS);

    let (invoice, checkout_id) =
        create_with_checkout(&node, &mdk_client, &description, req, expiry_secs).await?;

    let payment_hash = invoice.payment_hash().to_string();
    let expires_at = invoice.expires_at().map(|d| d.as_secs()).unwrap_or(0);
    let amount_sat = invoice.amount_milli_satoshis().map(|m| m / 1000);

    let invoice_str = invoice.to_string();
    let metadata = InvoiceMetadata {
        payment_hash: payment_hash.clone(),
        external_id: req.external_id.clone(),
        webhook_url: req.webhook_url.clone(),
        checkout_id,
        description: req.description.clone(),
        invoice: Some(invoice_str.clone()),
        amount_sat,
        created_at: crate::time::seconds_since_epoch(),
        expires_at,
    };

    metadata_store
        .insert(&metadata)
        .map_err(|e| AppError::Internal(format!("Failed to store invoice metadata: {e}")))?;

    Ok(Json(CreateInvoiceResponse {
        amount_sat,
        payment_hash,
        serialized: invoice_str,
        checkout_id: metadata.checkout_id,
    }))
}

async fn create_with_checkout(
    node: &Node,
    client: &MdkApiClient,
    description: &Bolt11InvoiceDescription,
    req: &CreateInvoiceRequest,
    expiry_secs: u32,
) -> Result<(Bolt11Invoice, String), AppError> {
    let products = req.product.as_ref().map(|p| vec![p.clone()]);

    let metadata: Option<serde_json::Value> = req
        .metadata
        .as_ref()
        .map(|s| serde_json::from_str(s))
        .transpose()
        .map_err(|e| AppError::BadRequest(format!("Invalid metadata JSON: {e}")))?;

    let customer = if req.customer_name.is_some()
        || req.customer_email.is_some()
        || req.customer_external_id.is_some()
    {
        Some(CheckoutCustomer {
            name: req.customer_name.clone(),
            email: req.customer_email.clone(),
            external_id: req.customer_external_id.clone(),
        })
    } else {
        None
    };

    let checkout_req = CreateCheckoutRequest {
        node_id: node.node_id().to_string(),
        amount: req.amount_sat,
        currency: req.currency.clone().or_else(|| Some("SAT".into())),
        products,
        success_url: req.success_url.clone(),
        metadata,
        customer,
    };

    let checkout = client.create_checkout(&checkout_req).await.map_err(|e| {
        error!("MDK checkout/create failed: {e}");
        AppError::Internal(format!("Failed to create checkout: {e}"))
    })?;

    info!(
        "Created checkout {} (status: {})",
        checkout.id, checkout.status
    );

    // Use the amount from the checkout (authoritative for product-based checkouts).
    let amount_msat = match checkout.invoice_amount_sats {
        Some(sats) => Some(sats * 1000),
        None => req.amount_sat.map(|s| s * 1000),
    };

    let invoice = create_jit_invoice(node, amount_msat, description, expiry_secs)?;

    let scid = extract_scid(&invoice);
    let payment_hash = invoice.payment_hash().to_string();
    let expires_at_iso = invoice
        .expires_at()
        .and_then(|d| {
            DateTime::from_timestamp(d.as_secs() as i64, 0)
                .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
        })
        .unwrap_or_default();

    let register_req = RegisterInvoiceRequest {
        node_id: node.node_id().to_string(),
        scid,
        checkout_id: checkout.id.clone(),
        invoice: invoice.to_string(),
        payment_hash,
        invoice_expires_at: expires_at_iso,
    };

    let _registered = client.register_invoice(&register_req).await.map_err(|e| {
        error!("MDK checkout/registerInvoice failed: {e}");
        AppError::Internal(format!("Failed to register invoice: {e}"))
    })?;

    Ok((invoice, checkout.id))
}

fn create_jit_invoice(
    node: &Node,
    amount_msat: Option<u64>,
    description: &Bolt11InvoiceDescription,
    expiry_secs: u32,
) -> Result<Bolt11Invoice, AppError> {
    node.bolt11_payment()
        .receive_via_lsps4_jit_channel(amount_msat, description, expiry_secs)
        .map_err(|e| AppError::Internal(format!("Failed to create JIT invoice: {e}")))
}

fn extract_scid(invoice: &Bolt11Invoice) -> String {
    invoice
        .route_hints()
        .iter()
        .flat_map(|hint| &hint.0)
        .next()
        .map(|hop| hop.short_channel_id.to_string())
        .unwrap_or_default()
}

pub async fn handle_get_incoming_payment(
    node: Arc<Node>,
    metadata_store: Arc<InvoiceMetadataStore>,
    Path(payment_hash): Path<String>,
) -> Result<Json<IncomingPaymentResponse>, AppError> {
    let metadata = metadata_store
        .get_by_payment_hash(&payment_hash)
        .map_err(|e| AppError::Internal(format!("Failed to query metadata: {}", e)))?
        .ok_or_else(|| AppError::NotFound(format!("Invoice {} not found", payment_hash)))?;

    let hash_bytes = <[u8; 32]>::from_hex(&payment_hash)
        .map_err(|_| AppError::BadRequest("Invalid payment hash hex".into()))?;
    let details = node.payment(&PaymentId(hash_bytes));

    Ok(Json(enrich_metadata(&metadata, details.as_ref())))
}

pub async fn handle_list_incoming_payments(
    node: Arc<Node>,
    metadata_store: Arc<InvoiceMetadataStore>,
    params: &ListPaymentsRequest,
) -> Result<Json<Vec<IncomingPaymentResponse>>, AppError> {
    let now = crate::time::seconds_since_epoch();
    let from = params.from.unwrap_or(0);
    let to = params.to.unwrap_or(now);
    let limit = params.limit.unwrap_or(20);
    let offset = params.offset.unwrap_or(0);
    let all = params.all.unwrap_or(false);

    let rows = metadata_store
        .list(from, to, limit, offset, all, params.external_id.as_deref())
        .map_err(|e| AppError::Internal(format!("Failed to list invoices: {e}")))?;

    let wanted: HashSet<[u8; 32]> = rows
        .iter()
        .filter_map(|m| <[u8; 32]>::from_hex(&m.payment_hash).ok())
        .collect();

    let payment_map: HashMap<[u8; 32], PaymentDetails> = node
        .list_payments_with_filter(|p| wanted.contains(&p.id.0))
        .into_iter()
        .map(|p| (p.id.0, p))
        .collect();

    let payments = rows
        .iter()
        .map(|m| {
            let details = match <[u8; 32]>::from_hex(&m.payment_hash) {
                Ok(bytes) => payment_map.get(&bytes),
                Err(_) => {
                    error!("Corrupt payment_hash in DB: {}", m.payment_hash);
                    None
                }
            };
            enrich_metadata(m, details)
        })
        .collect();

    Ok(Json(payments))
}

/// Build an `IncomingPaymentResponse` from stored metadata + LDK payment details.
fn enrich_metadata(
    metadata: &InvoiceMetadata,
    details: Option<&PaymentDetails>,
) -> IncomingPaymentResponse {
    let requested_sat = metadata.amount_sat;

    let (is_paid, preimage, received_sat, completed_at) = match details {
        Some(d) => {
            let preimage = extract_preimage(&d.kind);
            let is_paid = d.status == PaymentStatus::Succeeded;
            let received_sat = if is_paid {
                d.amount_msat.unwrap_or(0) / 1000
            } else {
                0
            };
            let completed_at = if is_paid {
                Some(d.latest_update_timestamp)
            } else {
                None
            };
            (is_paid, preimage, received_sat, completed_at)
        }
        None => (false, None, 0, None),
    };

    let now = crate::time::seconds_since_epoch();
    let is_expired = !is_paid && metadata.expires_at > 0 && metadata.expires_at <= now;
    let fees = if is_paid {
        requested_sat.unwrap_or(0).saturating_sub(received_sat)
    } else {
        0
    };

    let expires_at = if metadata.expires_at > 0 {
        Some(metadata.expires_at)
    } else {
        None
    };

    IncomingPaymentResponse {
        payment_hash: metadata.payment_hash.clone(),
        preimage,
        external_id: metadata.external_id.clone(),
        description: metadata.description.clone(),
        invoice: metadata.invoice.clone(),
        is_paid,
        is_expired,
        requested_sat,
        received_sat,
        fees,
        completed_at,
        created_at: metadata.created_at,
        expires_at,
    }
}

fn extract_preimage(kind: &PaymentKind) -> Option<String> {
    match kind {
        PaymentKind::Bolt11 { preimage, .. }
        | PaymentKind::Bolt11Jit { preimage, .. }
        | PaymentKind::Bolt12Offer { preimage, .. }
        | PaymentKind::Bolt12Refund { preimage, .. }
        | PaymentKind::Spontaneous { preimage, .. } => preimage.map(|p| format!("{}", p)),
        PaymentKind::Onchain { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldk_node::lightning::types::payment::{PaymentHash, PaymentPreimage};
    use ldk_node::payment::PaymentDirection;

    fn test_metadata() -> InvoiceMetadata {
        InvoiceMetadata {
            payment_hash: "aa".repeat(32),
            external_id: Some("ext-1".into()),
            webhook_url: None,
            checkout_id: "chk_1".into(),
            description: Some("coffee".into()),
            invoice: Some("lnbcrt1...".into()),
            amount_sat: Some(1000),
            created_at: 1700000000,
            expires_at: 1700003600,
        }
    }

    fn paid_details() -> PaymentDetails {
        PaymentDetails {
            id: PaymentId([0xaa; 32]),
            kind: PaymentKind::Bolt11 {
                hash: PaymentHash([0xaa; 32]),
                preimage: Some(PaymentPreimage([0xbb; 32])),
                secret: None,
            },
            amount_msat: Some(950_000),
            fee_paid_msat: None,
            direction: PaymentDirection::Inbound,
            status: PaymentStatus::Succeeded,
            latest_update_timestamp: 1700001000,
        }
    }

    fn pending_details() -> PaymentDetails {
        PaymentDetails {
            id: PaymentId([0xaa; 32]),
            kind: PaymentKind::Bolt11 {
                hash: PaymentHash([0xaa; 32]),
                preimage: None,
                secret: None,
            },
            amount_msat: None,
            fee_paid_msat: None,
            direction: PaymentDirection::Inbound,
            status: PaymentStatus::Pending,
            latest_update_timestamp: 1700000500,
        }
    }

    #[test]
    fn enrich_no_details() {
        let m = test_metadata();
        let r = enrich_metadata(&m, None);
        assert!(!r.is_paid);
        assert!(r.preimage.is_none());
        assert_eq!(r.received_sat, 0);
        assert_eq!(r.fees, 0);
        assert!(r.completed_at.is_none());
        assert_eq!(r.requested_sat, Some(1000));
        assert_eq!(r.payment_hash, m.payment_hash);
        assert_eq!(r.external_id.as_deref(), Some("ext-1"));
    }

    #[test]
    fn enrich_paid() {
        let m = test_metadata();
        let d = paid_details();
        let r = enrich_metadata(&m, Some(&d));
        assert!(r.is_paid);
        assert!(!r.is_expired);
        assert!(r.preimage.is_some());
        assert_eq!(r.received_sat, 950);
        assert_eq!(r.fees, 50); // 1000 requested - 950 received
        assert_eq!(r.completed_at, Some(1700001000));
    }

    #[test]
    fn enrich_pending() {
        let m = test_metadata();
        let d = pending_details();
        let r = enrich_metadata(&m, Some(&d));
        assert!(!r.is_paid);
        assert!(r.preimage.is_none());
        assert_eq!(r.received_sat, 0);
        assert_eq!(r.fees, 0);
        assert!(r.completed_at.is_none());
    }

    #[test]
    fn enrich_paid_never_expired() {
        // A paid invoice should never report is_expired, even if the
        // expiry timestamp is in the past.
        let mut m = test_metadata();
        m.expires_at = 1; // long in the past
        let d = paid_details();
        let r = enrich_metadata(&m, Some(&d));
        assert!(r.is_paid);
        assert!(!r.is_expired);
    }

    #[test]
    fn enrich_expired_unpaid() {
        let mut m = test_metadata();
        m.expires_at = 1; // long in the past
        let r = enrich_metadata(&m, None);
        assert!(!r.is_paid);
        assert!(r.is_expired);
    }

    #[test]
    fn enrich_zero_expires_at_not_expired() {
        let mut m = test_metadata();
        m.expires_at = 0;
        let r = enrich_metadata(&m, None);
        assert!(!r.is_expired);
        assert_eq!(r.expires_at, None);
    }

    fn test_req(description: Option<&str>, description_hash: Option<&str>) -> CreateInvoiceRequest {
        CreateInvoiceRequest {
            amount_sat: None,
            description: description.map(String::from),
            description_hash: description_hash.map(String::from),
            expiry_seconds: None,
            external_id: None,
            webhook_url: None,
            product: None,
            currency: None,
            success_url: None,
            metadata: None,
            customer_name: None,
            customer_email: None,
            customer_external_id: None,
        }
    }

    #[test]
    fn description_direct() {
        let req = test_req(Some("coffee"), None);
        assert!(matches!(
            parse_description(&req),
            Ok(Bolt11InvoiceDescription::Direct(_))
        ));
    }

    #[test]
    fn description_hash() {
        let hash = "ab".repeat(32);
        let req = test_req(None, Some(&hash));
        assert!(matches!(
            parse_description(&req),
            Ok(Bolt11InvoiceDescription::Hash(_))
        ));
    }

    #[test]
    fn description_too_long() {
        let long = "x".repeat(MAX_DESCRIPTION_LEN + 1);
        let req = test_req(Some(&long), None);
        assert!(matches!(
            parse_description(&req),
            Err(AppError::BadRequest(msg)) if msg.contains("too long")
        ));
    }

    #[test]
    fn description_hash_invalid_hex() {
        let req = test_req(None, Some("not_hex"));
        assert!(matches!(
            parse_description(&req),
            Err(AppError::BadRequest(msg)) if msg.contains("Invalid descriptionHash")
        ));
    }

    #[test]
    fn description_neither_provided() {
        let req = test_req(None, None);
        assert!(matches!(
            parse_description(&req),
            Err(AppError::BadRequest(msg)) if msg.contains("either")
        ));
    }

    #[test]
    fn description_both_provided() {
        let hash = "ab".repeat(32);
        let req = test_req(Some("coffee"), Some(&hash));
        assert!(matches!(
            parse_description(&req),
            Err(AppError::BadRequest(msg)) if msg.contains("either")
        ));
    }
}
