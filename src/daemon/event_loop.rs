use std::sync::Arc;

use ldk_node::{Event, Node};
use log::{error, info};
use tokio::sync::broadcast;

use mdk::mdk_api::client::MdkApiClient;
use mdk::mdk_api::types::{PaymentEntry, PaymentReceivedRequest};

use crate::daemon::store::invoice_metadata::InvoiceMetadataStore;
use crate::daemon::time;
use crate::daemon::types::WebhookEvent;
use crate::daemon::webhook::dispatcher::spawn_webhook_delivery;

pub async fn run_event_loop(
    node: Arc<Node>,
    metadata_store: Arc<InvoiceMetadataStore>,
    webhook_secret: Vec<u8>,
    http_client: reqwest::Client,
    mdk_client: Arc<MdkApiClient>,
    event_tx: broadcast::Sender<String>,
) {
    loop {
        let event = node.next_event_async().await;
        match event {
            Event::PaymentReceived {
                payment_hash,
                amount_msat,
                ..
            } => {
                info!(
                    "PAYMENT_RECEIVED: hash {}, amount_msat {}",
                    payment_hash, amount_msat
                );

                if let Err(e) = node.event_handled() {
                    error!("Failed to mark event as handled: {e}");
                }

                // Trigger webhook and MDK notification if registered for this payment hash.
                let hash_str = payment_hash.to_string();
                match metadata_store.get_by_payment_hash(&hash_str) {
                    Ok(Some(metadata)) => {
                        if let Err(e) = metadata_store.mark_paid(&hash_str) {
                            error!("Failed to mark payment paid: {e}");
                        }

                        let event = WebhookEvent::PaymentReceived {
                            payment_hash: hash_str.clone(),
                            amount_msat,
                            external_id: metadata.external_id.clone(),
                            timestamp: time::seconds_since_epoch(),
                        };

                        if let Ok(json) = serde_json::to_string(&event) {
                            let _ = event_tx.send(json);
                        }

                        if let Some(webhook_url) = metadata.webhook_url {
                            spawn_webhook_delivery(
                                http_client.clone(),
                                webhook_url,
                                webhook_secret.clone(),
                                event,
                            );
                        }

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
            Event::PaymentForwarded {
                prev_channel_id,
                next_channel_id,
                total_fee_earned_msat,
                outbound_amount_forwarded_msat,
                ..
            } => {
                info!(
                    "PAYMENT_FORWARDED: outbound_msat {}, fee_msat: {}, in: {}, out: {}",
                    outbound_amount_forwarded_msat.unwrap_or(0),
                    total_fee_earned_msat.unwrap_or(0),
                    prev_channel_id,
                    next_channel_id
                );
                if let Err(e) = node.event_handled() {
                    error!("Failed to mark event as handled: {e}");
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
