pub mod error;
pub mod invoices;
pub mod node;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
use ldk_server::ldk_node::Node;

use crate::mdk::client::MdkApiClient;
use crate::store::invoice_metadata::InvoiceMetadataStore;

#[derive(Clone)]
pub struct AppState {
    pub node: Arc<Node>,
    pub metadata_store: Arc<InvoiceMetadataStore>,
    pub api_key: String,
    pub mdk_client: Arc<MdkApiClient>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/invoices", post(create_invoice))
        .route("/v1/invoices/{payment_hash}", get(get_invoice))
        .route("/v1/node", get(get_node))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
}

async fn auth_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let token = auth_header.strip_prefix("Bearer ").unwrap_or("");
    if token != state.api_key {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(req).await)
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
