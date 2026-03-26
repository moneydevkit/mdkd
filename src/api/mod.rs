pub mod auth;
pub mod balance;
pub mod channels;
pub mod decode;
pub mod error;
pub mod info;
pub mod invoices;

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
        .route("/getinfo", get(get_info))
        .route("/payments/incoming", get(list_incoming_payments))
        .route(
            "/payments/incoming/{payment_hash}",
            get(get_incoming_payment),
        )
        .route("/getbalance", get(get_balance))
        .route("/listchannels", get(list_channels))
        .route("/decodeinvoice", post(decode_invoice))
        .route("/decodeoffer", post(decode_offer));

    let full_routes = Router::new()
        .route("/createinvoice", post(create_invoice))
        .route("/closechannel", post(close_channel))
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
    axum::Form(req): axum::Form<crate::types::CreateInvoiceRequest>,
) -> Result<axum::Json<crate::types::CreateInvoiceResponse>, error::AppError> {
    invoices::handle_create_invoice(state.node, state.metadata_store, state.mdk_client, &req).await
}

async fn get_incoming_payment(
    State(state): State<AppState>,
    path: axum::extract::Path<String>,
) -> Result<axum::Json<crate::types::IncomingPaymentResponse>, error::AppError> {
    invoices::handle_get_incoming_payment(state.node, state.metadata_store, path).await
}

async fn list_incoming_payments(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<crate::types::ListPaymentsRequest>,
) -> Result<axum::Json<Vec<crate::types::IncomingPaymentResponse>>, error::AppError> {
    invoices::handle_list_incoming_payments(state.node, state.metadata_store, &params).await
}

async fn get_info(
    State(state): State<AppState>,
) -> Result<axum::Json<crate::types::GetInfoResponse>, error::AppError> {
    info::handle_get_info(state.node).await
}

async fn get_balance(
    State(state): State<AppState>,
) -> Result<axum::Json<crate::types::GetBalanceResponse>, error::AppError> {
    balance::handle_get_balance(state.node).await
}

async fn decode_invoice(
    axum::Form(req): axum::Form<crate::types::DecodeInvoiceRequest>,
) -> Result<axum::Json<crate::types::DecodeInvoiceResponse>, error::AppError> {
    decode::handle_decode_invoice(&req)
}

async fn list_channels(
    State(state): State<AppState>,
) -> Result<axum::Json<Vec<crate::types::ChannelInfo>>, error::AppError> {
    channels::handle_list_channels(state.node).await
}

async fn close_channel(
    State(state): State<AppState>,
    axum::Form(req): axum::Form<crate::types::CloseChannelRequest>,
) -> Result<axum::http::StatusCode, error::AppError> {
    channels::handle_close_channel(state.node, &req).await
}

async fn decode_offer(
    axum::Form(req): axum::Form<crate::types::DecodeOfferRequest>,
) -> Result<axum::Json<crate::types::DecodeOfferResponse>, error::AppError> {
    decode::handle_decode_offer(&req)
}
