use std::time::Duration;

use hex::DisplayHex;
use hmac::{Hmac, Mac};
use log::{error, info};
use sha2::Sha256;

use crate::types::WebhookPayload;

type HmacSha256 = Hmac<Sha256>;

const RETRY_DELAYS: &[Duration] = &[
	Duration::from_secs(1),
	Duration::from_secs(5),
	Duration::from_secs(30),
];

pub fn spawn_webhook_delivery(
	client: reqwest::Client, url: String, secret: Vec<u8>, payload: WebhookPayload,
) {
	tokio::spawn(async move {
		if let Err(e) = deliver(&client, &url, &secret, &payload).await {
			error!("Webhook delivery to {} failed after all retries: {}", url, e);
		}
	});
}

async fn deliver(
	client: &reqwest::Client, url: &str, secret: &[u8], payload: &WebhookPayload,
) -> Result<(), String> {
	let body = serde_json::to_vec(payload).map_err(|e| format!("serialize: {}", e))?;

	let timestamp = payload.timestamp;
	let signature = compute_hmac(secret, &body);

	for (attempt, delay) in RETRY_DELAYS.iter().enumerate() {
		match client
			.post(url)
			.header("Content-Type", "application/json")
			.header("X-MDK-Signature", &signature)
			.header("X-MDK-Timestamp", timestamp.to_string())
			.body(body.clone())
			.timeout(Duration::from_secs(10))
			.send()
			.await
		{
			Ok(resp) if resp.status().is_success() => {
				info!("Webhook delivered to {} on attempt {}", url, attempt + 1);
				return Ok(());
			},
			Ok(resp) => {
				let status = resp.status();
				error!(
					"Webhook to {} attempt {} returned {}, retrying in {:?}",
					url,
					attempt + 1,
					status,
					delay
				);
			},
			Err(e) => {
				error!(
					"Webhook to {} attempt {} failed: {}, retrying in {:?}",
					url,
					attempt + 1,
					e,
					delay
				);
			},
		}
		tokio::time::sleep(*delay).await;
	}

	Err("all retries exhausted".into())
}

fn compute_hmac(secret: &[u8], body: &[u8]) -> String {
	let mut mac =
		HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
	mac.update(body);
	mac.finalize().into_bytes().to_lower_hex_string()
}
