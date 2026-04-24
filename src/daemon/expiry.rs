use std::sync::Arc;
use std::time::Duration;

use log::{error, info};

use crate::daemon::store::invoice_metadata::InvoiceMetadataStore;
use crate::daemon::time;
use crate::daemon::types::WebhookEvent;
use crate::daemon::webhook::dispatcher::spawn_webhook_delivery;

const POLL_INTERVAL: Duration = Duration::from_secs(30);

pub async fn run_expiry_monitor(
    metadata_store: Arc<InvoiceMetadataStore>,
    webhook_secret: Vec<u8>,
    http_client: reqwest::Client,
) -> ! {
    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        let now = time::seconds_since_epoch();
        let expired = match metadata_store.get_expired_pending(now) {
            Ok(invoices) => invoices,
            Err(e) => {
                error!("Expiry monitor: failed to query expired invoices: {e}");
                continue;
            }
        };

        for meta in expired {
            let webhook_url = match meta.webhook_url {
                Some(ref url) => url.clone(),
                None => continue,
            };

            info!(
                "Invoice {} expired, firing webhook to {}",
                meta.payment_hash, webhook_url
            );

            let event = WebhookEvent::InvoiceExpired {
                payment_hash: meta.payment_hash.clone(),
                external_id: meta.external_id.clone(),
                timestamp: now,
            };

            spawn_webhook_delivery(
                http_client.clone(),
                webhook_url,
                webhook_secret.clone(),
                event,
            );

            if let Err(e) = metadata_store.mark_expired_notified(&meta.payment_hash) {
                error!(
                    "Expiry monitor: failed to mark {} as notified: {e}",
                    meta.payment_hash
                );
            }
        }
    }
}
