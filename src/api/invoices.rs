use std::sync::Arc;

use axum::extract::Path;
use axum::Json;
use hex::FromHex;
use ldk_server::ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_server::ldk_node::lightning_invoice::{Bolt11InvoiceDescription, Description};
use ldk_server::ldk_node::payment::PaymentStatus;
use ldk_server::ldk_node::Node;

use crate::api::error::AppError;
use crate::store::invoice_metadata::{InvoiceMetadata, InvoiceMetadataStore};
use crate::types::{CreateInvoiceRequest, CreateInvoiceResponse, GetInvoiceResponse};

pub async fn handle_create_invoice(
    node: Arc<Node>,
    metadata_store: Arc<InvoiceMetadataStore>,
    Json(req): Json<CreateInvoiceRequest>,
) -> Result<Json<CreateInvoiceResponse>, AppError> {
    let description = Bolt11InvoiceDescription::Direct(
        Description::new(req.description)
            .map_err(|e| AppError::BadRequest(format!("Invalid description: {}", e)))?,
    );

    let has_inbound = node
        .list_channels()
        .iter()
        .any(|c| c.is_usable && c.inbound_capacity_msat >= req.amount_msat);

    let invoice = if has_inbound {
        node.bolt11_payment()
            .receive(req.amount_msat, &description, req.expiry_secs)
            .map_err(|e| AppError::Internal(format!("Failed to create invoice: {}", e)))?
    } else {
        node.bolt11_payment()
            .receive_via_lsps4_jit_channel(Some(req.amount_msat), &description, req.expiry_secs)
            .map_err(|e| AppError::Internal(format!("Failed to create JIT invoice: {}", e)))?
    };

    let payment_hash = invoice.payment_hash().to_string();
    let expires_at = invoice.expires_at().map(|d| d.as_secs()).unwrap_or(0);
    let invoice_str = invoice.to_string();

    let metadata = InvoiceMetadata {
        payment_hash: payment_hash.clone(),
        external_id: req.external_id.clone(),
        webhook_url: req.webhook_url,
        created_at: InvoiceMetadataStore::now(),
    };

    metadata_store
        .insert(&metadata)
        .map_err(|e| AppError::Internal(format!("Failed to store invoice metadata: {}", e)))?;

    Ok(Json(CreateInvoiceResponse {
        invoice: invoice_str,
        payment_hash,
        external_id: req.external_id,
        expires_at,
    }))
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

    let (amount_msat, status) = match node.payment(&payment_id) {
        Some(details) => {
            let status_str = match details.status {
                PaymentStatus::Pending => "pending",
                PaymentStatus::Succeeded => "received",
                PaymentStatus::Failed => "failed",
            };
            (details.amount_msat, status_str.to_string())
        }
        None => (None, "pending".to_string()),
    };

    Ok(Json(GetInvoiceResponse {
        payment_hash,
        amount_msat,
        status,
        external_id: metadata.external_id,
    }))
}
