use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct DecodeInvoiceRequest {
    pub invoice: String,
}

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingHint {
    pub hops: Vec<RoutingHintHop>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingHintHop {
    pub node_id: String,
    pub short_channel_id: String,
    pub fee_base_msat: u32,
    pub fee_proportional_millionths: u32,
    pub cltv_expiry_delta: u16,
}

#[derive(Deserialize)]
pub struct DecodeOfferRequest {
    pub offer: String,
}

#[derive(Debug, Serialize)]
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

#[derive(Deserialize)]
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInvoiceResponse {
    pub amount_sat: Option<u64>,
    pub payment_hash: String,
    pub serialized: String,
    // mdk-platform extensions
    pub checkout_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetInvoiceResponse {
    pub payment_hash: String,
    pub amount_msat: Option<u64>,
    pub status: String,
    pub external_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetBalanceResponse {
    pub balance_sat: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetInfoResponse {
    pub node_id: String,
    pub channels: Vec<ChannelInfo>,
    pub chain: String,
    pub block_height: u32,
    pub version: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfo {
    pub state: ChannelState,
    pub channel_id: String,
    pub balance_sat: u64,
    pub inbound_liquidity_sat: u64,
    pub capacity_sat: u64,
    pub funding_tx_id: Option<String>,
}

/// LDK removes closing/closed channels from `list_channels()`, so those
/// states never appear here. `Offline` means the channel exists but the
/// peer is disconnected (not closing).
#[derive(Serialize)]
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
        timestamp: i64,
    },
    #[serde(rename = "invoice_expired", rename_all = "camelCase")]
    InvoiceExpired {
        payment_hash: String,
        external_id: Option<String>,
        timestamp: i64,
    },
}

impl WebhookEvent {
    pub fn timestamp(&self) -> i64 {
        match self {
            WebhookEvent::PaymentReceived { timestamp, .. } => *timestamp,
            WebhookEvent::InvoiceExpired { timestamp, .. } => *timestamp,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub error: String,
    pub code: String,
}
