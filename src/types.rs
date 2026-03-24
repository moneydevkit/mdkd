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
#[serde(rename_all = "camelCase")]
pub struct CreateInvoiceRequest {
    pub amount_msat: Option<u64>,
    pub description: String,
    pub expiry_secs: u32,
    pub external_id: Option<String>,
    pub webhook_url: Option<String>,
    pub product: Option<String>,
    pub currency: Option<String>,
    pub success_url: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub customer: Option<CheckoutCustomerInput>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutCustomerInput {
    pub name: Option<String>,
    pub email: Option<String>,
    pub external_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInvoiceResponse {
    pub invoice: String,
    pub payment_hash: String,
    pub external_id: Option<String>,
    pub expires_at: u64,
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
pub struct NodeInfoResponse {
    pub node_id: String,
    pub network: String,
    pub channels: Vec<ChannelInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfo {
    pub channel_id: String,
    pub counterparty_node_id: String,
    pub channel_value_sats: u64,
    pub outbound_capacity_msat: u64,
    pub inbound_capacity_msat: u64,
    pub is_usable: bool,
    pub is_channel_ready: bool,
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
