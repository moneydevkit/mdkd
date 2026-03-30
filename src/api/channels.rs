use std::sync::Arc;

use axum::http::StatusCode;
use axum::Json;
use hex::FromHex;
use ldk_node::lightning::ln::types::ChannelId;
use ldk_node::Node;

use crate::api::error::AppError;
use crate::types::{ChannelInfo, CloseChannelRequest};

pub async fn handle_list_channels(node: Arc<Node>) -> Result<Json<Vec<ChannelInfo>>, AppError> {
    let channels = node.list_channels().iter().map(ChannelInfo::from).collect();
    Ok(Json(channels))
}

pub async fn handle_close_channel(
    node: Arc<Node>,
    req: &CloseChannelRequest,
) -> Result<StatusCode, AppError> {
    let bytes = <[u8; 32]>::from_hex(&req.channel_id)
        .map_err(|_| AppError::BadRequest(format!("invalid channel_id hex: {}", req.channel_id)))?;
    let target = ChannelId::from_bytes(bytes);

    let ch = node
        .list_channels()
        .into_iter()
        .find(|ch| ch.channel_id == target)
        .ok_or_else(|| AppError::NotFound(format!("channel not found: {}", req.channel_id)))?;

    node.close_channel(&ch.user_channel_id, ch.counterparty_node_id)
        .map_err(|e| AppError::Internal(format!("close_channel failed: {e}")))?;

    Ok(StatusCode::OK)
}
