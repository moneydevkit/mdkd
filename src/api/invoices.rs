use std::sync::Arc;

use axum::extract::Path;
use axum::Json;
use hex::FromHex;
use ldk_server::ldk_node::bitcoin::hashes::sha256;
use ldk_server::ldk_node::bitcoin::hashes::Hash as _;
use ldk_server::ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_server::ldk_node::lightning_invoice::{
    Bolt11Invoice, Bolt11InvoiceDescription, Description, Sha256,
};
use ldk_server::ldk_node::payment::PaymentStatus;
use ldk_server::ldk_node::Node;
use log::{error, info};

use crate::api::error::AppError;
use crate::mdk::client::MdkApiClient;
use crate::mdk::types::{CheckoutCustomer, CreateCheckoutRequest, RegisterInvoiceRequest};
use crate::store::invoice_metadata::{InvoiceMetadata, InvoiceMetadataStore};
use crate::types::{CreateInvoiceRequest, CreateInvoiceResponse, GetInvoiceResponse};

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

    let metadata = InvoiceMetadata {
        payment_hash: payment_hash.clone(),
        external_id: req.external_id.clone(),
        webhook_url: req.webhook_url.clone(),
        checkout_id,
        created_at: InvoiceMetadataStore::now(),
        expires_at: expires_at as i64,
    };

    metadata_store
        .insert(&metadata)
        .map_err(|e| AppError::Internal(format!("Failed to store invoice metadata: {e}")))?;

    Ok(Json(CreateInvoiceResponse {
        amount_sat,
        payment_hash,
        serialized: invoice.to_string(),
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
        .map(|d| {
            let dt = time::OffsetDateTime::from_unix_timestamp(d.as_secs() as i64)
                .expect("valid timestamp");
            dt.format(&time::format_description::well_known::Rfc3339)
                .expect("valid rfc3339")
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

pub async fn handle_get_invoice(
    node: Arc<Node>,
    metadata_store: Arc<InvoiceMetadataStore>,
    Path(payment_hash): Path<String>,
) -> Result<Json<GetInvoiceResponse>, AppError> {
    let metadata = metadata_store
        .get_by_payment_hash(&payment_hash)
        .map_err(|e| AppError::Internal(format!("Failed to query metadata: {}", e)))?
        .ok_or_else(|| AppError::NotFound(format!("Invoice {} not found", payment_hash)))?;

    // For Bolt11 inbound payments, PaymentId is the payment_hash bytes.
    let hash_bytes = <[u8; 32]>::from_hex(&payment_hash)
        .map_err(|_| AppError::BadRequest("Invalid payment hash hex".into()))?;
    let payment_id = PaymentId(hash_bytes);

    let now = InvoiceMetadataStore::now();
    let is_expired = metadata.expires_at > 0 && metadata.expires_at <= now;

    let (amount_msat, status) = match node.payment(&payment_id) {
        Some(details) => {
            let status_str = match details.status {
                PaymentStatus::Pending if is_expired => "expired",
                PaymentStatus::Pending => "pending",
                PaymentStatus::Succeeded => "received",
                PaymentStatus::Failed => "failed",
            };
            (details.amount_msat, status_str.to_string())
        }
        None if is_expired => (None, "expired".to_string()),
        None => (None, "pending".to_string()),
    };

    Ok(Json(GetInvoiceResponse {
        payment_hash,
        amount_msat,
        status,
        external_id: metadata.external_id,
    }))
}
