use std::sync::Arc;

use axum::Json;
use ldk_server::ldk_node::Node;

use crate::api::error::AppError;
use crate::types::{ChannelInfo, NodeInfoResponse};

pub async fn handle_get_node(
	node: Arc<Node>,
) -> Result<Json<NodeInfoResponse>, AppError> {
	let channels = node
		.list_channels()
		.into_iter()
		.map(|ch| ChannelInfo {
			channel_id: ch.channel_id.to_string(),
			counterparty_node_id: ch.counterparty_node_id.to_string(),
			channel_value_sats: ch.channel_value_sats,
			outbound_capacity_msat: ch.outbound_capacity_msat,
			inbound_capacity_msat: ch.inbound_capacity_msat,
			is_usable: ch.is_usable,
			is_channel_ready: ch.is_channel_ready,
		})
		.collect();

	let network = format!("{}", node.config().network);

	Ok(Json(NodeInfoResponse { node_id: node.node_id().to_string(), network, channels }))
}
