use std::sync::Arc;

use axum::Json;
use ldk_node::bitcoin::Network;
use ldk_node::Node;

use crate::daemon::api::error::AppError;
use crate::daemon::types::{ChannelInfo, GetInfoResponse};

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn handle_get_info(node: Arc<Node>) -> Result<Json<GetInfoResponse>, AppError> {
    let channels = node.list_channels().iter().map(ChannelInfo::from).collect();

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
