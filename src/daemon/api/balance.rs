use std::sync::Arc;

use axum::Json;
use mdk::client::MdkClient;

use crate::daemon::api::error::AppError;
use crate::daemon::types::GetBalanceResponse;

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
///
/// `max_withdrawable_sat` is what `balance_sat` can pay out after subtracting
/// a routing-fee buffer (see [`mdk::max_sendable`]). `None` when no usable
/// LSP channel exists.
pub async fn handle_get_balance(
    client: Arc<MdkClient>,
) -> Result<Json<GetBalanceResponse>, AppError> {
    let node = client.node();
    let balances = node.list_balances();
    let lightning_sat: u64 = node
        .list_channels()
        .iter()
        .map(|ch| ch.outbound_capacity_msat / 1000)
        .sum();

    let max_withdrawable_sat = client.max_sendable(None).ok().map(|e| e.amount_msat / 1000);

    Ok(Json(GetBalanceResponse {
        balance_sat: lightning_sat,
        onchain_balance_sat: balances.spendable_onchain_balance_sats,
        max_withdrawable_sat,
    }))
}
