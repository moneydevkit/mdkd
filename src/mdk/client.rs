use std::sync::Arc;

use chrono::{DateTime, SecondsFormat};
use ldk_node::bitcoin::hashes::sha256;
use ldk_node::bitcoin::hashes::Hash as _;
use ldk_node::lightning_invoice::{Bolt11InvoiceDescription, Description, Sha256};
use ldk_node::Node;
use log::{error, info};

use crate::mdk::error::MdkError;
use crate::mdk::mdk_api::client::MdkApiClient;
use crate::mdk::mdk_api::types::{CheckoutCustomer, CreateCheckoutRequest, RegisterInvoiceRequest};
use crate::mdk::types::{CheckoutResult, CreateCheckoutParams, InvoiceDescription};

const DEFAULT_EXPIRY_SECS: u32 = 3600;
const MAX_DESCRIPTION_LEN: usize = 128;

pub struct MdkClient {
    node: Arc<Node>,
    api: Arc<MdkApiClient>,
}

impl MdkClient {
    pub fn new(node: Arc<Node>, api: Arc<MdkApiClient>) -> Self {
        Self { node, api }
    }

    pub fn node(&self) -> &Node {
        &self.node
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

fn extract_scid(invoice: &ldk_node::lightning_invoice::Bolt11Invoice) -> String {
    invoice
        .route_hints()
        .iter()
        .flat_map(|hint| &hint.0)
        .next()
        .map(|hop| hop.short_channel_id.to_string())
        .unwrap_or_default()
}
