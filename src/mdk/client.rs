use std::str::FromStr;
use std::sync::Arc;

use bitcoin_payment_instructions::PaymentInstructions;
use chrono::{DateTime, SecondsFormat};
use ldk_node::bitcoin::hashes::sha256;
use ldk_node::bitcoin::hashes::Hash as _;
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description, Sha256};
use ldk_node::{Event, Node, NodeError, UserChannelId};
use log::{error, info, warn};
use reqwest::{Client, Proxy};
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::mdk::error::{MdkError, SpliceError};
use crate::mdk::max_sendable::{
    self, ChannelSnapshot, MaxSendableConfig, MaxSendableError, MaxSendableEstimate,
};
use crate::mdk::mdk_api::client::MdkApiClient;
use crate::mdk::mdk_api::types::{
    CheckoutCustomer, CreateCheckoutRequest, PaymentEntry, PaymentReceivedRequest,
    RegisterInvoiceRequest,
};
use crate::mdk::node::{build_node, NodeConfig, SpliceConfig};
use crate::mdk::splice_manager;
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
    lsp_pubkey: PublicKey,
    splice_cfg: SpliceConfig,
    max_sendable_cfg: MaxSendableConfig,
    event_tx: broadcast::Sender<MdkEvent>,
    event_handler: Option<EventHandler>,
    shutdown: CancellationToken,
    handle: Handle,
    /// Keeps the runtime alive when the library created it.
    /// None when the caller provided a handle.
    _runtime: Option<Arc<tokio::runtime::Runtime>>,
}

impl MdkClient {
    /// Build the LDK node, HTTP client, and platform API client from config.
    ///
    /// `runtime` — pass `Some(handle)` to reuse an existing tokio runtime
    /// (typical for Rust callers), or `None` to let the library create its own
    /// (typical for language bindings).
    ///
    /// Does not start the node or event loop — call `start()` for that.
    pub fn new(
        config: NodeConfig,
        access_token: String,
        event_handler: Option<EventHandler>,
        runtime: Option<Handle>,
    ) -> Result<Self, MdkError> {
        let (handle, owned_runtime) = match runtime {
            Some(h) => (h, None),
            None => {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| MdkError::Node(format!("failed to create tokio runtime: {e}")))?;
                let h = rt.handle().clone();
                (h, Some(Arc::new(rt)))
            }
        };

        let api_base_url = config.infra.mdk_api_base_url.clone();
        let socks_proxy = config.socks_proxy.clone();
        let lsp_pubkey = PublicKey::from_str(&config.infra.lsp_node_id)
            .map_err(|e| MdkError::InvalidInput(format!("bad lsp_node_id: {e}")))?;
        let splice_cfg = config.splice.clone();
        let max_sendable_cfg = config.max_sendable.clone();

        let node = build_node(config, handle.clone())?;
        let http_client = build_http_client(socks_proxy.as_deref())?;
        let api = Arc::new(MdkApiClient::new(
            http_client.clone(),
            api_base_url,
            access_token,
        ));

        let (event_tx, _) = broadcast::channel(256);
        Ok(Self {
            node,
            api,
            lsp_pubkey,
            splice_cfg,
            max_sendable_cfg,
            event_tx,
            event_handler,
            shutdown: CancellationToken::new(),
            handle,
            _runtime: owned_runtime,
        })
    }

    /// Start the LDK node and spawn the internal event loop.
    pub fn start(self: &Arc<Self>) -> Result<(), MdkError> {
        self.node.start()?;
        let this = Arc::clone(self);
        self.handle.spawn(async move {
            this.run_event_loop().await;
        });
        if self.splice_cfg.enabled {
            splice_manager::spawn(
                Arc::clone(self),
                self.splice_cfg.clone(),
                self.shutdown.clone(),
                &self.handle,
            );
        }
        Ok(())
    }

    /// Cancel the event loop and stop the LDK node.
    pub fn stop(&self) -> Result<(), MdkError> {
        self.shutdown.cancel();
        self.node.stop()?;
        Ok(())
    }

    pub fn node(&self) -> &Node {
        &self.node
    }

    pub fn node_arc(&self) -> Arc<Node> {
        Arc::clone(&self.node)
    }

    pub fn lsp_pubkey(&self) -> PublicKey {
        self.lsp_pubkey
    }

    /// Best-effort estimate of the largest amount that can flow out
    /// over Lightning right now, with routing-fee headroom subtracted.
    /// Recomputed from `node.list_channels()` on every call so the
    /// result reflects in-flight HTLCs and reserve as of *now*.
    ///
    /// `dest = None` returns a buffer-based estimate; `Some(_)`
    /// drives `Node::find_route` and subtracts the real fees. See
    /// [`crate::mdk::max_sendable`] for the full dispatch table.
    pub fn max_sendable(
        &self,
        dest: Option<&PaymentInstructions>,
    ) -> Result<MaxSendableEstimate, MaxSendableError> {
        let snaps: Vec<ChannelSnapshot> = self
            .node
            .list_channels()
            .iter()
            .map(ChannelSnapshot::from)
            .collect();
        max_sendable::compute_estimate(
            dest,
            &snaps,
            &self.lsp_pubkey,
            &self.max_sendable_cfg,
            |rp| self.node.find_route(rp).map_err(|e| format!("{e}")),
        )
    }

    /// Splice `amount_sats` of confirmed on-chain funds into the
    /// existing channel identified by `user_channel_id`, with the
    /// LSP as counterparty.
    ///
    /// Validates locally that the channel exists and is usable
    /// before delegating to ldk-node. ldk-node's splice errors are
    /// mapped to typed `SpliceError` variants so callers (notably
    /// the splice manager) can pattern-match on the failure mode
    /// without inspecting strings.
    pub fn splice_in(
        &self,
        user_channel_id: UserChannelId,
        amount_sats: u64,
    ) -> Result<(), MdkError> {
        if amount_sats == 0 {
            return Err(MdkError::InvalidInput(
                "splice amount must be greater than zero".into(),
            ));
        }

        let channels = self.node.list_channels();
        let channel = channels
            .iter()
            .find(|c| c.user_channel_id == user_channel_id)
            .ok_or_else(|| {
                MdkError::NotFound(format!(
                    "channel with user_channel_id {}",
                    user_channel_id.0
                ))
            })?;

        if !channel.is_usable {
            return Err(MdkError::Splice(SpliceError::ChannelNotUsable));
        }

        self.node
            .splice_in(&user_channel_id, self.lsp_pubkey, amount_sats)
            .map_err(map_splice_error)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MdkEvent> {
        self.event_tx.subscribe()
    }

    /// Fan an event out to the configured handler and broadcast
    /// subscribers. Used by the LDK event loop and the splice manager
    /// to surface internally-generated events.
    pub fn emit_event(&self, ev: MdkEvent) {
        if let Some(handler) = &self.event_handler {
            handler(ev.clone());
        }
        let _ = self.event_tx.send(ev);
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
                        self.emit_event(ev);
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
            Event::SplicePending {
                channel_id,
                new_funding_txo,
                ..
            } => {
                let cid = channel_id.to_string();
                let txid = new_funding_txo.txid.to_string();
                info!("SPLICE_PENDING: channel {cid}, new funding tx {txid}");
                Some(MdkEvent::SplicePending {
                    channel_id: cid,
                    new_funding_txid: txid,
                })
            }
            Event::SpliceFailed {
                channel_id,
                abandoned_funding_txo,
                ..
            } => {
                let cid = channel_id.to_string();
                let reason = match abandoned_funding_txo {
                    Some(txo) => format!("abandoned splice tx {}", txo.txid),
                    None => "splice abandoned before tx broadcast".to_string(),
                };
                warn!("SPLICE_FAILED: channel {cid}, {reason}");
                Some(MdkEvent::SpliceFailed {
                    channel_id: cid,
                    reason,
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

fn build_http_client(socks_proxy: Option<&str>) -> Result<Client, MdkError> {
    let mut builder = Client::builder();
    if let Some(proxy_url) = socks_proxy {
        let proxy = Proxy::all(proxy_url)
            .map_err(|e| MdkError::InvalidInput(format!("invalid SOCKS5 proxy for HTTP: {e}")))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|e| MdkError::Network(format!("failed to build HTTP client: {e}")))
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

/// Map an ldk-node error returned from a splice call into a typed
/// `MdkError::Splice`. Kept as a free helper (rather than a
/// `From<NodeError>` impl) because `NodeError::InsufficientFunds`
/// is also produced by `open_channel` and on-chain wallet paths,
/// where mapping it to a splice variant would be wrong. Anything
/// other than `InsufficientFunds` collapses to `Rejected` — the
/// splice manager treats the catch-all bucket uniformly.
fn map_splice_error(e: NodeError) -> MdkError {
    match e {
        NodeError::InsufficientFunds => MdkError::Splice(SpliceError::InsufficientFunds),
        _ => MdkError::Splice(SpliceError::Rejected),
    }
}
