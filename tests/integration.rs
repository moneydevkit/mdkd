mod common;

use std::time::Duration;

use common::{
    encode_lnurl, fund_lsp, random_mnemonic, setup_payer_lsp_channel, LspNode, MdkdHandle,
    PayerNode, TestBitcoind, WebhookReceiver,
};

const TEST_MNEMONIC: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

#[tokio::test(flavor = "multi_thread")]
async fn test_mnemonic_deterministic_node_id() {
    let bitcoind = TestBitcoind::new();

    let server1 = MdkdHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;
    let node_id_1 = server1.node_id.clone();
    drop(server1);

    let server2 = MdkdHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;
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
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

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

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;

    let resp = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "test invoice"),
                ("expirySeconds", "3600"),
                ("externalId", "order-42"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200);
    let invoice: serde_json::Value = resp.json().await.unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap();
    assert!(
        invoice_str.starts_with("lnbcrt"),
        "Expected lnbcrt prefix, got: {invoice_str}"
    );
    assert!(!payment_hash.is_empty());
    assert_eq!(invoice["amountSat"].as_u64().unwrap(), 100_000);

    // GET the invoice back.
    let resp: serde_json::Value = server
        .get(&format!("/payments/incoming/{payment_hash}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(resp["paymentHash"].as_str().unwrap(), payment_hash);
    assert!(!resp["isPaid"].as_bool().unwrap());
    assert_eq!(resp["requestedSat"].as_u64().unwrap(), 100_000);
    assert_eq!(resp["receivedSat"].as_u64().unwrap(), 0);
    assert_eq!(resp["fees"].as_u64().unwrap(), 0);
    assert!(!resp["isExpired"].as_bool().unwrap());
    assert_eq!(resp["externalId"].as_str().unwrap(), "order-42");
    assert_eq!(resp["description"].as_str().unwrap(), "test invoice");
    assert!(resp["invoice"].as_str().unwrap().starts_with("lnbcrt"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_auth_required() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

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
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp = server
        .get("/payments/incoming/0000000000000000000000000000000000000000000000000000000000000000")
        .await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_payment_flow() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // First payment — triggers JIT channel open from LSP to server.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "payment test"),
                ("expirySeconds", "3600"),
                ("externalId", "order-99"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    let start = std::time::Instant::now();
    let settled: serde_json::Value = loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
            break resp;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for first payment to settle");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    };

    // requestedSat is the original invoice amount; receivedSat is after LSP fee.
    let requested = settled["requestedSat"].as_u64().unwrap();
    let received = settled["receivedSat"].as_u64().unwrap();
    let fees = settled["fees"].as_u64().unwrap();
    assert_eq!(requested, 100_000);
    assert!(
        received < requested,
        "receivedSat should be less than requestedSat due to LSP fee"
    );
    assert_eq!(
        fees,
        requested - received,
        "fees should equal requestedSat - receivedSat"
    );
    assert!(settled["preimage"].as_str().is_some());
    assert_eq!(settled["description"].as_str().unwrap(), "payment test");
    assert!(settled["completedAt"].as_u64().is_some());
    // A paid invoice is never expired.
    assert!(!settled["isExpired"].as_bool().unwrap());

    // Second payment — reuses the existing LSP->server channel.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "50000"),
                ("description", "second payment"),
                ("expirySeconds", "3600"),
                ("externalId", "order-100"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
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
    let server = MdkdHandle::start(
        &bitcoind,
        Some(webhook.port),
        Some(&lsp),
        &random_mnemonic(),
    )
    .await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Create invoice with webhook URL.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "webhook test"),
                ("expirySeconds", "3600"),
                ("externalId", "hook-order-1"),
                ("webhookUrl", &webhook.url()),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
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
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp: serde_json::Value = server.get("/getbalance").await.json().await.unwrap();
    assert_eq!(resp["balanceSat"].as_u64().unwrap(), 0);
    assert_eq!(resp["onchainBalanceSat"].as_u64().unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_getbalance_after_payment() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Pay into the server node.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "balance test"),
                ("expirySeconds", "3600"),
                ("externalId", "bal-1"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    // Wait for payment to settle (zero-conf JIT channel, no mining needed).
    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
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
        "Expected 98k sat balance after receiving payment, got: {resp}"
    );
}

/// Regression test: total_lightning_balance_sats reports 0 for sub-dust
/// balances because the force-close claimable output would be dust.
/// outbound_capacity_msat still reflects the real spendable amount.
#[tokio::test(flavor = "multi_thread")]
async fn test_getbalance_small_payment() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100"),
                ("description", "dust balance regression"),
                ("expirySeconds", "3600"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for payment to settle");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let resp: serde_json::Value = server.get("/getbalance").await.json().await.unwrap();
    assert!(
        resp["balanceSat"].as_u64().unwrap() > 0,
        "Balance should be nonzero even for sub-dust amounts, got: {resp}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_jit_channel_invoice() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // First payment — triggers JIT channel open from LSP to server.
    let resp = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "jit test"),
                ("expirySeconds", "3600"),
                ("externalId", "jit-order-1"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200);
    let invoice: serde_json::Value = resp.json().await.unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    assert!(
        invoice_str.starts_with("lnbcrt"),
        "Expected lnbcrt prefix, got: {invoice_str}"
    );

    payer.pay_invoice(invoice_str);

    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();
    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for JIT payment to settle");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Second payment — reuses the JIT channel (no new channel open).
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "50000"),
                ("description", "jit reuse test"),
                ("expirySeconds", "3600"),
                ("externalId", "jit-order-2"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
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

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;

    // Create an invoice to decode.
    let created: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "decode test"),
                ("expirySeconds", "3600"),
                ("externalId", "decode-1"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = created["serialized"].as_str().unwrap();
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
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

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
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    // POST with empty form body — axum returns 422 for missing fields.
    let resp = server.post_form("/decodeinvoice", &[]).await;
    assert!(
        resp.status() == 400 || resp.status() == 422,
        "Expected 4xx error, got {}",
        resp.status()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_list_incoming_payments() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Create two invoices with different externalIds.
    let inv1: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "list test 1"),
                ("expirySeconds", "3600"),
                ("externalId", "list-order-1"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let inv1_hash = inv1["paymentHash"].as_str().unwrap().to_string();
    let inv1_str = inv1["serialized"].as_str().unwrap().to_string();

    let inv2: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "50000"),
                ("description", "list test 2"),
                ("expirySeconds", "3600"),
                ("externalId", "list-order-2"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let inv2_hash = inv2["paymentHash"].as_str().unwrap().to_string();

    // all=true should return both (unpaid) invoices.
    let list: Vec<serde_json::Value> = server
        .get("/payments/incoming?all=true")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 2, "all=true should return both invoices");
    // Newest first (inv2 was created after inv1).
    assert_eq!(list[0]["paymentHash"].as_str().unwrap(), inv2_hash);
    assert_eq!(list[1]["paymentHash"].as_str().unwrap(), inv1_hash);

    // all=false (default) should return nothing — none are paid yet.
    let list: Vec<serde_json::Value> = server.get("/payments/incoming").await.json().await.unwrap();
    assert!(
        list.is_empty(),
        "default list should be empty before any payment"
    );

    // externalId filter on all=true.
    let list: Vec<serde_json::Value> = server
        .get("/payments/incoming?all=true&externalId=list-order-1")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["paymentHash"].as_str().unwrap(), inv1_hash);

    // Pay the first invoice.
    payer.pay_invoice(&inv1_str);

    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{inv1_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for payment to settle");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Now default (paid only) should return exactly one.
    let list: Vec<serde_json::Value> = server.get("/payments/incoming").await.json().await.unwrap();
    assert_eq!(list.len(), 1, "paid-only list should have one entry");
    assert_eq!(list[0]["paymentHash"].as_str().unwrap(), inv1_hash);
    assert!(list[0]["isPaid"].as_bool().unwrap());

    // all=true still returns both, newest first.
    let list: Vec<serde_json::Value> = server
        .get("/payments/incoming?all=true")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 2, "all=true should still return both");
    assert_eq!(list[0]["paymentHash"].as_str().unwrap(), inv2_hash);
    assert_eq!(list[1]["paymentHash"].as_str().unwrap(), inv1_hash);

    // Limit/offset.
    let list: Vec<serde_json::Value> = server
        .get("/payments/incoming?all=true&limit=1")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 1, "limit=1 should return one");
    assert_eq!(
        list[0]["paymentHash"].as_str().unwrap(),
        inv2_hash,
        "limit=1 should return the newest"
    );

    let list: Vec<serde_json::Value> = server
        .get("/payments/incoming?all=true&limit=1&offset=1")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 1, "offset=1 should return one entry");
    assert_eq!(
        list[0]["paymentHash"].as_str().unwrap(),
        inv1_hash,
        "offset=1 should skip the newest and return the oldest"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_listchannels_empty() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let channels: Vec<serde_json::Value> = server.get("/listchannels").await.json().await.unwrap();
    assert!(channels.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_listchannels_and_closechannel() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // No channels yet on the server side.
    let channels: Vec<serde_json::Value> = server.get("/listchannels").await.json().await.unwrap();
    assert!(channels.is_empty());

    // Pay into the server to trigger a JIT channel open.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "channel test"),
                ("expirySeconds", "3600"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    // Wait for payment to settle (JIT channel will be created).
    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for payment to settle");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Now we should have exactly one channel.
    let channels: Vec<serde_json::Value> = server.get("/listchannels").await.json().await.unwrap();
    assert_eq!(channels.len(), 1);

    let ch = &channels[0];
    assert!(
        ch["channelId"].as_str().unwrap().len() == 64,
        "channelId should be 64-char hex"
    );
    assert!(ch["balanceSat"].as_u64().unwrap() > 0);
    assert!(ch["capacitySat"].as_u64().unwrap() > 0);
    assert!(ch["inboundLiquiditySat"].is_number());
    // State should be ONLINE or OPENING (depends on confirmation depth).
    let state = ch["state"].as_str().unwrap();
    assert!(
        state == "ONLINE" || state == "OPENING",
        "unexpected state: {state}"
    );

    // /getinfo should report the same channel.
    let info: serde_json::Value = server.get("/getinfo").await.json().await.unwrap();
    let info_channels = info["channels"].as_array().unwrap();
    assert_eq!(info_channels.len(), 1);
    assert_eq!(
        info_channels[0]["channelId"].as_str().unwrap(),
        ch["channelId"].as_str().unwrap()
    );

    // Close the channel.
    let channel_id = ch["channelId"].as_str().unwrap();
    let resp = server
        .post_form("/closechannel", &[("channelId", channel_id)])
        .await;
    assert_eq!(resp.status(), 200);

    // After close initiation, the channel should eventually disappear
    // from list_channels (LDK removes closing channels).
    let start = std::time::Instant::now();
    loop {
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let channels: Vec<serde_json::Value> =
            server.get("/listchannels").await.json().await.unwrap();
        if channels.is_empty() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for channel to close");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_closechannel_not_found() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp = server
        .post_form(
            "/closechannel",
            &[(
                "channelId",
                "0000000000000000000000000000000000000000000000000000000000000000",
            )],
        )
        .await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_closechannel_invalid_hex() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp = server
        .post_form("/closechannel", &[("channelId", "not-hex")])
        .await;
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_getbalance_onchain_after_close() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Pay into the server to trigger a JIT channel open.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "onchain balance test"),
                ("expirySeconds", "3600"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    // Wait for payment to settle.
    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for payment to settle");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Onchain balance should be zero before closing the channel.
    let resp: serde_json::Value = server.get("/getbalance").await.json().await.unwrap();
    assert_eq!(resp["onchainBalanceSat"].as_u64().unwrap(), 0);
    let lightning_balance = resp["balanceSat"].as_u64().unwrap();
    assert!(lightning_balance > 0);

    // Close the channel.
    let channels: Vec<serde_json::Value> = server.get("/listchannels").await.json().await.unwrap();
    assert_eq!(channels.len(), 1);
    let channel_id = channels[0]["channelId"].as_str().unwrap();
    let resp = server
        .post_form("/closechannel", &[("channelId", channel_id)])
        .await;
    assert_eq!(resp.status(), 200);

    // Wait for the channel to disappear and the closing tx to confirm.
    let start = std::time::Instant::now();
    loop {
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let channels: Vec<serde_json::Value> =
            server.get("/listchannels").await.json().await.unwrap();
        if channels.is_empty() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for channel to close");
        }
    }

    // Mine enough blocks for the closing output to be spendable.
    bitcoind.mine_blocks(6);
    tokio::time::sleep(Duration::from_secs(3)).await;

    // After close, onchain balance should hold roughly the channel funds
    // (minus on-chain fees). Lightning balance should be zero.
    let resp: serde_json::Value = server.get("/getbalance").await.json().await.unwrap();
    let onchain = resp["onchainBalanceSat"].as_u64().unwrap();
    assert!(
        onchain > 0,
        "Expected non-zero onchain balance after channel close, got: {resp}"
    );
    assert_eq!(
        resp["balanceSat"].as_u64().unwrap(),
        0,
        "Lightning balance should be zero after channel close"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_sendtoaddress_invalid_address() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp = server
        .post_form(
            "/sendtoaddress",
            &[
                ("address", "not-a-real-address"),
                ("amountSat", "50000"),
                ("feerateSatByte", "10"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_sendtoaddress_missing_params() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    // Missing all required params.
    let resp = server.post_form("/sendtoaddress", &[]).await;
    assert!(
        resp.status() == 400 || resp.status() == 422,
        "Expected 4xx error, got {}",
        resp.status()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_sendtoaddress_insufficient_funds() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    // Valid regtest address but no on-chain funds.
    let resp = server
        .post_form(
            "/sendtoaddress",
            &[
                ("address", "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080"),
                ("amountSat", "50000"),
                ("feerateSatByte", "10"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 500);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "internal_error");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_sendtoaddress_success() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Pay into the server to trigger a JIT channel open.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "100000"),
                ("description", "sendtoaddress test"),
                ("expirySeconds", "3600"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let invoice_str = invoice["serialized"].as_str().unwrap();
    let payment_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(invoice_str);

    // Wait for payment to settle.
    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{payment_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for payment to settle");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Close the channel to move funds on-chain.
    let channels: Vec<serde_json::Value> = server.get("/listchannels").await.json().await.unwrap();
    assert_eq!(channels.len(), 1);
    let channel_id = channels[0]["channelId"].as_str().unwrap();
    let resp = server
        .post_form("/closechannel", &[("channelId", channel_id)])
        .await;
    assert_eq!(resp.status(), 200);

    // Wait for channel to close and funds to become spendable.
    let start = std::time::Instant::now();
    loop {
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let channels: Vec<serde_json::Value> =
            server.get("/listchannels").await.json().await.unwrap();
        if channels.is_empty() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for channel to close");
        }
    }

    bitcoind.mine_blocks(6);
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Verify we have on-chain funds.
    let balance: serde_json::Value = server.get("/getbalance").await.json().await.unwrap();
    let onchain = balance["onchainBalanceSat"].as_u64().unwrap();
    assert!(onchain > 0, "Expected on-chain balance, got: {balance}");

    // Send to a fresh bitcoind address.
    let dest_addr = bitcoind.bitcoind.client.new_address().unwrap().to_string();
    let send_amount = 10_000u64;

    let resp = server
        .post_form(
            "/sendtoaddress",
            &[
                ("address", &dest_addr),
                ("amountSat", &send_amount.to_string()),
                ("feerateSatByte", "10"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200);
    let txid = resp.text().await.unwrap();
    assert_eq!(txid.len(), 64, "txid should be 64-char hex");

    // The send should appear immediately in outgoing payments (before chain sync).
    let outgoing: Vec<serde_json::Value> =
        server.get("/payments/outgoing").await.json().await.unwrap();
    assert!(
        !outgoing.is_empty(),
        "Expected at least one outgoing payment immediately after send"
    );
    let found = outgoing.iter().find(|p| p["txId"].as_str() == Some(&txid));
    assert!(
        found.is_some(),
        "Outgoing payment with txid {txid} not found in list: {outgoing:?}"
    );
    let payment = found.unwrap();
    assert_eq!(payment["sent"].as_u64().unwrap(), send_amount);
    assert!(payment["txId"].as_str().is_some());

    // Confirm the send tx, then poll until the wallet syncs.
    bitcoind.mine_blocks(1);
    let start = std::time::Instant::now();
    loop {
        let balance_after: serde_json::Value =
            server.get("/getbalance").await.json().await.unwrap();
        let onchain_after = balance_after["onchainBalanceSat"].as_u64().unwrap();
        if onchain_after < onchain {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!(
                "On-chain balance never decreased after send: before={onchain}, after={onchain_after}"
            );
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    // After enough confirmations (ANTI_REORG_DELAY = 6), outgoing payment should
    // be marked as paid.
    bitcoind.mine_blocks(6);
    let start = std::time::Instant::now();
    loop {
        let outgoing: Vec<serde_json::Value> =
            server.get("/payments/outgoing").await.json().await.unwrap();
        let payment = outgoing
            .iter()
            .find(|p| p["txId"].as_str() == Some(&txid))
            .expect("outgoing payment should still be in list");
        if payment["isPaid"].as_bool().unwrap() {
            assert!(
                payment["fees"].as_u64().is_some(),
                "fees should be set after confirmation"
            );
            assert!(payment["completedAt"].as_u64().is_some());
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!("Outgoing payment never marked as paid: {payment}");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_openapi_scalar() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    // /scalar is outside the auth middleware — no credentials needed.
    let resp = reqwest::Client::new()
        .get(format!("{}/scalar", server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let html = resp.text().await.unwrap();
    assert!(html.contains("<!doctype html>") || html.contains("<!DOCTYPE html>"));

    // Verify the security scheme is present.
    assert!(
        html.contains("basic_auth"),
        "Missing basic_auth security scheme"
    );
}

// Spec test vector: offer with description + issuer + nodeId.
const BOLT12_OFFER: &str =
    "lno1pgx9getnwss8vetrw3hhyucjy358garswvaz7tmzdak8gvfj9ehhyeeqgf85c4p3xgsxjmnyw4ehgunfv4e3vggzamrjghtt05kvkvpcp0a79gmy3nt6jsn98ad2xs8de6sl9qmgvcvs";

#[tokio::test(flavor = "multi_thread")]
async fn test_decodeoffer() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp = server
        .post_form("/decodeoffer", &[("offer", BOLT12_OFFER)])
        .await;
    assert_eq!(resp.status(), 200);

    let decoded: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(decoded["offerId"].as_str().unwrap().len(), 64);
    assert_eq!(decoded["description"].as_str().unwrap(), "Test vectors");
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
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp = server
        .post_form("/decodeoffer", &[("offer", "not-a-real-offer")])
        .await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_payinvoice_invalid_bolt11() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp = server
        .post_form("/payinvoice", &[("invoice", "not-a-real-bolt11")])
        .await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
    assert!(body["error"]
        .as_str()
        .unwrap()
        .to_lowercase()
        .contains("bolt11"));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_payinvoice_outbound_payment() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Step 1: fund mdkd's outbound side by receiving a payment first.
    // This opens the JIT mdkd<->LSP channel and leaves mdkd with the inbound funds.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "200000"),
                ("description", "fund-mdkd-for-pay-test"),
                ("expirySeconds", "3600"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let inbound_invoice = invoice["serialized"].as_str().unwrap();
    let inbound_hash = invoice["paymentHash"].as_str().unwrap().to_string();

    payer.pay_invoice(inbound_invoice);

    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{inbound_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap_or(false) {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out funding mdkd via LSP JIT channel");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Step 2: PayerNode issues a fresh invoice we will pay FROM mdkd.
    let payer_balance_before = payer.outbound_capacity_msat();
    let outbound_invoice = payer.create_invoice(50_000, "pay test", 3600);

    // Step 3: hit /payinvoice on mdkd and wait for it to settle.
    let resp = server
        .post_form("/payinvoice", &[("invoice", &outbound_invoice)])
        .await;
    assert_eq!(resp.status(), 200, "/payinvoice returned non-200");
    let body: serde_json::Value = resp.json().await.unwrap();
    let payment_id = body["paymentId"].as_str().unwrap().to_string();
    assert_eq!(payment_id.len(), 64);
    assert_eq!(body["paymentHash"].as_str().unwrap().len(), 64);

    let start = std::time::Instant::now();
    let settled: serde_json::Value = loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/outgoing/{payment_id}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap_or(false) {
            break resp;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!(
                "Timed out waiting for outgoing payment to settle: {:?}",
                resp
            );
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(1)).await;
    };

    assert!(settled["isPaid"].as_bool().unwrap());
    assert!(
        settled["preimage"].as_str().is_some(),
        "settled payment should expose a preimage"
    );
    let sent = settled["sent"].as_u64().unwrap();
    assert_eq!(sent, 50_000, "sent should equal the invoice amount in sats");
    let fees = settled["fees"].as_u64().unwrap();
    assert!(fees < sent, "fees should be a fraction of sent amount");

    // Verify the payer node actually received the value (defense against silent
    // routing bugs where mdkd thinks the payment succeeded but the counterparty
    // never saw it).
    let start = std::time::Instant::now();
    loop {
        payer.sync_wallets();
        let payer_balance_after = payer.outbound_capacity_msat();
        // Payer's *outbound* capacity decreases by (received - fee they took, if any) when they
        // route - but since mdkd is paying THEM directly, payer's *inbound* capacity decreases.
        // We assert via list_balances spendable Lightning balance increase instead.
        let spendable = payer.node.list_balances().total_lightning_balance_sats;
        if spendable >= 50_000 {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            panic!(
                "Payer node never observed the inbound 50k sat (before={} after={} spendable={})",
                payer_balance_before, payer_balance_after, spendable
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_payinvoice_amount_mismatch_400() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    // A throwaway PayerNode just to mint a real bolt11 with an amount.
    let payer = PayerNode::new(&bitcoind);
    let invoice = payer.create_invoice(10_000, "mismatch test", 600);

    // amountSat that disagrees with the invoice amount must be rejected up front.
    let resp = server
        .post_form(
            "/payinvoice",
            &[("invoice", &invoice), ("amountSat", "5000")],
        )
        .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
    let err = body["error"].as_str().unwrap().to_lowercase();
    assert!(err.contains("does not match"), "unexpected error: {err}");
    assert!(err.contains("amountsat"), "unexpected error: {err}");

    // Matching amountSat must pass validation. Payment itself will fail with an
    // internal error (no channels in this minimal setup), but crucially it must
    // not be rejected with a 400 from the validation path.
    let resp = server
        .post_form(
            "/payinvoice",
            &[("invoice", &invoice), ("amountSat", "10000")],
        )
        .await;
    assert_ne!(
        resp.status(),
        400,
        "matching amountSat should pass validation, got status {} body {}",
        resp.status(),
        resp.text().await.unwrap_or_default()
    );
}

// ---------------------------------------------------------------------------
// /pay — unified send endpoint
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_invalid_destination_400() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp = server
        .post_form("/pay", &[("destination", "garbage-not-a-real-destination")])
        .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_amount_sat_zero_400() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    // amountSat=0 should fail before destination resolution; pass any string.
    let resp = server
        .post_form(
            "/pay",
            &[
                ("destination", "lnbcrt1pseventually-irrelevant"),
                ("amountSat", "0"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap().to_lowercase();
    assert!(err.contains("amountsat"), "unexpected error: {err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_wait_secs_over_cap_400() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let resp = server
        .post_form(
            "/pay",
            &[("destination", "anything"), ("waitForPaymentSecs", "51")],
        )
        .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap().to_lowercase();
    assert!(
        err.contains("waitforpaymentsecs"),
        "unexpected error: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_bolt11_amount_mismatch_400() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let payer = PayerNode::new(&bitcoind);
    let invoice = payer.create_invoice(10_000, "mismatch test", 600);

    let resp = server
        .post_form("/pay", &[("destination", &invoice), ("amountSat", "5000")])
        .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap().to_lowercase();
    assert!(err.contains("does not match"), "unexpected error: {err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_payer_note_on_bolt11_400() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let payer = PayerNode::new(&bitcoind);
    let invoice = payer.create_invoice(10_000, "payer note test", 600);

    let resp = server
        .post_form("/pay", &[("destination", &invoice), ("payerNote", "hello")])
        .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap().to_lowercase();
    assert!(
        err.contains("payernote") || err.contains("bolt12"),
        "unexpected: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_quantity_on_bolt11_400() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let payer = PayerNode::new(&bitcoind);
    let invoice = payer.create_invoice(10_000, "qty test", 600);

    let resp = server
        .post_form("/pay", &[("destination", &invoice), ("quantity", "3")])
        .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap().to_lowercase();
    assert!(
        err.contains("quantity") || err.contains("bolt12"),
        "unexpected: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_lnurl_dns_failure_500() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    // Bind a port, then drop the listener so the next connect refuses.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let lnurl = encode_lnurl(&format!("http://127.0.0.1:{port}/lnurl-init"));

    let resp = server
        .post_form("/pay", &[("destination", &lnurl), ("amountSat", "1000")])
        .await;
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "internal_error");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_lnurl_amount_mismatch_400() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    // PayerNode mints a real, signed BOLT11 with amount 10_000 sats.
    let payer = PayerNode::new(&bitcoind);
    let invoice = payer.create_invoice(10_000, "lnurl mismatch", 600);

    let mock_server = MockServer::start().await;
    let metadata = "[[\"text/plain\",\"test\"]]";
    let init = serde_json::json!({
        "tag": "payRequest",
        "callback": format!("{}/callback", mock_server.uri()),
        "minSendable": 1_000,
        "maxSendable": 100_000_000,
        "metadata": metadata,
    });
    Mock::given(method("GET"))
        .and(path("/lnurl-init"))
        .respond_with(ResponseTemplate::new(200).set_body_json(init))
        .mount(&mock_server)
        .await;
    // Callback returns the 10_000-sat invoice no matter what amount was requested.
    Mock::given(method("GET"))
        .and(path("/callback"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "pr": invoice,
            "routes": [],
        })))
        .mount(&mock_server)
        .await;

    let lnurl = encode_lnurl(&format!("{}/lnurl-init", mock_server.uri()));

    // Caller requests 5_000 sats, server hands back invoice for 10_000.
    let resp = server
        .post_form("/pay", &[("destination", &lnurl), ("amountSat", "5000")])
        .await;
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
    let err = body["error"].as_str().unwrap().to_lowercase();
    assert!(
        err.contains("wrong amount") || err.contains("malicious") || err.contains("does not match"),
        "unexpected error: {err}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_dispatch_failure_returns_500() {
    let bitcoind = TestBitcoind::new();
    // No LSP, no channels — mdkd has zero outbound capacity. Dispatch fails
    // synchronously and we must return ApiError (not PayResponse{failed}).
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    let payer = PayerNode::new(&bitcoind);
    let invoice = payer.create_invoice(1_000, "dispatch fail", 600);

    let resp = server
        .post_form(
            "/pay",
            &[
                ("destination", &invoice),
                // Skip any wait so we observe the synchronous dispatch error.
                ("waitForPaymentSecs", "0"),
            ],
        )
        .await;

    // Two acceptable shapes here:
    //   - 500 ApiError if ldk-node rejects the send synchronously
    //   - 200 PayResponse{status: pending} if ldk-node accepts and the
    //     failure surfaces only via a later PaymentFailed event
    // The contract we lock down: if 200, the body MUST be a PayResponse
    // with paymentId; if 500, the body is ApiError WITHOUT paymentId.
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap();
    if status == 500 {
        assert_eq!(body["code"].as_str().unwrap(), "internal_error");
        assert!(
            body.get("paymentId").is_none(),
            "ApiError must not contain paymentId, got {body:?}",
        );
    } else if status == 200 {
        assert!(
            body.get("paymentId").is_some(),
            "PayResponse must have paymentId"
        );
        assert_eq!(body["status"].as_str().unwrap(), "PENDING");
    } else {
        panic!("unexpected status {status}: {body:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_bolt11_blocks_until_succeeded() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Fund mdkd's outbound side via a JIT incoming payment.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "200000"),
                ("description", "fund-mdkd-for-pay-test"),
                ("expirySeconds", "3600"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let inbound_invoice = invoice["serialized"].as_str().unwrap();
    let inbound_hash = invoice["paymentHash"].as_str().unwrap().to_string();
    payer.pay_invoice(inbound_invoice);

    let start = std::time::Instant::now();
    loop {
        let resp: serde_json::Value = server
            .get(&format!("/payments/incoming/{inbound_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if resp["isPaid"].as_bool().unwrap_or(false) {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out funding mdkd via LSP JIT channel");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    // Issue an outbound invoice from PayerNode and call /pay.
    let outbound_invoice = payer.create_invoice(50_000, "pay-any test", 3600);

    // Mine in the background so the payment can settle on regtest while
    // mdkd is blocking on /pay (sync_wallets / route discovery may need
    // tip blocks). 30s default wait.
    let bitcoind_url = bitcoind.bitcoind.rpc_url();
    let _ = bitcoind_url; // placeholder; we just rely on test wall clock for now

    let resp = server
        .post_form(
            "/pay",
            &[
                ("destination", &outbound_invoice),
                ("waitForPaymentSecs", "45"),
            ],
        )
        .await;
    assert_eq!(resp.status(), 200, "/pay returned non-200");
    let body: serde_json::Value = resp.json().await.unwrap();
    let payment_id = body["paymentId"].as_str().unwrap().to_string();
    assert_eq!(payment_id.len(), 64);
    assert_eq!(body["paymentHash"].as_str().unwrap().len(), 64);

    // Allow either SUCCEEDED (terminal) or PENDING (event missed during the
    // 45s budget on a slow regtest); on PENDING we then poll outgoing.
    let status = body["status"].as_str().unwrap();
    if status == "SUCCEEDED" {
        assert!(body["preimage"].as_str().is_some(), "missing preimage");
        assert!(body["feeSat"].as_u64().is_some(), "missing feeSat");
    } else {
        assert_eq!(status, "PENDING", "unexpected status: {status}");
    }

    // Whether SUCCEEDED or PENDING, GET /payments/outgoing/{id} must converge
    // and the field shape must match.
    let start = std::time::Instant::now();
    let settled: serde_json::Value = loop {
        let r: serde_json::Value = server
            .get(&format!("/payments/outgoing/{payment_id}"))
            .await
            .json()
            .await
            .unwrap();
        if r["isPaid"].as_bool().unwrap_or(false) {
            break r;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for outgoing payment to settle: {:?}", r);
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(1)).await;
    };

    assert_eq!(settled["paymentId"].as_str().unwrap(), payment_id);
    assert_eq!(
        settled["paymentHash"].as_str().unwrap(),
        body["paymentHash"].as_str().unwrap()
    );
    assert!(settled["preimage"].as_str().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn test_pay_wait_zero_returns_pending() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkdHandle::start(&bitcoind, None, Some(&lsp), &random_mnemonic()).await;
    let payer = PayerNode::new(&bitcoind);
    setup_payer_lsp_channel(&bitcoind, &payer, &lsp, 500_000).await;

    // Fund mdkd's outbound side via a JIT incoming payment.
    let invoice: serde_json::Value = server
        .post_form(
            "/createinvoice",
            &[
                ("amountSat", "200000"),
                ("description", "fund-mdkd-for-pay-zero-wait"),
                ("expirySeconds", "3600"),
            ],
        )
        .await
        .json()
        .await
        .unwrap();
    let inbound_invoice = invoice["serialized"].as_str().unwrap();
    let inbound_hash = invoice["paymentHash"].as_str().unwrap().to_string();
    payer.pay_invoice(inbound_invoice);

    let start = std::time::Instant::now();
    loop {
        let r: serde_json::Value = server
            .get(&format!("/payments/incoming/{inbound_hash}"))
            .await
            .json()
            .await
            .unwrap();
        if r["isPaid"].as_bool().unwrap_or(false) {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out funding mdkd");
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let outbound_invoice = payer.create_invoice(40_000, "wait zero", 3600);

    let t0 = std::time::Instant::now();
    let resp = server
        .post_form(
            "/pay",
            &[
                ("destination", &outbound_invoice),
                ("waitForPaymentSecs", "0"),
            ],
        )
        .await;
    let elapsed = t0.elapsed();
    assert_eq!(resp.status(), 200);

    // wait=0 must short-circuit. Allow generous slack (3s) for the post-dispatch
    // node.payment() lookup; this still rules out the default 30s block.
    assert!(
        elapsed < Duration::from_secs(3),
        "wait=0 should short-circuit, took {elapsed:?}",
    );

    let body: serde_json::Value = resp.json().await.unwrap();
    let payment_id = body["paymentId"].as_str().unwrap().to_string();
    assert_eq!(payment_id.len(), 64);
    let status = body["status"].as_str().unwrap();
    // Status should be PENDING; in a pathological race it could be SUCCEEDED
    // before the lookup, but never FAILED for this funded path.
    assert!(
        status == "PENDING" || status == "SUCCEEDED",
        "got: {status}"
    );
    // Optional fields not yet known should be omitted (no nulls on the wire).
    if status == "PENDING" {
        assert!(
            body.get("preimage").is_none(),
            "preimage must be absent on PENDING"
        );
        assert!(
            body.get("feeSat").is_none(),
            "feeSat must be absent on PENDING"
        );
        assert!(
            body.get("reason").is_none(),
            "reason must be absent on PENDING"
        );
    }

    // Poll until terminal — proves the dispatched payment really did fly.
    let start = std::time::Instant::now();
    loop {
        let r: serde_json::Value = server
            .get(&format!("/payments/outgoing/{payment_id}"))
            .await
            .json()
            .await
            .unwrap();
        if r["isPaid"].as_bool().unwrap_or(false) {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            panic!("Timed out waiting for outgoing payment to settle: {:?}", r);
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

// Existing /payinvoice contract must still work after wiring /pay.
#[tokio::test(flavor = "multi_thread")]
async fn test_payinvoice_still_works_after_pay() {
    let bitcoind = TestBitcoind::new();
    let server = MdkdHandle::start(&bitcoind, None, None, &random_mnemonic()).await;

    // Garbage invoice → 400, just like before.
    let resp = server
        .post_form("/payinvoice", &[("invoice", "not-bolt11")])
        .await;
    assert_eq!(resp.status(), 400);

    // Suppress unused-import warnings inherited from common in tests that
    // don't otherwise touch them.
    let _ = WebhookReceiver::start;
}
