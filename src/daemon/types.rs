use ldk_node::ChannelDetails;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Deserialize, ToSchema)]
pub struct DecodeInvoiceRequest {
    pub invoice: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecodeInvoiceResponse {
    pub amount: Option<u64>,
    pub amount_msat: Option<u64>,
    pub payment_hash: String,
    pub payment_secret: String,
    pub description: Option<String>,
    pub payment_metadata: Option<String>,
    pub expiry_seconds: u64,
    pub created_at_seconds: u64,
    pub node_id: String,
    pub routing_hints: Vec<RoutingHint>,
    pub features: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutingHint {
    pub hops: Vec<RoutingHintHop>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoutingHintHop {
    pub node_id: String,
    pub short_channel_id: String,
    pub fee_base_msat: u32,
    pub fee_proportional_millionths: u32,
    pub cltv_expiry_delta: u16,
}

#[derive(Deserialize, ToSchema)]
pub struct DecodeOfferRequest {
    pub offer: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecodeOfferResponse {
    pub offer_id: String,
    pub amount: Option<u64>,
    pub amount_msat: Option<u64>,
    pub description: Option<String>,
    pub issuer: Option<String>,
    pub node_id: Option<String>,
    pub features: Vec<String>,
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateInvoiceRequest {
    pub amount_sat: Option<u64>,
    pub description: Option<String>,
    pub description_hash: Option<String>,
    pub expiry_seconds: Option<u32>,
    pub external_id: Option<String>,
    pub webhook_url: Option<String>,
    // mdk-platform extensions
    pub product: Option<String>,
    pub currency: Option<String>,
    pub success_url: Option<String>,
    pub metadata: Option<String>,
    pub customer_name: Option<String>,
    pub customer_email: Option<String>,
    pub customer_external_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateInvoiceResponse {
    pub amount_sat: Option<u64>,
    pub payment_hash: String,
    pub serialized: String,
    // mdk-platform extensions
    pub checkout_id: String,
}

#[derive(Deserialize, ToSchema, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ListPaymentsRequest {
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    pub all: Option<bool>,
    pub external_id: Option<String>,
}

/// All timestamps are unix epoch **seconds** — BOLT11 expiry and LDK's
/// `latest_update_timestamp` are seconds-precision.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IncomingPaymentResponse {
    pub payment_hash: String,
    pub preimage: Option<String>,
    pub external_id: Option<String>,
    pub description: Option<String>,
    pub invoice: Option<String>,
    pub is_paid: bool,
    pub is_expired: bool,
    pub requested_sat: Option<u64>,
    pub received_sat: u64,
    pub fees: u64,
    pub completed_at: Option<u64>,
    pub created_at: u64,
    pub expires_at: Option<u64>,
}

#[derive(Deserialize, ToSchema, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ListOutgoingPaymentsRequest {
    pub from: Option<u64>,
    pub to: Option<u64>,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
    /// If true, include failed payments. Otherwise only successful + pending.
    pub all: Option<bool>,
}

/// Matches phoenixd's outgoing payment response shape.
/// Timestamps are unix epoch seconds. Fees are in satoshis.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OutgoingPaymentResponse {
    pub payment_id: String,
    pub payment_hash: Option<String>,
    pub preimage: Option<String>,
    pub tx_id: Option<String>,
    pub is_paid: bool,
    pub sent: Option<u64>,
    pub fees: Option<u64>,
    pub invoice: Option<String>,
    pub completed_at: Option<u64>,
    pub created_at: u64,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetBalanceResponse {
    pub balance_sat: u64,
    /// Spendable on-chain balance in sats.
    pub onchain_balance_sat: u64,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetInfoResponse {
    pub node_id: String,
    pub channels: Vec<ChannelInfo>,
    pub chain: String,
    pub block_height: u32,
    pub version: &'static str,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfo {
    pub state: ChannelState,
    pub channel_id: String,
    pub balance_sat: u64,
    pub inbound_liquidity_sat: u64,
    pub capacity_sat: u64,
    pub funding_tx_id: Option<String>,
}

impl From<&ChannelDetails> for ChannelInfo {
    fn from(ch: &ChannelDetails) -> Self {
        let state = match (ch.is_channel_ready, ch.is_usable) {
            (true, true) => ChannelState::Online,
            (true, false) => ChannelState::Offline,
            (false, _) => ChannelState::Opening,
        };

        Self {
            state,
            channel_id: ch.channel_id.to_string(),
            balance_sat: ch.outbound_capacity_msat / 1000,
            inbound_liquidity_sat: ch.inbound_capacity_msat / 1000,
            capacity_sat: ch.channel_value_sats,
            funding_tx_id: ch.funding_txo.map(|txo| txo.txid.to_string()),
        }
    }
}

/// LDK removes closing/closed channels from `list_channels()`, so those
/// states never appear here. `Offline` means the channel exists but the
/// peer is disconnected (not closing).
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ChannelState {
    Online,
    Offline,
    Opening,
}

#[derive(Serialize)]
#[serde(tag = "event")]
pub enum WebhookEvent {
    #[serde(rename = "payment_received", rename_all = "camelCase")]
    PaymentReceived {
        payment_hash: String,
        amount_msat: u64,
        external_id: Option<String>,
        timestamp: u64,
    },
    #[serde(rename = "invoice_expired", rename_all = "camelCase")]
    InvoiceExpired {
        payment_hash: String,
        external_id: Option<String>,
        timestamp: u64,
    },
}

impl WebhookEvent {
    pub fn timestamp(&self) -> u64 {
        match self {
            WebhookEvent::PaymentReceived { timestamp, .. } => *timestamp,
            WebhookEvent::InvoiceExpired { timestamp, .. } => *timestamp,
        }
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SendToAddressRequest {
    pub address: String,
    pub amount_sat: u64,
    pub feerate_sat_byte: Option<u64>,
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloseChannelRequest {
    pub channel_id: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayInvoiceRequest {
    pub invoice: String,
    pub amount_sat: Option<u64>,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayInvoiceResponse {
    pub payment_id: String,
    pub payment_hash: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub error: String,
    pub code: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayRequest {
    pub destination: String,
    pub amount_sat: Option<u64>,
    pub wait_for_payment_secs: Option<u64>,
    pub payer_note: Option<String>,
    pub quantity: Option<u64>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PayStatus {
    Succeeded,
    Failed,
    Pending,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayResponse {
    pub payment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preimage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_sat: Option<u64>,
    pub status: PayStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
