use std::sync::Arc;

use ldk_server::ldk_node::bitcoin::{Address, FeeRate};
use ldk_server::ldk_node::Node;

use crate::api::error::AppError;
use crate::types::SendToAddressRequest;

pub async fn handle_send_to_address(
    node: Arc<Node>,
    req: &SendToAddressRequest,
) -> Result<String, AppError> {
    let address: Address = req
        .address
        .parse::<Address<_>>()
        .map_err(|e| AppError::BadRequest(format!("invalid bitcoin address: {e}")))?
        .assume_checked();

    let fee_rate = req
        .feerate_sat_byte
        .map(|sat_per_vb| {
            FeeRate::from_sat_per_vb(sat_per_vb).ok_or_else(|| {
                AppError::BadRequest(format!("invalid feerateSatByte: {sat_per_vb} (overflow)"))
            })
        })
        .transpose()?;

    let txid = node
        .onchain_payment()
        .send_to_address(&address, req.amount_sat, fee_rate)
        .map_err(|e| AppError::Internal(format!("send_to_address failed: {e}")))?;

    Ok(txid.to_string())
}
