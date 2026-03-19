use std::sync::Arc;

use axum::extract::Path;
use axum::Json;
use hex::FromHex;
use ldk_server::ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_server::ldk_node::lightning_invoice::{
    Bolt11Invoice, Bolt11InvoiceDescription, Description,
};
use ldk_server::ldk_node::payment::PaymentStatus;
use ldk_server::ldk_node::Node;
use log::info;

use crate::api::error::AppError;
use crate::mdk::client::MdkApiClient;
use crate::mdk::types::{CheckoutCustomer, CreateCheckoutRequest, RegisterInvoiceRequest};
use crate::store::invoice_metadata::{InvoiceMetadata, InvoiceMetadataStore};
use crate::types::{CreateInvoiceRequest, CreateInvoiceResponse, GetInvoiceResponse};

pub async fn handle_create_invoice(
    node: Arc<Node>,
    metadata_store: Arc<InvoiceMetadataStore>,
    mdk_client: Arc<MdkApiClient>,
    Json(req): Json<CreateInvoiceRequest>,
) -> Result<Json<CreateInvoiceResponse>, AppError> {
    let description = Bolt11InvoiceDescription::Direct(
        Description::new(req.description.clone())
            .map_err(|e| AppError::BadRequest(format!("Invalid description: {}", e)))?,
    );

    let (invoice, checkout_id) =
        create_with_checkout(&node, &mdk_client, &description, &req).await?;

    let payment_hash = invoice.payment_hash().to_string();
    let expires_at = invoice.expires_at().map(|d| d.as_secs()).unwrap_or(0);
    let invoice_str = invoice.to_string();

    let metadata = InvoiceMetadata {
        payment_hash: payment_hash.clone(),
        external_id: req.external_id.clone(),
        webhook_url: req.webhook_url,
        checkout_id: checkout_id.clone(),
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
        checkout_id,
    }))
}

async fn create_with_checkout(
    node: &Node,
    client: &MdkApiClient,
    description: &Bolt11InvoiceDescription,
    req: &CreateInvoiceRequest,
) -> Result<(Bolt11Invoice, String), AppError> {
    let products = req.product.as_ref().map(|p| vec![p.clone()]);

    let checkout_req = CreateCheckoutRequest {
        node_id: node.node_id().to_string(),
        amount: req.amount_msat.map(|m| m / 1000),
        currency: req.currency.clone().or_else(|| Some("SAT".into())),
        products,
        success_url: req.success_url.clone(),
        metadata: req.metadata.clone(),
        customer: req.customer.as_ref().map(|c| CheckoutCustomer {
            name: c.name.clone(),
            email: c.email.clone(),
            external_id: c.external_id.clone(),
        }),
    };

    let checkout = client
        .create_checkout(&checkout_req)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to create checkout: {e}")))?;

    info!(
        "Created checkout {} (status: {})",
        checkout.id, checkout.status
    );

    // Use the amount from the checkout (authoritative for product-based checkouts).
    let amount_msat = match checkout.invoice_amount_sats {
        Some(sats) => sats * 1000,
        None => req.amount_msat.ok_or_else(|| {
            AppError::BadRequest(
                "Checkout did not return an amount and no amount_msat provided".into(),
            )
        })?,
    };

    let invoice = create_invoice(node, amount_msat, description, req.expiry_secs)?;

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

    let _registered = client
        .register_invoice(&register_req)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to register invoice: {e}")))?;

    Ok((invoice, checkout.id))
}

fn create_invoice(
    node: &Node,
    amount_msat: u64,
    description: &Bolt11InvoiceDescription,
    expiry_secs: u32,
) -> Result<Bolt11Invoice, AppError> {
    node.bolt11_payment()
        .receive_via_lsps4_jit_channel(Some(amount_msat), description, expiry_secs)
        .map_err(|e| AppError::Internal(format!("Failed to create JIT invoice: {}", e)))
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
