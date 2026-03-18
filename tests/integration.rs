mod common;

use std::time::Duration;

use common::{
    setup_funded_channel, LspNode, MdkServerHandle, PayerNode, TestBitcoind, WebhookReceiver,
};

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
async fn test_node_info() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    let resp: serde_json::Value = server.get("/v1/node").await.json().await.unwrap();
    assert!(!resp["nodeId"].as_str().unwrap().is_empty());
    assert_eq!(resp["network"].as_str().unwrap(), "regtest");
    assert!(resp["channels"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_create_and_get_invoice() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;
    let payer = PayerNode::new(&bitcoind);
    setup_funded_channel(&bitcoind, &payer, &server, 200_000).await;

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
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    let resp = server
        .get("/v1/invoices/0000000000000000000000000000000000000000000000000000000000000000")
        .await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_payment_flow() {
    let bitcoind = TestBitcoind::new();
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;
    let payer = PayerNode::new(&bitcoind);

    setup_funded_channel(&bitcoind, &payer, &server, 200_000).await;

    // Create invoice on mdk-server.
    let body = serde_json::json!({
        "amountMsat": 10_000_000,
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
    let server = MdkServerHandle::start(&bitcoind, Some(webhook.port), None, TEST_MNEMONIC).await;
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
    let invoice: serde_json::Value = server
        .post("/v1/invoices", &body)
        .await
        .json()
        .await
        .unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn test_jit_channel_invoice() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);

    // Fund LSP so it can open JIT channels.
    let lsp_addr = lsp.onchain_address();
    bitcoind.fund_address(&lsp_addr, 1.0);
    bitcoind.mine_blocks(6);
    let start = std::time::Instant::now();
    loop {
        lsp.sync_wallets();
        if lsp.node.list_balances().spendable_onchain_balance_sats > 0 {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("Timed out waiting for LSP on-chain balance");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Start mdk-server pointed at real LSP — no channels yet.
    let server = MdkServerHandle::start(&bitcoind, None, Some(&lsp), TEST_MNEMONIC).await;

    // Create invoice — should take the JIT path (no inbound liquidity).
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

    // Pay via a payer that has a channel to the LSP.
    let payer = PayerNode::new(&bitcoind);
    let payer_addr = payer.onchain_address();
    bitcoind.fund_address(&payer_addr, 1.0);
    bitcoind.mine_blocks(6);
    let start = std::time::Instant::now();
    loop {
        payer.sync_wallets();
        if payer.node.list_balances().spendable_onchain_balance_sats > 0 {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("Timed out waiting for payer on-chain balance");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Open channel payer -> LSP.
    payer.open_channel(
        &lsp.node_id(),
        &format!("127.0.0.1:{}", lsp.p2p_port),
        500_000,
    );
    bitcoind.mine_blocks(6);
    let start = std::time::Instant::now();
    loop {
        payer.sync_wallets();
        if payer.list_channels_usable() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for payer->LSP channel");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // Pay the JIT invoice.
    payer.pay_invoice(invoice_str);

    // Wait for payment to settle (JIT channel open + forward).
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
}
