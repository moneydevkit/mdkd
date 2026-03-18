use serde::{Deserialize, Serialize};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkout_id: Option<String>,
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
#[serde(rename_all = "camelCase")]
pub struct WebhookPayload {
    pub event: String,
    pub payment_hash: String,
    pub amount_msat: u64,
    pub external_id: Option<String>,
    pub timestamp: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub error: String,
    pub code: String,
}
