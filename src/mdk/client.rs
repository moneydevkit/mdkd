use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::types::*;

#[derive(Clone)]
pub struct MdkApiClient {
    http: reqwest::Client,
    base_url: String,
    access_token: String,
}

/// oRPC request envelope: `{ "json": <input>, "meta": [...] }`
///
/// The `meta` array tells the oRPC server which fields need special
/// deserialization. Date fields use type marker `1`:
///   `[[1, "fieldName"]]`
#[derive(Serialize)]
struct OrpcRequest<T: Serialize> {
    json: T,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    meta: Vec<(u8, &'static str)>,
}

/// oRPC response envelope: `{ "json": <output> }`
#[derive(Deserialize)]
struct OrpcResponse<T> {
    json: T,
}

impl MdkApiClient {
    pub fn new(base_url: String, access_token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            access_token,
        }
    }

    pub async fn create_checkout(
        &self,
        req: &CreateCheckoutRequest,
    ) -> Result<Checkout, MdkApiError> {
        self.post("checkout/create", req, vec![]).await
    }

    pub async fn register_invoice(
        &self,
        req: &RegisterInvoiceRequest,
    ) -> Result<Checkout, MdkApiError> {
        // invoiceExpiresAt is z.date() — tell oRPC to parse it as a Date.
        let meta = vec![(1, "invoiceExpiresAt")];
        self.post("checkout/registerInvoice", req, meta).await
    }

    pub async fn payment_received(
        &self,
        req: &PaymentReceivedRequest,
    ) -> Result<PaymentReceivedResponse, MdkApiError> {
        self.post("checkout/paymentReceived", req, vec![]).await
    }

    async fn post<Req: Serialize, Resp: DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
        meta: Vec<(u8, &'static str)>,
    ) -> Result<Resp, MdkApiError> {
        let url = format!("{}/{}", self.base_url, path);
        let envelope = OrpcRequest { json: body, meta };
        let response = self
            .http
            .post(&url)
            .header("x-api-key", &self.access_token)
            .json(&envelope)
            .send()
            .await?;

        let status = response.status();
        let bytes = response.bytes().await?;

        if status.is_success() {
            let resp: OrpcResponse<Resp> = serde_json::from_slice(&bytes).map_err(|e| {
                MdkApiError::Deserialize(format!("{e} (body: {})", String::from_utf8_lossy(&bytes)))
            })?;
            Ok(resp.json)
        } else {
            // oRPC errors also use the { "json": { ... } } envelope.
            match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(val) => {
                    let err = val.get("json").unwrap_or(&val);
                    let code = err
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("UNKNOWN")
                        .to_string();
                    let message = err
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown error")
                        .to_string();
                    Err(MdkApiError::Api {
                        code,
                        message,
                        status: status.as_u16(),
                    })
                }
                Err(_) => Err(MdkApiError::Api {
                    code: "UNKNOWN".into(),
                    message: String::from_utf8_lossy(&bytes).into_owned(),
                    status: status.as_u16(),
                }),
            }
        }
    }
}
