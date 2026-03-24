pub mod auth;
pub mod balance;
pub mod decode;
pub mod error;
pub mod invoices;
pub mod node;

use std::sync::Arc;

use axum::extract::State;
use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use ldk_server::ldk_node::Node;

pub use auth::HttpAuth;

use crate::mdk::client::MdkApiClient;
use crate::store::invoice_metadata::InvoiceMetadataStore;

#[derive(Clone)]
pub struct AppState {
    pub node: Arc<Node>,
    pub metadata_store: Arc<InvoiceMetadataStore>,
    pub http_auth: HttpAuth,
    pub mdk_client: Arc<MdkApiClient>,
}

pub fn router(state: AppState) -> Router {
    let read_only_routes = Router::new()
        .route("/v1/node", get(get_node))
        .route("/v1/invoices/{payment_hash}", get(get_invoice))
        .route("/getbalance", get(get_balance))
        .route("/decodeinvoice", post(decode_invoice));

    let full_routes = Router::new()
        .route("/v1/invoices", post(create_invoice))
        .layer(middleware::from_fn(auth::require_full_access));

    read_only_routes
        .merge(full_routes)
        .layer(middleware::from_fn_with_state(
            state.http_auth.clone(),
            auth::auth_middleware,
        ))
        .with_state(state)
}

async fn create_invoice(
    State(state): State<AppState>,
    body: axum::Json<crate::types::CreateInvoiceRequest>,
) -> Result<axum::Json<crate::types::CreateInvoiceResponse>, error::AppError> {
    invoices::handle_create_invoice(state.node, state.metadata_store, state.mdk_client, body).await
}

async fn get_invoice(
    State(state): State<AppState>,
    path: axum::extract::Path<String>,
) -> Result<axum::Json<crate::types::GetInvoiceResponse>, error::AppError> {
    invoices::handle_get_invoice(state.node, state.metadata_store, path).await
}

async fn get_node(
    State(state): State<AppState>,
) -> Result<axum::Json<crate::types::NodeInfoResponse>, error::AppError> {
    node::handle_get_node(state.node).await
}

async fn get_balance(
    State(state): State<AppState>,
) -> Result<axum::Json<crate::types::GetBalanceResponse>, error::AppError> {
    balance::handle_get_balance(state.node).await
}

async fn decode_invoice(
    axum::Form(req): axum::Form<crate::types::DecodeInvoiceRequest>,
) -> Result<axum::Json<crate::types::DecodeInvoiceResponse>, error::AppError> {
    decode::handle_decode_invoice(req)
}
