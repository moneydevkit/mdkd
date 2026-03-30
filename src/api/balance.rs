use std::sync::Arc;

use axum::Json;
use ldk_node::Node;

use crate::api::error::AppError;
use crate::types::GetBalanceResponse;

/// Returns the node's Lightning and on-chain balances.
///
/// `balance_sat` sums `outbound_capacity_msat` across all channels.
/// This reflects what the user can actually spend over Lightning right now.
///
/// Known limitation: `outbound_capacity_msat` drops to zero when the peer
/// is disconnected. A proper ownership balance would require a field LDK
/// doesn't currently expose on `ChannelDetails`.
///
/// We intentionally avoid `total_lightning_balance_sats` because it reports
/// force-close claimable amounts (after on-chain fees and reserves), which
/// can be zero even when the channel has a real outbound balance.
///
/// `onchain_balance_sat` is what the user can actually sweep/send on-chain right now.
pub async fn handle_get_balance(node: Arc<Node>) -> Result<Json<GetBalanceResponse>, AppError> {
    let balances = node.list_balances();
    let lightning_sat: u64 = node
        .list_channels()
        .iter()
        .map(|ch| ch.outbound_capacity_msat / 1000)
        .sum();

    Ok(Json(GetBalanceResponse {
        balance_sat: lightning_sat,
        onchain_balance_sat: balances.spendable_onchain_balance_sats,
    }))
}
