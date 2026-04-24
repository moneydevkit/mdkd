use std::fmt;

use serde::{Deserialize, Serialize};

// --- Requests ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCheckoutRequest {
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub products: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customer: Option<CheckoutCustomer>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckoutCustomer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterInvoiceRequest {
    pub node_id: String,
    pub scid: String,
    pub checkout_id: String,
    pub invoice: String,
    pub payment_hash: String,
    pub invoice_expires_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentReceivedRequest {
    pub payments: Vec<PaymentEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentEntry {
    pub payment_hash: String,
    pub amount_sats: u64,
    pub sandbox: bool,
}

// --- Responses ---

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct Checkout {
    pub id: String,
    pub status: String,
    pub invoice_amount_sats: Option<u64>,
    pub invoice_scid: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct PaymentReceivedResponse {
    pub ok: bool,
}

// --- Errors ---

#[derive(Debug)]
pub enum MdkApiError {
    Network(reqwest::Error),
    Api {
        code: String,
        message: String,
        status: u16,
    },
    Deserialize(String),
}

impl fmt::Display for MdkApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MdkApiError::Network(e) => write!(f, "network error: {e}"),
            MdkApiError::Api {
                code,
                message,
                status,
            } => {
                write!(f, "API error {status} [{code}]: {message}")
            }
            MdkApiError::Deserialize(msg) => write!(f, "deserialize error: {msg}"),
        }
    }
}

impl From<reqwest::Error> for MdkApiError {
    fn from(e: reqwest::Error) -> Self {
        MdkApiError::Network(e)
    }
}
