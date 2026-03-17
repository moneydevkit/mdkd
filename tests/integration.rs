mod common;

use std::time::Duration;

use common::{
	setup_funded_channel, MdkServerHandle, PayerNode, TestBitcoind, WebhookReceiver,
};

#[tokio::test(flavor = "multi_thread")]
async fn test_node_info() {
	let bitcoind = TestBitcoind::new();
	let server = MdkServerHandle::start(&bitcoind, None).await;

	let resp: serde_json::Value = server.get("/v1/node").await.json().await.unwrap();
	assert!(!resp["nodeId"].as_str().unwrap().is_empty());
	assert_eq!(resp["network"].as_str().unwrap(), "regtest");
	assert!(resp["channels"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_create_and_get_invoice() {
	let bitcoind = TestBitcoind::new();
	let server = MdkServerHandle::start(&bitcoind, None).await;

	let body = serde_json::json!({
		"amountMsat": 100_000,
		"description": "test invoice",
		"expirySecs": 3600,
		"externalId": "order-42"
	});

	let resp = server.post("/v1/invoices", &body).await;
	assert_eq!(resp.status(), 200);
	let invoice: serde_json::Value = resp.json().await.unwrap();
	let invoice_str = invoice["invoice"].as_str().unwrap();
	let payment_hash = invoice["paymentHash"].as_str().unwrap();
	assert!(invoice_str.starts_with("lnbcrt"), "Expected lnbcrt prefix, got: {invoice_str}");
	assert!(!payment_hash.is_empty());
	assert_eq!(invoice["externalId"].as_str().unwrap(), "order-42");
	assert!(invoice["expiresAt"].as_u64().unwrap() > 0);

	// GET the invoice back.
	let resp: serde_json::Value = server
		.get(&format!("/v1/invoices/{payment_hash}"))
		.await
		.json()
		.await
		.unwrap();
	assert_eq!(resp["paymentHash"].as_str().unwrap(), payment_hash);
	assert_eq!(resp["status"].as_str().unwrap(), "pending");
	assert_eq!(resp["externalId"].as_str().unwrap(), "order-42");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_required() {
	let bitcoind = TestBitcoind::new();
	let server = MdkServerHandle::start(&bitcoind, None).await;

	// Request without auth header.
	let resp = reqwest::Client::new()
		.get(format!("{}/v1/node", server.base_url()))
		.send()
		.await
		.unwrap();
	assert_eq!(resp.status(), 401);

	// Request with wrong key.
	let resp = reqwest::Client::new()
		.get(format!("{}/v1/node", server.base_url()))
		.header("Authorization", "Bearer deadbeef")
		.send()
		.await
		.unwrap();
	assert_eq!(resp.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invoice_not_found() {
	let bitcoind = TestBitcoind::new();
	let server = MdkServerHandle::start(&bitcoind, None).await;

	let resp = server.get("/v1/invoices/0000000000000000000000000000000000000000000000000000000000000000").await;
	assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_payment_flow() {
	let bitcoind = TestBitcoind::new();
	let server = MdkServerHandle::start(&bitcoind, None).await;
	let payer = PayerNode::new(&bitcoind);

	setup_funded_channel(&bitcoind, &payer, &server, 200_000).await;

	// Create invoice on mdk-server.
	let body = serde_json::json!({
		"amountMsat": 10_000_000,
		"description": "payment test",
		"expirySecs": 3600,
		"externalId": "order-99"
	});
	let invoice: serde_json::Value = server.post("/v1/invoices", &body).await.json().await.unwrap();
	let invoice_str = invoice["invoice"].as_str().unwrap();
	let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

	// Pay from the payer node.
	payer.pay_invoice(invoice_str);

	// Wait for payment to settle.
	tokio::time::sleep(Duration::from_secs(5)).await;

	// Verify invoice status is "received".
	let resp: serde_json::Value = server
		.get(&format!("/v1/invoices/{payment_hash}"))
		.await
		.json()
		.await
		.unwrap();
	assert_eq!(resp["status"].as_str().unwrap(), "received");
	assert_eq!(resp["externalId"].as_str().unwrap(), "order-99");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_webhook_delivery() {
	let bitcoind = TestBitcoind::new();
	let webhook = WebhookReceiver::start().await;
	let server = MdkServerHandle::start(&bitcoind, Some(webhook.port)).await;
	let payer = PayerNode::new(&bitcoind);

	setup_funded_channel(&bitcoind, &payer, &server, 200_000).await;

	// Create invoice with webhook URL.
	let body = serde_json::json!({
		"amountMsat": 10_000_000,
		"description": "webhook test",
		"expirySecs": 3600,
		"externalId": "hook-order-1",
		"webhookUrl": webhook.url()
	});
	let invoice: serde_json::Value = server.post("/v1/invoices", &body).await.json().await.unwrap();
	let invoice_str = invoice["invoice"].as_str().unwrap();
	let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

	// Pay from the payer node.
	payer.pay_invoice(invoice_str);

	// Wait for payment + webhook delivery.
	let start = std::time::Instant::now();
	loop {
		let received = webhook.received();
		if !received.is_empty() {
			let payload = &received[0];
			assert_eq!(payload["event"].as_str().unwrap(), "payment_received");
			assert_eq!(payload["paymentHash"].as_str().unwrap(), payment_hash);
			assert_eq!(payload["externalId"].as_str().unwrap(), "hook-order-1");
			assert!(payload["amountMsat"].as_u64().unwrap() > 0);
			break;
		}
		if start.elapsed() > Duration::from_secs(30) {
			panic!("Timed out waiting for webhook delivery");
		}
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
}
