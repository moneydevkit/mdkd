use std::sync::Arc;

use axum::Json;
use ldk_server::ldk_node::bitcoin::Network;
use ldk_server::ldk_node::Node;

use crate::api::error::AppError;
use crate::types::{ChannelInfo, ChannelState, GetInfoResponse};

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn handle_get_info(node: Arc<Node>) -> Result<Json<GetInfoResponse>, AppError> {
    let channels = node
        .list_channels()
        .into_iter()
        .map(|ch| {
            let state = match (ch.is_channel_ready, ch.is_usable) {
                (true, true) => ChannelState::Online,
                (true, false) => ChannelState::Offline,
                (false, _) => ChannelState::Opening,
            };

            ChannelInfo {
                state,
                channel_id: ch.channel_id.to_string(),
                balance_sat: ch.outbound_capacity_msat / 1000,
                inbound_liquidity_sat: ch.inbound_capacity_msat / 1000,
                capacity_sat: ch.channel_value_sats,
                funding_tx_id: ch.funding_txo.map(|txo| txo.txid.to_string()),
            }
        })
        .collect();

    let chain = match node.config().network {
        Network::Bitcoin => "mainnet",
        Network::Testnet => "testnet3",
        Network::Signet => "signet",
        Network::Regtest => "regtest",
        _ => "unknown",
    };

    let block_height = node.status().current_best_block.height;

    Ok(Json(GetInfoResponse {
        node_id: node.node_id().to_string(),
        channels,
        chain: chain.to_string(),
        block_height,
        version: VERSION,
    }))
}
