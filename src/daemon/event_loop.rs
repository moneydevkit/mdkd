use std::sync::Arc;

use log::{error, info, warn};
use tokio::sync::broadcast;

use mdk::client::MdkClient;
use mdk::types::MdkEvent;

use crate::daemon::store::invoice_metadata::InvoiceMetadataStore;
use crate::daemon::time;
use crate::daemon::types::WebhookEvent;
use crate::daemon::webhook::dispatcher::spawn_webhook_delivery;

pub async fn run_event_loop(
    mdk_client: Arc<MdkClient>,
    metadata_store: Arc<InvoiceMetadataStore>,
    webhook_secret: Vec<u8>,
    http_client: reqwest::Client,
    event_tx: broadcast::Sender<String>,
) {
    let mut rx = mdk_client.subscribe();
    loop {
        match rx.recv().await {
            Ok(event) => {
                handle_event(
                    &event,
                    &metadata_store,
                    &webhook_secret,
                    &http_client,
                    &event_tx,
                );
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("Daemon event handler lagged, missed {n} events");
            }
            Err(broadcast::error::RecvError::Closed) => {
                info!("MdkClient event channel closed, stopping daemon event loop");
                break;
            }
        }
    }
}

fn handle_event(
    event: &MdkEvent,
    metadata_store: &InvoiceMetadataStore,
    webhook_secret: &[u8],
    http_client: &reqwest::Client,
    event_tx: &broadcast::Sender<String>,
) {
    let MdkEvent::PaymentReceived {
        payment_hash,
        amount_sats,
    } = event
    else {
        return;
    };

    let metadata = match metadata_store.get_by_payment_hash(payment_hash) {
        Ok(Some(m)) => m,
        Ok(None) => return,
        Err(e) => {
            error!("Failed to look up invoice metadata: {e}");
            return;
        }
    };

    if let Err(e) = metadata_store.mark_paid(payment_hash) {
        error!("Failed to mark payment paid: {e}");
    }

    let webhook_event = WebhookEvent::PaymentReceived {
        payment_hash: payment_hash.clone(),
        amount_msat: amount_sats * 1000,
        external_id: metadata.external_id.clone(),
        timestamp: time::seconds_since_epoch(),
    };

    if let Ok(json) = serde_json::to_string(&webhook_event) {
        let _ = event_tx.send(json);
    }

    if let Some(webhook_url) = metadata.webhook_url {
        spawn_webhook_delivery(
            http_client.clone(),
            webhook_url,
            webhook_secret.to_vec(),
            webhook_event,
        );
    }
}
