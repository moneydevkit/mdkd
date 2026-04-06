use std::sync::Arc;

use chrono::{DateTime, SecondsFormat};
use ldk_node::bitcoin::hashes::sha256;
use ldk_node::bitcoin::hashes::Hash as _;
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description, Sha256};
use ldk_node::{Event, Node};
use log::{error, info};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::mdk::error::MdkError;
use crate::mdk::mdk_api::client::MdkApiClient;
use crate::mdk::mdk_api::types::{
    CheckoutCustomer, CreateCheckoutRequest, PaymentEntry, PaymentReceivedRequest,
    RegisterInvoiceRequest,
};
use crate::mdk::types::{CheckoutResult, CreateCheckoutParams, InvoiceDescription, MdkEvent};

const DEFAULT_EXPIRY_SECS: u32 = 3600;
const MAX_DESCRIPTION_LEN: usize = 128;

/// Callback invoked for each translated MdkEvent.
/// Fires before the broadcast channel send, so handlers see events
/// even when no broadcast subscriber exists.
pub type EventHandler = Arc<dyn Fn(MdkEvent) + Send + Sync>;

pub struct MdkClient {
    node: Arc<Node>,
    api: Arc<MdkApiClient>,
    event_tx: broadcast::Sender<MdkEvent>,
    event_handler: Option<EventHandler>,
    shutdown: CancellationToken,
}

impl MdkClient {
    pub fn new(
        node: Arc<Node>,
        api: Arc<MdkApiClient>,
        event_handler: Option<EventHandler>,
    ) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            node,
            api,
            event_tx,
            event_handler,
            shutdown: CancellationToken::new(),
        }
    }

    /// Spawn the internal event loop. Call once after construction.
    /// The loop translates LDK events into MdkEvents, notifies the
    /// platform on payment receipt, invokes the event handler callback,
    /// and broadcasts to all subscribers.
    pub fn start(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run_event_loop().await;
        });
    }

    /// Cancel the event loop. Idempotent.
    pub fn stop(&self) {
        self.shutdown.cancel();
    }

    pub fn node(&self) -> &Node {
        &self.node
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MdkEvent> {
        self.event_tx.subscribe()
    }

    async fn run_event_loop(&self) {
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                event = self.node.next_event_async() => {
                    let mdk_event = self.handle_ldk_event(&event).await;

                    if let Err(e) = self.node.event_handled() {
                        error!("Failed to mark event as handled: {e}");
                    }

                    if let Some(ev) = mdk_event {
                        if let Some(handler) = &self.event_handler {
                            handler(ev.clone());
                        }
                        let _ = self.event_tx.send(ev);
                    }
                }
            }
        }
        info!("Event loop stopped");
    }

    async fn handle_ldk_event(&self, event: &Event) -> Option<MdkEvent> {
        match event {
            Event::PaymentReceived {
                payment_hash,
                amount_msat,
                ..
            } => {
                let hash = payment_hash.to_string();
                let amount_sats = amount_msat / 1000;
                info!("PAYMENT_RECEIVED: hash {hash}, amount_sats {amount_sats}");

                self.notify_payment_received(&hash, amount_sats).await;

                Some(MdkEvent::PaymentReceived {
                    payment_hash: hash,
                    amount_sats,
                })
            }
            Event::PaymentSuccessful {
                payment_id,
                payment_hash,
                fee_paid_msat,
                ..
            } => {
                info!("PAYMENT_SUCCESSFUL: hash {payment_hash}");
                Some(MdkEvent::PaymentSuccessful {
                    payment_id: format_payment_id(payment_id),
                    payment_hash: Some(payment_hash.to_string()),
                    fee_paid_sats: fee_paid_msat.map(|m| m / 1000),
                })
            }
            Event::PaymentFailed {
                payment_id,
                payment_hash,
                reason,
                ..
            } => {
                info!("PAYMENT_FAILED: hash {payment_hash:?}, reason: {reason:?}");
                Some(MdkEvent::PaymentFailed {
                    payment_id: format_payment_id(payment_id),
                    reason: reason.map(|r| format!("{r:?}")),
                })
            }
            Event::ChannelPending {
                channel_id,
                counterparty_node_id,
                ..
            } => {
                info!("CHANNEL_PENDING: {channel_id} from {counterparty_node_id}");
                Some(MdkEvent::ChannelPending {
                    channel_id: channel_id.to_string(),
                    counterparty_node_id: counterparty_node_id.to_string(),
                })
            }
            Event::ChannelReady {
                channel_id,
                counterparty_node_id,
                ..
            } => {
                info!("CHANNEL_READY: {channel_id} from {counterparty_node_id:?}");
                Some(MdkEvent::ChannelReady {
                    channel_id: channel_id.to_string(),
                    counterparty_node_id: counterparty_node_id
                        .map(|pk| pk.to_string())
                        .unwrap_or_default(),
                })
            }
            Event::PaymentForwarded {
                total_fee_earned_msat,
                outbound_amount_forwarded_msat,
                prev_channel_id,
                next_channel_id,
                ..
            } => {
                info!(
                    "PAYMENT_FORWARDED: outbound_msat {}, fee_msat {}, in: {prev_channel_id}, out: {next_channel_id}",
                    outbound_amount_forwarded_msat.unwrap_or(0),
                    total_fee_earned_msat.unwrap_or(0),
                );
                Some(MdkEvent::PaymentForwarded {
                    fee_earned_sats: total_fee_earned_msat.map(|m| m / 1000),
                })
            }
            _ => None,
        }
    }

    async fn notify_payment_received(&self, payment_hash: &str, amount_sats: u64) {
        let req = PaymentReceivedRequest {
            payments: vec![PaymentEntry {
                payment_hash: payment_hash.to_string(),
                amount_sats,
                sandbox: false,
            }],
        };
        match self.api.payment_received(&req).await {
            Ok(_) => {
                info!("Notified moneydevkit.com of payment {payment_hash} ({amount_sats} sats)")
            }
            Err(e) => error!("Failed to notify moneydevkit.com for payment {payment_hash}: {e}"),
        }
    }

    pub async fn create_checkout(
        &self,
        params: CreateCheckoutParams,
    ) -> Result<CheckoutResult, MdkError> {
        let description = to_bolt11_description(&params.description)?;
        let expiry_secs = params.expiry_seconds.unwrap_or(DEFAULT_EXPIRY_SECS);

        let customer = params.customer.map(|c| CheckoutCustomer {
            name: c.name,
            email: c.email,
            external_id: c.external_id,
        });

        let checkout_req = CreateCheckoutRequest {
            node_id: self.node.node_id().to_string(),
            amount: params.amount_sat,
            currency: params.currency.or_else(|| Some("SAT".into())),
            products: params.product.map(|p| vec![p]),
            success_url: params.success_url,
            metadata: params.metadata,
            customer,
        };

        let checkout = self.api.create_checkout(&checkout_req).await.map_err(|e| {
            error!("MDK checkout/create failed: {e}");
            MdkError::from(e)
        })?;

        info!(
            "Created checkout {} (status: {})",
            checkout.id, checkout.status
        );

        let amount_msat = match checkout.invoice_amount_sats {
            Some(sats) => Some(sats * 1000),
            None => params.amount_sat.map(|s| s * 1000),
        };

        let invoice = self
            .node
            .bolt11_payment()
            .receive_via_lsps4_jit_channel(amount_msat, &description, expiry_secs)
            .map_err(|e| MdkError::Node(format!("failed to create JIT invoice: {e}")))?;

        let scid = extract_scid(&invoice);
        let payment_hash = invoice.payment_hash().to_string();
        let expires_at = invoice.expires_at().map(|d| d.as_secs());
        let expires_at_iso = expires_at
            .and_then(|secs| {
                DateTime::from_timestamp(secs as i64, 0)
                    .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
            })
            .unwrap_or_default();

        let register_req = RegisterInvoiceRequest {
            node_id: self.node.node_id().to_string(),
            scid,
            checkout_id: checkout.id.clone(),
            invoice: invoice.to_string(),
            payment_hash: payment_hash.clone(),
            invoice_expires_at: expires_at_iso,
        };

        self.api
            .register_invoice(&register_req)
            .await
            .map_err(|e| {
                error!("MDK checkout/registerInvoice failed: {e}");
                MdkError::from(e)
            })?;

        let amount_sat = invoice.amount_milli_satoshis().map(|m| m / 1000);

        Ok(CheckoutResult {
            checkout_id: checkout.id,
            invoice: invoice.to_string(),
            payment_hash,
            amount_sat,
            expires_at,
        })
    }
}

fn to_bolt11_description(desc: &InvoiceDescription) -> Result<Bolt11InvoiceDescription, MdkError> {
    match desc {
        InvoiceDescription::Direct(text) => {
            if text.len() > MAX_DESCRIPTION_LEN {
                return Err(MdkError::InvalidInput(format!(
                    "description too long (max {MAX_DESCRIPTION_LEN} characters)"
                )));
            }
            let d = Description::new(text.clone())
                .map_err(|e| MdkError::InvalidInput(format!("invalid description: {e}")))?;
            Ok(Bolt11InvoiceDescription::Direct(d))
        }
        InvoiceDescription::Hash(bytes) => Ok(Bolt11InvoiceDescription::Hash(Sha256(
            sha256::Hash::from_byte_array(*bytes),
        ))),
    }
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

fn format_payment_id(id: &Option<PaymentId>) -> String {
    match id {
        Some(pid) => pid.0.iter().map(|b| format!("{b:02x}")).collect(),
        None => "unknown".into(),
    }
}
