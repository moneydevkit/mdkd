use std::sync::Arc;

use axum::Json;
use ldk_server::ldk_node::Node;

use crate::api::error::AppError;
use crate::types::GetBalanceResponse;

/// Returns the node's Lightning balance.
///
/// `balance_sat`: Total sats owned across all Lightning channels. Uses
/// `total_lightning_balance_sats` which sums our side of every channel's
/// commitment regardless of peer connectivity or channel usability. This
/// means the balance stays stable even if the LSP goes offline. It reflects
/// ownership, not what can be routed at this instant.
pub async fn handle_get_balance(node: Arc<Node>) -> Result<Json<GetBalanceResponse>, AppError> {
    let balance_sat = node.list_balances().total_lightning_balance_sats;

    Ok(Json(GetBalanceResponse { balance_sat }))
}
