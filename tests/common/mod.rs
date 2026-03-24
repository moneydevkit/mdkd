use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ldk_server::ldk_node::bitcoin::Network;
use ldk_server::ldk_node::config::Config as LdkNodeConfig;
use ldk_server::ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_server::ldk_node::lightning_invoice::Bolt11Invoice;
use ldk_server::ldk_node::{Builder, Node};

use ldk_node_lsp::config::Config as LspNodeConfig;
use ldk_node_lsp::lightning::ln::msgs::SocketAddress as LspSocketAddress;
use ldk_node_lsp::liquidity::LSPS4ServiceConfig;
use ldk_node_lsp::Builder as LspBuilder;
use ldk_node_lsp::Node as LspLdkNode;

// ---------------------------------------------------------------------------
// TestBitcoind
// ---------------------------------------------------------------------------

pub struct TestBitcoind {
    pub bitcoind: corepc_node::Node,
}

impl TestBitcoind {
    pub fn new() -> Self {
        let exe = std::env::var("BITCOIND_EXE")
            .expect("BITCOIND_EXE must be set (use `nix develop` or set it manually)");
        let bitcoind = corepc_node::Node::new(exe).unwrap();
        let address = bitcoind.client.new_address().unwrap();
        bitcoind.client.generate_to_address(101, &address).unwrap();
        Self { bitcoind }
    }

    pub fn mine_blocks(&self, count: u64) {
        let address = self.bitcoind.client.new_address().unwrap();
        self.bitcoind
            .client
            .generate_to_address(count as usize, &address)
            .unwrap();
    }

    pub fn fund_address(&self, addr: &str, btc_amount: f64) {
        use corepc_node::client::bitcoin::{Address, Amount};
        let address: Address<corepc_node::client::bitcoin::address::NetworkUnchecked> =
            addr.parse().unwrap();
        let address = address.assume_checked();
        let amount = Amount::from_btc(btc_amount).unwrap();
        self.bitcoind
            .client
            .send_to_address(&address, amount)
            .unwrap();
        self.mine_blocks(1);
    }

    pub fn rpc_details(&self) -> (String, u16, String, String) {
        let rpc_url = self.bitcoind.rpc_url();
        let rpc_address = rpc_url.strip_prefix("http://").unwrap_or(&rpc_url);
        let parts: Vec<&str> = rpc_address.splitn(2, ':').collect();
        let host = parts[0].to_string();
        let port: u16 = parts[1].parse().unwrap();

        let cookie = std::fs::read_to_string(&self.bitcoind.params.cookie_file).unwrap();
        let mut cparts = cookie.splitn(2, ':');
        let user = cparts.next().unwrap().to_string();
        let password = cparts.next().unwrap().to_string();

        (host, port, user, password)
    }
}

// ---------------------------------------------------------------------------
// MdkServerHandle
// ---------------------------------------------------------------------------

pub struct MdkServerHandle {
    child: Option<Child>,
    pub api_port: u16,
    pub p2p_port: u16,
    pub storage_dir: PathBuf,
    pub http_password_full: String,
    pub node_id: String,
    client: reqwest::Client,
    _mock_mdk: MockMdkApi,
}

impl MdkServerHandle {
    pub async fn start(
        bitcoind: &TestBitcoind,
        webhook_port: Option<u16>,
        lsp: Option<&LspNode>,
        mnemonic: &str,
    ) -> Self {
        #[allow(deprecated)]
        let storage_dir = tempfile::tempdir().unwrap().into_path();

        let api_port = find_available_port();
        let p2p_port = find_available_port();

        let (rpc_host, rpc_port, rpc_user, rpc_password) = bitcoind.rpc_details();
        let rpc_address = format!("{rpc_host}:{rpc_port}");

        let webhook_secret = "aa".repeat(32); // 32 bytes as hex

        let (lsp_node_id, lsp_address) = match lsp {
            Some(l) => (l.node_id(), format!("127.0.0.1:{}", l.p2p_port)),
            None => (
                "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798".to_string(),
                "127.0.0.1:19735".to_string(),
            ),
        };

        let mock_mdk = MockMdkApi::start().await;
        let mdk_api_base_url = mock_mdk.base_url();
        let mdk_access_token = "test_token_dummy";
        let http_password_full = "test_full_password";
        let http_password_read_only = "test_readonly_password";

        let config = format!(
            r#"[node]
network = "regtest"
listening_addresses = ["127.0.0.1:{p2p_port}"]
rest_service_address = "127.0.0.1:{api_port}"

[storage.disk]
dir_path = "{storage_dir}"

[bitcoind]
rpc_address = "{rpc_address}"
rpc_user = "{rpc_user}"
rpc_password = "{rpc_password}"
"#,
            storage_dir = storage_dir.display(),
        );

        let config_path = storage_dir.join("config.toml");
        std::fs::write(&config_path, &config).unwrap();

        let binary = env!("CARGO_BIN_EXE_mdk-server");
        let mut child = Command::new(binary)
            .arg(config_path.to_str().unwrap())
            .env("MDK_MNEMONIC", mnemonic)
            .env("MDK_ACCESS_TOKEN", mdk_access_token)
            .env("MDK_HTTP_PASSWORD_FULL", http_password_full)
            .env("MDK_HTTP_PASSWORD_READ_ONLY", http_password_read_only)
            .env("MDK_LSP_NODE_ID", &lsp_node_id)
            .env("MDK_LSP_ADDRESS", &lsp_address)
            .env("MDK_API_BASE_URL", &mdk_api_base_url)
            .env("MDK_WEBHOOK_SECRET", &webhook_secret)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("Failed to start mdk-server at {binary}: {e}"));

        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                eprintln!("[mdk-server stdout] {}", line);
            }
        });
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if line.contains("Failed to retrieve fee rate estimates") {
                    continue;
                }
                eprintln!("[mdk-server stderr] {}", line);
            }
        });

        let client = reqwest::Client::new();

        let mut handle = Self {
            child: Some(child),
            api_port,
            p2p_port,
            storage_dir,
            http_password_full: http_password_full.to_string(),
            node_id: String::new(),
            client,
            _mock_mdk: mock_mdk,
        };

        // Poll until the API is up.
        let node_info = handle.wait_for_ready(Duration::from_secs(60)).await;
        handle.node_id = node_info["nodeId"].as_str().unwrap().to_string();

        let _ = webhook_port; // reserved for future use in config
        handle
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.api_port)
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{}", self.base_url(), path))
            .basic_auth("", Some(&self.http_password_full))
            .send()
            .await
            .unwrap()
    }

    pub async fn post(&self, path: &str, body: &serde_json::Value) -> reqwest::Response {
        self.client
            .post(format!("{}{}", self.base_url(), path))
            .basic_auth("", Some(&self.http_password_full))
            .json(body)
            .send()
            .await
            .unwrap()
    }

    pub async fn post_form(&self, path: &str, params: &[(&str, &str)]) -> reqwest::Response {
        self.client
            .post(format!("{}{}", self.base_url(), path))
            .basic_auth("", Some(&self.http_password_full))
            .form(params)
            .send()
            .await
            .unwrap()
    }

    async fn wait_for_ready(&self, timeout: Duration) -> serde_json::Value {
        let start = std::time::Instant::now();
        loop {
            let result = self
                .client
                .get(format!("{}/v1/node", self.base_url()))
                .basic_auth("", Some(&self.http_password_full))
                .send()
                .await;

            if let Ok(resp) = result {
                if let Ok(info) = resp.json::<serde_json::Value>().await {
                    if info.get("nodeId").is_some() {
                        return info;
                    }
                }
            }

            if start.elapsed() > timeout {
                panic!("Timed out waiting for mdk-server to become ready");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

impl Drop for MdkServerHandle {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

// ---------------------------------------------------------------------------
// PayerNode — raw ldk_node for paying invoices to mdk-server
// ---------------------------------------------------------------------------

pub struct PayerNode {
    pub node: Arc<Node>,
    pub p2p_port: u16,
    _storage_dir: PathBuf,
}

impl PayerNode {
    pub fn new(bitcoind: &TestBitcoind) -> Self {
        #[allow(deprecated)]
        let storage_dir = tempfile::tempdir().unwrap().into_path();
        let p2p_port = find_available_port();

        let config = LdkNodeConfig {
            network: Network::Regtest,
            storage_dir_path: storage_dir.to_str().unwrap().to_string(),
            listening_addresses: Some(vec![SocketAddress::from_str(&format!(
                "127.0.0.1:{p2p_port}"
            ))
            .unwrap()]),
            ..Default::default()
        };

        let mut builder = Builder::from_config(config);
        let (rpc_host, rpc_port, rpc_user, rpc_password) = bitcoind.rpc_details();
        builder.set_chain_source_bitcoind_rpc(rpc_host, rpc_port, rpc_user, rpc_password);

        let seed_path = storage_dir.join("keys_seed").to_str().unwrap().to_string();
        builder.set_entropy_seed_path(seed_path);

        let node = Arc::new(builder.build().unwrap());
        node.start().unwrap();

        Self {
            node,
            p2p_port,
            _storage_dir: storage_dir,
        }
    }

    pub fn node_id(&self) -> String {
        self.node.node_id().to_string()
    }

    pub fn onchain_address(&self) -> String {
        self.node
            .onchain_payment()
            .new_address()
            .unwrap()
            .to_string()
    }

    pub fn pay_invoice(&self, invoice_str: &str) {
        let invoice = Bolt11Invoice::from_str(invoice_str).unwrap();
        self.node.bolt11_payment().send(&invoice, None).unwrap();
    }

    pub fn open_channel(&self, node_id: &str, addr: &str, amount_sats: u64) {
        use ldk_server::ldk_node::bitcoin::secp256k1::PublicKey;
        let pubkey = PublicKey::from_str(node_id).unwrap();
        let socket_addr = SocketAddress::from_str(addr).unwrap();
        self.node
            .open_channel(pubkey, socket_addr, amount_sats, None, None)
            .unwrap();
    }

    pub fn list_channels_usable(&self) -> bool {
        self.node.list_channels().iter().any(|c| c.is_usable)
    }

    pub fn sync_wallets(&self) {
        self.node.sync_wallets().unwrap();
    }
}

impl Drop for PayerNode {
    fn drop(&mut self) {
        let _ = self.node.stop();
    }
}

// ---------------------------------------------------------------------------
// LspNode — LSPS4 liquidity provider for JIT channel tests
// ---------------------------------------------------------------------------

pub struct LspNode {
    pub node: Arc<LspLdkNode>,
    pub p2p_port: u16,
    _storage_dir: PathBuf,
}

impl LspNode {
    pub fn new(bitcoind: &TestBitcoind) -> Self {
        #[allow(deprecated)]
        let storage_dir = tempfile::tempdir().unwrap().into_path();
        let p2p_port = find_available_port();

        let config = LspNodeConfig {
            network: ldk_node_lsp::bitcoin::Network::Regtest,
            storage_dir_path: storage_dir.to_str().unwrap().to_string(),
            listening_addresses: Some(vec![LspSocketAddress::from_str(&format!(
                "127.0.0.1:{p2p_port}"
            ))
            .unwrap()]),
            ..Default::default()
        };

        let mut builder = LspBuilder::from_config(config);
        let (rpc_host, rpc_port, rpc_user, rpc_password) = bitcoind.rpc_details();
        builder.set_chain_source_bitcoind_rpc(rpc_host, rpc_port, rpc_user, rpc_password);

        let seed_path = storage_dir.join("keys_seed").to_str().unwrap().to_string();
        builder.set_entropy_seed_path(seed_path);

        builder.set_liquidity_provider_lsps4(LSPS4ServiceConfig {
            min_channel_size_msat: 50_000_000,
            channel_over_provisioning_ppm: 500_000,
            forwarding_fee_proportional_millionths: 20_000,
            channel_size_tiers: vec![],
        });

        let node = Arc::new(builder.build().unwrap());
        node.start().unwrap();

        Self {
            node,
            p2p_port,
            _storage_dir: storage_dir,
        }
    }

    pub fn node_id(&self) -> String {
        self.node.node_id().to_string()
    }

    pub fn onchain_address(&self) -> String {
        self.node
            .onchain_payment()
            .new_address()
            .unwrap()
            .to_string()
    }

    pub fn sync_wallets(&self) {
        self.node.sync_wallets().unwrap();
    }
}

impl Drop for LspNode {
    fn drop(&mut self) {
        let _ = self.node.stop();
    }
}

// ---------------------------------------------------------------------------
// MockMdkApi — minimal oRPC mock for moneydevkit.com checkout endpoints
// ---------------------------------------------------------------------------

pub struct MockMdkApi {
    pub port: u16,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl MockMdkApi {
    pub async fn start() -> Self {
        let port = find_available_port();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let app = axum::Router::new()
            .route(
                "/rpc/checkout/create",
                axum::routing::post(mock_create_checkout),
            )
            .route(
                "/rpc/checkout/registerInvoice",
                axum::routing::post(mock_register_invoice),
            )
            .route(
                "/rpc/checkout/paymentReceived",
                axum::routing::post(mock_payment_received),
            );

        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        Self {
            port,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/rpc", self.port)
    }
}

impl Drop for MockMdkApi {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

static CHECKOUT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

async fn mock_create_checkout(
    axum::Json(body): axum::Json<serde_json::Value>,
) -> axum::Json<serde_json::Value> {
    let seq = CHECKOUT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let amount = body
        .get("json")
        .and_then(|j| j.get("amount"))
        .and_then(|a| a.as_u64());

    axum::Json(serde_json::json!({
        "json": {
            "id": format!("chk_test_{seq}"),
            "status": "pending",
            "invoiceAmountSats": amount,
            "invoiceScid": null
        }
    }))
}

async fn mock_register_invoice() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "json": {
            "id": "chk_test_registered",
            "status": "invoice_registered",
            "invoiceAmountSats": null,
            "invoiceScid": null
        }
    }))
}

async fn mock_payment_received() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "json": {
            "ok": true
        }
    }))
}

// ---------------------------------------------------------------------------
// WebhookReceiver — captures POST requests from mdk-server
// ---------------------------------------------------------------------------

pub struct WebhookReceiver {
    pub port: u16,
    pub payloads: Arc<Mutex<Vec<serde_json::Value>>>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl WebhookReceiver {
    pub async fn start() -> Self {
        let port = find_available_port();
        let payloads: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let payloads_clone = Arc::clone(&payloads);

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let store = Arc::clone(&payloads_clone);
                async move {
                    store.lock().unwrap().push(body);
                    axum::http::StatusCode::OK
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        Self {
            port,
            payloads,
            shutdown_tx: Some(shutdown_tx),
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/hook", self.port)
    }

    pub fn received(&self) -> Vec<serde_json::Value> {
        self.payloads.lock().unwrap().clone()
    }
}

impl Drop for WebhookReceiver {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

// ---------------------------------------------------------------------------
// Channel setup helpers
// ---------------------------------------------------------------------------

/// Fund the LSP on-chain and wait for balance to be confirmed.
pub async fn fund_lsp(bitcoind: &TestBitcoind, lsp: &LspNode) {
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
}

/// Fund payer and open a confirmed channel from payer to the LSP.
pub async fn setup_payer_lsp_channel(
    bitcoind: &TestBitcoind,
    payer: &PayerNode,
    lsp: &LspNode,
    channel_amount_sats: u64,
) {
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

    payer.open_channel(
        &lsp.node_id(),
        &format!("127.0.0.1:{}", lsp.p2p_port),
        channel_amount_sats,
    );
    bitcoind.mine_blocks(6);

    let start = std::time::Instant::now();
    loop {
        payer.sync_wallets();
        if payer.list_channels_usable() {
            break;
        }
        if start.elapsed() > Duration::from_secs(60) {
            let channels: Vec<_> = payer
                .node
                .list_channels()
                .iter()
                .map(|c| {
                    format!(
                        "id={} ready={} usable={} value={}",
                        c.channel_id, c.is_channel_ready, c.is_usable, c.channel_value_sats
                    )
                })
                .collect();
            panic!(
                "Timed out waiting for payer->LSP channel. Channels: {:?}",
                channels
            );
        }
        bitcoind.mine_blocks(1);
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

pub fn find_available_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

