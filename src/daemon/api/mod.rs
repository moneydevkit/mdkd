pub mod auth;
pub mod balance;
pub mod channels;
pub mod decode;
pub mod error;
pub mod info;
pub mod invoices;
pub mod onchain;
pub mod pay;
pub mod pay_any;
pub mod websocket;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::{middleware, Form, Json, Router};
use ldk_node::Node;
use tokio::sync::broadcast;
use utoipa::openapi::security::{Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_scalar::{Scalar, Servable};

pub use auth::HttpAuth;

use mdk::client::MdkClient;

use crate::daemon::api::error::AppError;
use crate::daemon::store::invoice_metadata::InvoiceMetadataStore;
use crate::daemon::types::{
    ApiError, ChannelInfo, CloseChannelRequest, CreateInvoiceRequest, CreateInvoiceResponse,
    DecodeInvoiceRequest, DecodeInvoiceResponse, DecodeOfferRequest, DecodeOfferResponse,
    GetBalanceResponse, GetInfoResponse, IncomingPaymentResponse, ListOutgoingPaymentsRequest,
    ListPaymentsRequest, OutgoingPaymentResponse, PayInvoiceRequest, PayInvoiceResponse,
    PayRequest, PayResponse, SendToAddressRequest,
};

#[derive(Clone)]
pub struct AppState {
    pub node: Arc<Node>,
    pub metadata_store: Arc<InvoiceMetadataStore>,
    pub http_auth: HttpAuth,
    pub mdk_client: Arc<MdkClient>,
    pub event_tx: broadcast::Sender<String>,
}

#[derive(OpenApi)]
#[openapi(
    modifiers(&SecurityAddon),
    tags(
        (name = "node", description = "Node information and status"),
        (name = "channels", description = "Channel management"),
        (name = "payments", description = "Incoming payments"),
        (name = "invoices", description = "Invoice creation"),
        (name = "send", description = "Outbound Lightning payments"),
        (name = "decode", description = "Decode Lightning artifacts"),
        (name = "onchain", description = "On-chain operations"),
    )
)]
struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "basic_auth",
                SecurityScheme::Http(Http::new(HttpAuthScheme::Basic)),
            );
        }
    }
}

pub fn router(state: AppState) -> Router {
    let read_only_routes = OpenApiRouter::new()
        .routes(routes!(get_info))
        .routes(routes!(get_balance))
        .routes(routes!(list_channels))
        .routes(routes!(list_incoming_payments))
        .routes(routes!(get_incoming_payment))
        .routes(routes!(list_outgoing_payments))
        .routes(routes!(get_outgoing_payment))
        .routes(routes!(decode_invoice))
        .routes(routes!(decode_offer));

    let full_routes = OpenApiRouter::new()
        .routes(routes!(create_invoice))
        .routes(routes!(close_channel))
        .routes(routes!(send_to_address))
        .routes(routes!(pay_invoice))
        .routes(routes!(pay))
        .layer(middleware::from_fn(auth::require_full_access));

    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(read_only_routes)
        .merge(full_routes)
        .layer(middleware::from_fn_with_state(
            state.http_auth.clone(),
            auth::auth_middleware,
        ))
        .split_for_parts();

    let ws_state = websocket::WsState {
        auth: state.http_auth.clone(),
        event_tx: state.event_tx.clone(),
    };

    let router = router.merge(Scalar::with_url("/scalar", api)).route(
        "/websocket",
        axum::routing::get(websocket::handler).with_state(ws_state),
    );

    #[cfg(feature = "demo")]
    let router = {
        const DEMO_HTML: &str = include_str!("../../../wallet.html");
        router.route(
            "/",
            axum::routing::get(|| async { axum::response::Html(DEMO_HTML) }),
        )
    };

    router.with_state(state)
}

// -- Handlers -----------------------------------------------------------------

#[utoipa::path(
    get, path = "/getinfo", tag = "node",
    responses(
        (status = 200, body = GetInfoResponse),
        (status = 500, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn get_info(State(state): State<AppState>) -> Result<Json<GetInfoResponse>, AppError> {
    info::handle_get_info(state.node).await
}

#[utoipa::path(
    get, path = "/getbalance", tag = "node",
    responses(
        (status = 200, body = GetBalanceResponse),
        (status = 500, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn get_balance(State(state): State<AppState>) -> Result<Json<GetBalanceResponse>, AppError> {
    balance::handle_get_balance(state.mdk_client).await
}

#[utoipa::path(
    get, path = "/listchannels", tag = "channels",
    responses(
        (status = 200, body = Vec<ChannelInfo>),
        (status = 500, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn list_channels(State(state): State<AppState>) -> Result<Json<Vec<ChannelInfo>>, AppError> {
    channels::handle_list_channels(state.node).await
}

#[utoipa::path(
    get, path = "/payments/incoming", tag = "payments",
    params(ListPaymentsRequest),
    responses(
        (status = 200, body = Vec<IncomingPaymentResponse>),
        (status = 500, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn list_incoming_payments(
    State(state): State<AppState>,
    Query(params): Query<ListPaymentsRequest>,
) -> Result<Json<Vec<IncomingPaymentResponse>>, AppError> {
    invoices::handle_list_incoming_payments(state.node, state.metadata_store, &params).await
}

#[utoipa::path(
    get, path = "/payments/incoming/{payment_hash}", tag = "payments",
    params(("payment_hash" = String, Path, description = "Hex-encoded payment hash")),
    responses(
        (status = 200, body = IncomingPaymentResponse),
        (status = 404, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn get_incoming_payment(
    State(state): State<AppState>,
    path: Path<String>,
) -> Result<Json<IncomingPaymentResponse>, AppError> {
    invoices::handle_get_incoming_payment(state.node, state.metadata_store, path).await
}

#[utoipa::path(
    post, path = "/decodeinvoice", tag = "decode",
    request_body(content = DecodeInvoiceRequest, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 200, body = DecodeInvoiceResponse),
        (status = 400, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn decode_invoice(
    Form(req): Form<DecodeInvoiceRequest>,
) -> Result<Json<DecodeInvoiceResponse>, AppError> {
    decode::handle_decode_invoice(&req)
}

#[utoipa::path(
    post, path = "/decodeoffer", tag = "decode",
    request_body(content = DecodeOfferRequest, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 200, body = DecodeOfferResponse),
        (status = 400, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn decode_offer(
    Form(req): Form<DecodeOfferRequest>,
) -> Result<Json<DecodeOfferResponse>, AppError> {
    decode::handle_decode_offer(&req)
}

#[utoipa::path(
    post, path = "/createinvoice", tag = "invoices",
    request_body(content = CreateInvoiceRequest, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 200, body = CreateInvoiceResponse),
        (status = 400, body = ApiError),
        (status = 403, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn create_invoice(
    State(state): State<AppState>,
    Form(req): Form<CreateInvoiceRequest>,
) -> Result<Json<CreateInvoiceResponse>, AppError> {
    invoices::handle_create_invoice(state.mdk_client, state.metadata_store, &req).await
}

#[utoipa::path(
    post, path = "/closechannel", tag = "channels",
    request_body(content = CloseChannelRequest, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 200, description = "Channel close initiated"),
        (status = 400, body = ApiError),
        (status = 404, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn close_channel(
    State(state): State<AppState>,
    Form(req): Form<CloseChannelRequest>,
) -> Result<axum::http::StatusCode, AppError> {
    channels::handle_close_channel(state.node, &req).await
}

#[utoipa::path(
    post, path = "/sendtoaddress", tag = "onchain",
    request_body(content = SendToAddressRequest, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 200, description = "Transaction ID", body = String),
        (status = 400, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn send_to_address(
    State(state): State<AppState>,
    Form(req): Form<SendToAddressRequest>,
) -> Result<String, AppError> {
    onchain::handle_send_to_address(state.node, state.metadata_store, &req).await
}

#[utoipa::path(
    get, path = "/payments/outgoing", tag = "payments",
    params(ListOutgoingPaymentsRequest),
    responses(
        (status = 200, body = Vec<OutgoingPaymentResponse>),
        (status = 500, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn list_outgoing_payments(
    State(state): State<AppState>,
    Query(params): Query<ListOutgoingPaymentsRequest>,
) -> Result<Json<Vec<OutgoingPaymentResponse>>, AppError> {
    invoices::handle_list_outgoing_payments(state.node, state.metadata_store, &params).await
}

#[utoipa::path(
    get, path = "/payments/outgoing/{payment_id}", tag = "payments",
    params(("payment_id" = String, Path, description = "Hex-encoded payment ID")),
    responses(
        (status = 200, body = OutgoingPaymentResponse),
        (status = 404, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn get_outgoing_payment(
    State(state): State<AppState>,
    path: Path<String>,
) -> Result<Json<OutgoingPaymentResponse>, AppError> {
    invoices::handle_get_outgoing_payment(state.node, path).await
}

#[utoipa::path(
    post, path = "/payinvoice", tag = "send",
    request_body(content = PayInvoiceRequest, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 200, body = PayInvoiceResponse),
        (status = 400, body = ApiError),
        (status = 500, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn pay_invoice(
    State(state): State<AppState>,
    Form(req): Form<PayInvoiceRequest>,
) -> Result<Json<PayInvoiceResponse>, AppError> {
    Ok(Json(pay::handle_pay_invoice(state.node, &req).await?))
}

#[utoipa::path(
    post, path = "/pay", tag = "send",
    request_body(content = PayRequest, content_type = "application/x-www-form-urlencoded"),
    responses(
        (status = 200, body = PayResponse),
        (status = 400, body = ApiError),
        (status = 500, body = ApiError),
    ),
    security(("basic_auth" = []))
)]
async fn pay(
    State(state): State<AppState>,
    Form(req): Form<PayRequest>,
) -> Result<Json<PayResponse>, AppError> {
    Ok(Json(pay_any::handle_pay(state, &req).await?))
}
