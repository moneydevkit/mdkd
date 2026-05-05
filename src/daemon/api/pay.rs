use std::str::FromStr;
use std::sync::Arc;

use hex::DisplayHex;
use ldk_node::lightning_invoice::Bolt11Invoice;
use ldk_node::Node;

use crate::daemon::api::error::AppError;
use crate::daemon::types::{PayInvoiceRequest, PayInvoiceResponse};

pub async fn handle_pay_invoice(
    node: Arc<Node>,
    req: &PayInvoiceRequest,
) -> Result<PayInvoiceResponse, AppError> {
    let invoice = Bolt11Invoice::from_str(req.invoice.trim())
        .map_err(|e| AppError::BadRequest(format!("invalid bolt11 invoice: {e}")))?;

    let bolt11 = node.bolt11_payment();
    let payment_id = match (invoice.amount_milli_satoshis(), req.amount_sat) {
        (Some(_), None) => bolt11
            .send(&invoice, None)
            .map_err(|e| AppError::Internal(format!("pay failed: {e}")))?,
        (None, Some(amount_sat)) => bolt11
            .send_using_amount(&invoice, amount_sat * 1000, None)
            .map_err(|e| AppError::Internal(format!("pay failed: {e}")))?,
        (Some(invoice_msat), Some(amount_sat)) => {
            if invoice_msat != amount_sat * 1000 {
                return Err(AppError::BadRequest(format!(
                    "amountSat ({amount_sat}) does not match invoice amount ({} sat)",
                    invoice_msat / 1000
                )));
            }
            bolt11
                .send(&invoice, None)
                .map_err(|e| AppError::Internal(format!("pay failed: {e}")))?
        }
        (None, None) => {
            return Err(AppError::BadRequest(
                "zero-amount invoice requires amountSat".into(),
            ))
        }
    };

    let payment_hash = invoice.payment_hash().to_string();

    Ok(PayInvoiceResponse {
        payment_id: payment_id.0.to_lower_hex_string(),
        payment_hash,
    })
}
