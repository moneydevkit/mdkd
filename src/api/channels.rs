use std::sync::Arc;

use axum::Json;
use ldk_server::ldk_node::Node;

use crate::api::error::AppError;

pub async fn handle_list_channels(node: Arc<Node>) -> Result<Json<Vec<ChannelInfo>>, AppError> {
    let channels = node.list_channels().iter().map(ChannelInfo::from).collect();
    Ok(Json(channels))
}

