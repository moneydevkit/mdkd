use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use hex::DisplayHex;
use ldk_server::io::persist::paginated_kv_store::PaginatedKVStore;
use ldk_server::io::persist::{
    FORWARDED_PAYMENTS_PERSISTENCE_PRIMARY_NAMESPACE,
    FORWARDED_PAYMENTS_PERSISTENCE_SECONDARY_NAMESPACE, PAYMENTS_PERSISTENCE_PRIMARY_NAMESPACE,
    PAYMENTS_PERSISTENCE_SECONDARY_NAMESPACE,
};
use ldk_server::ldk_node::{Event, Node};
use ldk_server::util::proto_adapter::{forwarded_payment_to_proto, payment_to_proto};
use log::{error, info};
use prost::Message;

use crate::mdk::client::MdkApiClient;
use crate::mdk::types::{PaymentEntry, PaymentReceivedRequest};
use crate::store::invoice_metadata::InvoiceMetadataStore;
use crate::types::WebhookPayload;
use crate::webhook::dispatcher::spawn_webhook_delivery;

pub async fn run_event_loop(
    node: Arc<Node>,
    paginated_store: Arc<dyn PaginatedKVStore>,
    metadata_store: Arc<InvoiceMetadataStore>,
    webhook_secret: Vec<u8>,
    http_client: reqwest::Client,
    mdk_client: Arc<MdkApiClient>,
) {
    loop {
        let event = node.next_event_async().await;
        match event {
            Event::PaymentReceived {
                payment_id,
                payment_hash,
                amount_msat,
                ..
            } => {
                info!(
                    "PAYMENT_RECEIVED: id {:?}, hash {}, amount_msat {}",
                    payment_id, payment_hash, amount_msat
                );

                let payment_id = payment_id.expect("PaymentId expected for ldk-server >=0.1");

                if let Some(payment_details) = node.payment(&payment_id) {
                    let payment = payment_to_proto(payment_details);
                    let time = now();

                    match paginated_store.write(
                        PAYMENTS_PERSISTENCE_PRIMARY_NAMESPACE,
                        PAYMENTS_PERSISTENCE_SECONDARY_NAMESPACE,
                        &payment.id,
                        time,
                        &payment.encode_to_vec(),
                    ) {
                        Ok(_) => {
                            if let Err(e) = node.event_handled() {
                                error!("Failed to mark event as handled: {e}");
                            }
                        }
                        Err(e) => {
                            error!("Failed to write payment to persistence: {e}");
                        }
                    }
                } else {
                    error!("Unable to find payment with paymentId: {payment_id}");
                }

                // Trigger webhook and MDK notification if registered for this payment hash.
                let hash_str = payment_hash.to_string();
                match metadata_store.get_by_payment_hash(&hash_str) {
                    Ok(Some(metadata)) => {
                        if let Some(webhook_url) = metadata.webhook_url {
                            let payload = WebhookPayload {
                                event: "payment_received".into(),
                                payment_hash: hash_str.clone(),
                                amount_msat,
                                external_id: metadata.external_id.clone(),
                                timestamp: now(),
                            };
                            spawn_webhook_delivery(
                                http_client.clone(),
                                webhook_url,
                                webhook_secret.clone(),
                                payload,
                            );
                        }

                        // Notify moneydevkit.com of the payment.
                        let client = Arc::clone(&mdk_client);
                        let hash = hash_str.clone();
                        let amount_sats = amount_msat / 1000;
                        tokio::spawn(async move {
                            let req = PaymentReceivedRequest {
                                payments: vec![PaymentEntry {
                                    payment_hash: hash.clone(),
                                    amount_sats,
                                    sandbox: false,
                                }],
                            };
                            if let Err(e) = client.payment_received(&req).await {
                                error!(
                                    "Failed to notify moneydevkit.com for payment {}: {e}",
                                    hash
                                );
                            } else {
                                info!(
                                    "Notified moneydevkit.com of payment {} ({} sats)",
                                    hash, amount_sats
                                );
                            }
                        });
                    }
                    Ok(None) => {}
                    Err(e) => error!("Failed to look up invoice metadata: {e}"),
                }
            }
            Event::PaymentSuccessful { payment_id, .. } => {
                let payment_id = payment_id.expect("PaymentId expected for ldk-server >=0.1");
                upsert_payment(&node, &paginated_store, &payment_id);
            }
            Event::PaymentFailed { payment_id, .. } => {
                let payment_id = payment_id.expect("PaymentId expected for ldk-server >=0.1");
                upsert_payment(&node, &paginated_store, &payment_id);
            }
            Event::PaymentClaimable { payment_id, .. } => {
                if let Some(payment_details) = node.payment(&payment_id) {
                    let payment = payment_to_proto(payment_details);
                    upsert_payment_proto(&node, &paginated_store, &payment);
                } else {
                    error!("Unable to find payment with paymentId: {payment_id}");
                }
            }
            Event::PaymentForwarded {
                prev_channel_id,
                next_channel_id,
                prev_user_channel_id,
                next_user_channel_id,
                prev_node_id,
                next_node_id,
                total_fee_earned_msat,
                skimmed_fee_msat,
                claim_from_onchain_tx,
                outbound_amount_forwarded_msat,
            } => {
                info!(
                    "PAYMENT_FORWARDED: outbound_msat {}, fee_msat: {}, in: {}, out: {}",
                    outbound_amount_forwarded_msat.unwrap_or(0),
                    total_fee_earned_msat.unwrap_or(0),
                    prev_channel_id,
                    next_channel_id
                );

                let forwarded_payment = forwarded_payment_to_proto(
                    prev_channel_id,
                    next_channel_id,
                    prev_user_channel_id,
                    next_user_channel_id,
                    prev_node_id,
                    next_node_id,
                    total_fee_earned_msat,
                    skimmed_fee_msat,
                    claim_from_onchain_tx,
                    outbound_amount_forwarded_msat,
                );

                let mut forwarded_payment_id = [0u8; 32];
                getrandom::getrandom(&mut forwarded_payment_id)
                    .expect("Failed to generate random bytes");

                match paginated_store.write(
                    FORWARDED_PAYMENTS_PERSISTENCE_PRIMARY_NAMESPACE,
                    FORWARDED_PAYMENTS_PERSISTENCE_SECONDARY_NAMESPACE,
                    &forwarded_payment_id.to_lower_hex_string(),
                    now(),
                    &forwarded_payment.encode_to_vec(),
                ) {
                    Ok(_) => {
                        if let Err(e) = node.event_handled() {
                            error!("Failed to mark event as handled: {e}");
                        }
                    }
                    Err(e) => {
                        error!("Failed to write forwarded payment to persistence: {e}");
                    }
                }
            }
            Event::ChannelPending {
                channel_id,
                counterparty_node_id,
                ..
            } => {
                info!(
                    "CHANNEL_PENDING: {} from {}",
                    channel_id, counterparty_node_id
                );
                if let Err(e) = node.event_handled() {
                    error!("Failed to mark event as handled: {e}");
                }
            }
            Event::ChannelReady {
                channel_id,
                counterparty_node_id,
                ..
            } => {
                info!(
                    "CHANNEL_READY: {} from {:?}",
                    channel_id, counterparty_node_id
                );
                if let Err(e) = node.event_handled() {
                    error!("Failed to mark event as handled: {e}");
                }
            }
            _ => {
                if let Err(e) = node.event_handled() {
                    error!("Failed to mark event as handled: {e}");
                }
            }
        }
    }
}

fn upsert_payment(
    node: &Node,
    paginated_store: &Arc<dyn PaginatedKVStore>,
    payment_id: &ldk_server::ldk_node::lightning::ln::channelmanager::PaymentId,
) {
    if let Some(payment_details) = node.payment(payment_id) {
        let payment = payment_to_proto(payment_details);
        upsert_payment_proto(node, paginated_store, &payment);
    } else {
        error!("Unable to find payment with paymentId: {payment_id}");
    }
}

fn upsert_payment_proto(
    node: &Node,
    paginated_store: &Arc<dyn PaginatedKVStore>,
    payment: &ldk_server_protos::types::Payment,
) {
    match paginated_store.write(
        PAYMENTS_PERSISTENCE_PRIMARY_NAMESPACE,
        PAYMENTS_PERSISTENCE_SECONDARY_NAMESPACE,
        &payment.id,
        now(),
        &payment.encode_to_vec(),
    ) {
        Ok(_) => {
            if let Err(e) = node.event_handled() {
                error!("Failed to mark event as handled: {e}");
            }
        }
        Err(e) => {
            error!("Failed to write payment to persistence: {e}");
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time must be > 1970")
        .as_secs() as i64
}
