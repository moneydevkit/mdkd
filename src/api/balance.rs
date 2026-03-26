use std::sync::Arc;

use axum::Json;
use ldk_server::ldk_node::Node;

use crate::api::error::AppError;
use crate::types::GetBalanceResponse;

/// Returns the node's Lightning and on-chain balances.
///
/// `balance_sat`: Total sats owned across all Lightning channels. Uses
/// `total_lightning_balance_sats` which sums our side of every channel's
/// commitment regardless of peer connectivity or channel usability. This
/// means the balance stays stable even if the LSP goes offline. It reflects
/// ownership, not what can be routed at this instant.
///
/// `onchain_balance_sat`: Spendable on-chain sats.
pub async fn handle_get_balance(node: Arc<Node>) -> Result<Json<GetBalanceResponse>, AppError> {
    let balances = node.list_balances();

    Ok(Json(GetBalanceResponse {
        balance_sat: balances.total_lightning_balance_sats,
        onchain_balance_sat: balances.spendable_onchain_balance_sats,
    }))
}
