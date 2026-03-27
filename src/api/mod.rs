pub mod auth;
pub mod balance;
pub mod channels;
pub mod decode;
pub mod error;
pub mod info;
pub mod invoices;
pub mod onchain;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::Router;
use axum::{middleware, Form, Json};
use ldk_server::ldk_node::Node;

pub use auth::HttpAuth;

use crate::api::error::AppError;
use crate::mdk::client::MdkApiClient;
use crate::store::invoice_metadata::InvoiceMetadataStore;
use crate::types::{
    ChannelInfo, CloseChannelRequest, CreateInvoiceRequest, DecodeInvoiceRequest,
    DecodeInvoiceResponse, DecodeOfferRequest, DecodeOfferResponse, GetBalanceResponse,
    GetInfoResponse, IncomingPaymentResponse, ListPaymentsRequest, SendToAddressRequest,
};

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
        .route("/sendtoaddress", post(send_to_address))
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
    Form(req): Form<CreateInvoiceRequest>,
) -> Result<Json<crate::types::CreateInvoiceResponse>, AppError> {
    invoices::handle_create_invoice(state.node, state.metadata_store, state.mdk_client, &req).await
}

async fn get_incoming_payment(
    State(state): State<AppState>,
    path: Path<String>,
) -> Result<Json<IncomingPaymentResponse>, AppError> {
    invoices::handle_get_incoming_payment(state.node, state.metadata_store, path).await
}

async fn list_incoming_payments(
    State(state): State<AppState>,
    Query(params): Query<ListPaymentsRequest>,
) -> Result<Json<Vec<IncomingPaymentResponse>>, AppError> {
    invoices::handle_list_incoming_payments(state.node, state.metadata_store, &params).await
}

async fn get_info(State(state): State<AppState>) -> Result<Json<GetInfoResponse>, AppError> {
    info::handle_get_info(state.node).await
}

async fn get_balance(State(state): State<AppState>) -> Result<Json<GetBalanceResponse>, AppError> {
    balance::handle_get_balance(state.node).await
}

async fn decode_invoice(
    Form(req): Form<DecodeInvoiceRequest>,
) -> Result<Json<DecodeInvoiceResponse>, AppError> {
    decode::handle_decode_invoice(&req)
}

async fn list_channels(State(state): State<AppState>) -> Result<Json<Vec<ChannelInfo>>, AppError> {
    channels::handle_list_channels(state.node).await
}

async fn close_channel(
    State(state): State<AppState>,
    Form(req): Form<CloseChannelRequest>,
) -> Result<axum::http::StatusCode, AppError> {
    channels::handle_close_channel(state.node, &req).await
}

async fn send_to_address(
    State(state): State<AppState>,
    Form(req): Form<SendToAddressRequest>,
) -> Result<String, AppError> {
    onchain::handle_send_to_address(state.node, &req).await
}

async fn decode_offer(
    Form(req): Form<DecodeOfferRequest>,
) -> Result<Json<DecodeOfferResponse>, AppError> {
    decode::handle_decode_offer(&req)
}
