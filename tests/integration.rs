mod common;

use std::time::Duration;

use common::{
    fund_lsp, setup_payer_lsp_channel, LspNode, MdkServerHandle, PayerNode, TestBitcoind,
    WebhookReceiver,
};

use serde_json::json;

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[tokio::test(flavor = "multi_thread")]
async fn test_mnemonic_deterministic_node_id() {
    let bitcoind = TestBitcoind::new();

    let server1 = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;
    let node_id_1 = server1.node_id.clone();
    drop(server1);

    let server2 = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;
    let node_id_2 = server2.node_id.clone();
    drop(server2);

    assert_eq!(
        node_id_1, node_id_2,
        "Same mnemonic must produce the same node ID"
    );
    assert!(!node_id_1.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_get_info() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    let resp: serde_json::Value = server.get("/getinfo").await.json().await.unwrap();
    assert!(!resp["nodeId"].as_str().unwrap().is_empty());
    assert_eq!(resp["chain"].as_str().unwrap(), "regtest");
    assert!(resp["blockHeight"].as_u64().is_some());
    assert!(!resp["version"].as_str().unwrap().is_empty());
    assert!(resp["channels"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_create_and_get_invoice() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkServerHandle::start(&bitcoind, None, Some(&lsp), TEST_MNEMONIC).await;

    let body = serde_json::json!({
        "amountMsat": 100_000_000,
        "description": "test invoice",
        "expirySecs": 3600,
        "externalId": "order-42"
    });

    let resp = server.post("/v1/invoices", &body).await;
    assert_eq!(resp.status(), 200);
    let invoice: serde_json::Value = resp.json().await.unwrap();
    let invoice_str = invoice["invoice"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap();
    assert!(
        invoice_str.starts_with("lnbcrt"),
        "Expected lnbcrt prefix, got: {invoice_str}"
    );
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
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    // Request without auth header.
    let resp = reqwest::Client::new()
        .get(format!("{}/getinfo", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Request with wrong password.
    let resp = reqwest::Client::new()
        .get(format!("{}/getinfo", server.base_url()))
        .basic_auth("", Some("deadbeef"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_invoice_not_found() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    let resp = server
        .get("/v1/invoices/0000000000000000000000000000000000000000000000000000000000000000")
        .await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_payment_flow() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkServerHandle::start(&bitcoind, None, Some(&lsp), TEST_MNEMONIC).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // First payment — triggers JIT channel open from LSP to server.
    let body = serde_json::json!({
        "amountMsat": 100_000_000,
        "description": "payment test",
        "expirySecs": 3600,
        "externalId": "order-99"
    });
    let invoice: serde_json::Value = server
        .post("/v1/invoices", &body)
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["invoice"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/v1/invoices/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["status"].as_str().unwrap() == "received" {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for first payment to settle");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Second payment — reuses the existing LSP->server channel.
    let body = serde_json::json!({
        "amountMsat": 50_000_000,
        "description": "second payment",
        "expirySecs": 3600,
        "externalId": "order-100"
    });
    let invoice: serde_json::Value = server
        .post("/v1/invoices", &body)
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["invoice"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/v1/invoices/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["status"].as_str().unwrap() == "received" {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for second payment to settle");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_webhook_delivery() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let webhook = WebhookReceiver::start().await;
    let server =
        MdkServerHandle::start(&bitcoind, Some(webhook.port), Some(&lsp), TEST_MNEMONIC).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Create invoice with webhook URL.
    let body = serde_json::json!({
        "amountMsat": 100_000_000,
        "description": "webhook test",
        "expirySecs": 3600,
        "externalId": "hook-order-1",
        "webhookUrl": webhook.url()
    });
    let invoice: serde_json::Value = server
        .post("/v1/invoices", &body)
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["invoice"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

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
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for webhook delivery");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_getbalance_empty() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    let resp: serde_json::Value = server.get("/getbalance").await.json().await.unwrap();
    assert_eq!(resp["balanceSat"].as_u64().unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_getbalance_after_payment() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkServerHandle::start(&bitcoind, None, Some(&lsp), TEST_MNEMONIC).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Pay into the server node.
    let body = serde_json::json!({
        "amountMsat": 100_000_000,
        "description": "balance test",
        "expirySecs": 3600,
        "externalId": "bal-1"
    });
    let invoice: serde_json::Value = server
        .post("/v1/invoices", &body)
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["invoice"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    // Wait for payment to settle (zero-conf JIT channel, no mining needed).
    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/v1/invoices/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["status"].as_str().unwrap() == "received" {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for payment to settle");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let resp: serde_json::Value = server.get("/getbalance").await.json().await.unwrap();
    assert!(
        resp["balanceSat"].as_u64().unwrap() == 98_000, // 2% LSP fee
        "Expected non-zero balance after receiving payment, got: {resp}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_jit_channel_invoice() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkServerHandle::start(&bitcoind, None, Some(&lsp), TEST_MNEMONIC).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // First payment — triggers JIT channel open from LSP to server.
    let body = serde_json::json!({
        "amountMsat": 100_000_000,
        "description": "jit test",
        "expirySecs": 3600,
        "externalId": "jit-order-1"
    });
    let resp = server.post("/v1/invoices", &body).await;
    assert_eq!(resp.status(), 200);
    let invoice: serde_json::Value = resp.json().await.unwrap();
    let invoice_str = invoice["invoice"].as_str().unwrap();
    assert!(
        invoice_str.starts_with("lnbcrt"),
        "Expected lnbcrt prefix, got: {invoice_str}"
    );

    payer.pay_invoice(invoice_str);

    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();
    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/v1/invoices/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["status"].as_str().unwrap() == "received" {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for JIT payment to settle");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Second payment — reuses the JIT channel (no new channel open).
    let body = serde_json::json!({
        "amountMsat": 50_000_000,
        "description": "jit reuse test",
        "expirySecs": 3600,
        "externalId": "jit-order-2"
    });
    let invoice: serde_json::Value = server
        .post("/v1/invoices", &body)
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["invoice"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/v1/invoices/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["status"].as_str().unwrap() == "received" {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for second JIT payment to settle");
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_decodeinvoice() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkServerHandle::start(&bitcoind, None, Some(&lsp), TEST_MNEMONIC).await;

    // Create an invoice to decode.
    let create_body = json!({
        "amountMsat": 100_000_000,
        "description": "decode test",
        "expirySecs": 3600,
        "externalId": "decode-1"
    });
    let created: serde_json::Value = server
        .post("/v1/invoices", &create_body)
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = created["invoice"].as_str().unwrap();
    let expected_hash = created["paymentHash"].as_str().unwrap();

    // Decode it.
    let resp = server
        .post_form("/decodeinvoice", &[("invoice", invoice_str)])
        .await;
    assert_eq!(resp.status(), 200);

    let decoded: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(decoded["paymentHash"].as_str().unwrap(), expected_hash);
    assert_eq!(decoded["amountMsat"].as_u64().unwrap(), 100_000_000);
    assert_eq!(decoded["amount"].as_u64().unwrap(), 100_000);
    assert_eq!(decoded["description"].as_str().unwrap(), "decode test");
    assert_eq!(decoded["expirySeconds"].as_u64().unwrap(), 3600);
    assert!(decoded["createdAtSeconds"].as_u64().unwrap() > 0);
    assert!(!decoded["nodeId"].as_str().unwrap().is_empty());
    assert!(!decoded["paymentSecret"].as_str().unwrap().is_empty());
    assert!(decoded["routingHints"].as_array().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_decodeinvoice_invalid() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    let resp = server
        .post_form("/decodeinvoice", &[("invoice", "not-a-real-invoice")])
        .await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_decodeinvoice_missing_param() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    // POST with empty form body — axum returns 422 for missing fields.
    let resp = server.post_form("/decodeinvoice", &[]).await;
    assert!(
        resp.status() == 400 || resp.status() == 422,
        "Expected 4xx error, got {}",
        resp.status()
    );
}

// Spec test vector: offer with description + issuer + nodeId.
const BOLT12_OFFER: &str =
    "lno1pgx9getnwss8vetrw3hhyucjy358garswvaz7tmzdak8gvfj9ehhyeeqgf85c4p3xgsxjmnyw4ehgunfv4e3vggzamrjghtt05kvkvpcp0a79gmy3nt6jsn98ad2xs8de6sl9qmgvcvs";

#[tokio::test(flavor = "multi_thread")]
async fn test_decodeoffer() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    let resp = server
        .post_form("/decodeoffer", &[("offer", BOLT12_OFFER)])
        .await;
    assert_eq!(resp.status(), 200);

    let decoded: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(decoded["offerId"].as_str().unwrap().len(), 64);
    assert_eq!(
        decoded["description"].as_str().unwrap(),
        "Test vectors"
    );
    assert_eq!(
        decoded["issuer"].as_str().unwrap(),
        "https://bolt12.org BOLT12 industries"
    );
    assert!(decoded["nodeId"].as_str().is_some());
    assert!(decoded["amount"].is_null());
    assert!(decoded["amountMsat"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_decodeoffer_invalid() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    let resp = server
        .post_form("/decodeoffer", &[("offer", "not-a-real-offer")])
        .await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}
