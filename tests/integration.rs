mod common;

use std::time::Duration;

use common::{
    fund_lsp, setup_payer_lsp_channel, LspNode, MdkServerHandle, PayerNode, TestBitcoind,
    WebhookReceiver,
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
        .get("/payments/incoming/0000000000000000000000000000000000000000000000000000000000000000")
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
    let server =
        MdkServerHandle::start(&bitcoind, Some(webhook.port), Some(&lsp), TEST_MNEMONIC).await;
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

    let server = MdkServerHandle::start(&bitcoind, None, Some(&lsp), TEST_MNEMONIC).await;

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

#[tokio::test(flavor = "multi_thread")]
async fn test_list_incoming_payments() {
    let bitcoind = TestBitcoind::new();
    let lsp = LspNode::new(&bitcoind);
    fund_lsp(&bitcoind, &lsp).await;

    let server = MdkServerHandle::start(&bitcoind, None, Some(&lsp), TEST_MNEMONIC).await;
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
    let server = MdkServerHandle::start(&bitcoind, None, None, TEST_MNEMONIC).await;

    let resp = server
        .post_form("/decodeoffer", &[("offer", "not-a-real-offer")])
        .await;
    assert_eq!(resp.status(), 400);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"].as_str().unwrap(), "bad_request");
}
