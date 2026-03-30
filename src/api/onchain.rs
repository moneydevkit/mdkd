use std::sync::Arc;

use ldk_node::bitcoin::{Address, FeeRate};
use ldk_node::Node;
use log::error;

use crate::api::error::AppError;
use crate::store::invoice_metadata::{InvoiceMetadataStore, OutgoingSendRecord};
use crate::types::SendToAddressRequest;

pub async fn handle_send_to_address(
    node: Arc<Node>,
    metadata_store: Arc<InvoiceMetadataStore>,
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

    let txid_str = txid.to_string();

    // Store immediately so it appears in outgoing list before chain sync.
    let record = OutgoingSendRecord {
        txid: txid_str.clone(),
        address: req.address.clone(),
        amount_sat: req.amount_sat,
        fee_sat: None,
        created_at: crate::time::seconds_since_epoch(),
    };
    if let Err(e) = metadata_store.insert_outgoing_send(&record) {
        error!("Failed to store outgoing send: {e}");
    }

    Ok(txid_str)
}
