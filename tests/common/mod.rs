use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hex::DisplayHex;
use ldk_server::ldk_node::bitcoin::Network;
use ldk_server::ldk_node::config::Config as LdkNodeConfig;
use ldk_server::ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_server::ldk_node::lightning_invoice::Bolt11Invoice;
use ldk_server::ldk_node::{Builder, Node};

// ---------------------------------------------------------------------------
// TestBitcoind
// ---------------------------------------------------------------------------

pub struct TestBitcoind {
	pub bitcoind: corepc_node::Node,
}

impl TestBitcoind {
	pub fn new() -> Self {
		let bitcoind = match std::env::var("BITCOIND_EXE") {
			Ok(path) => corepc_node::Node::new(path).unwrap(),
			Err(_) => corepc_node::Node::from_downloaded().unwrap(),
		};
		let address = bitcoind.client.new_address().unwrap();
		bitcoind.client.generate_to_address(101, &address).unwrap();
		Self { bitcoind }
	}

	pub fn mine_blocks(&self, count: u64) {
		let address = self.bitcoind.client.new_address().unwrap();
		self.bitcoind.client.generate_to_address(count as usize, &address).unwrap();
	}

	pub fn fund_address(&self, addr: &str, btc_amount: f64) {
		use corepc_node::client::bitcoin::{Address, Amount};
		let address: Address<corepc_node::client::bitcoin::address::NetworkUnchecked> =
			addr.parse().unwrap();
		let address = address.assume_checked();
		let amount = Amount::from_btc(btc_amount).unwrap();
		self.bitcoind.client.send_to_address(&address, amount).unwrap();
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
	pub api_key: String,
	pub node_id: String,
	client: reqwest::Client,
}

impl MdkServerHandle {
	pub async fn start(bitcoind: &TestBitcoind, webhook_port: Option<u16>) -> Self {
		#[allow(deprecated)]
		let storage_dir = tempfile::tempdir().unwrap().into_path();

		let api_port = find_available_port();
		let p2p_port = find_available_port();
		let rest_port = find_available_port(); // dummy, never bound by mdk-server

		let (rpc_host, rpc_port, rpc_user, rpc_password) = bitcoind.rpc_details();
		let rpc_address = format!("{rpc_host}:{rpc_port}");

		let webhook_secret = "aa".repeat(32); // 32 bytes as hex

		let config = format!(
			r#"[node]
network = "regtest"
listening_addresses = ["127.0.0.1:{p2p_port}"]
rest_service_address = "127.0.0.1:{rest_port}"

[storage.disk]
dir_path = "{storage_dir}"

[bitcoind]
rpc_address = "{rpc_address}"
rpc_user = "{rpc_user}"
rpc_password = "{rpc_password}"

[mdk]
api_address = "127.0.0.1:{api_port}"
webhook_secret = "{webhook_secret}"
"#,
			storage_dir = storage_dir.display(),
		);

		let config_path = storage_dir.join("config.toml");
		std::fs::write(&config_path, &config).unwrap();

		let binary = env!("CARGO_BIN_EXE_mdk-server");
		let mut child = Command::new(binary)
			.arg(config_path.to_str().unwrap())
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

		let network_dir = storage_dir.join("regtest");
		let api_key_path = network_dir.join("api_key");
		wait_for_file(&api_key_path, Duration::from_secs(30)).await;

		let api_key_bytes = std::fs::read(&api_key_path).unwrap();
		let api_key = api_key_bytes.to_lower_hex_string();

		let client = reqwest::Client::new();

		let mut handle = Self {
			child: Some(child),
			api_port,
			p2p_port,
			storage_dir,
			api_key,
			node_id: String::new(),
			client,
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
			.header("Authorization", format!("Bearer {}", self.api_key))
			.send()
			.await
			.unwrap()
	}

	pub async fn post(&self, path: &str, body: &serde_json::Value) -> reqwest::Response {
		self.client
			.post(format!("{}{}", self.base_url(), path))
			.header("Authorization", format!("Bearer {}", self.api_key))
			.json(body)
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
				.header("Authorization", format!("Bearer {}", self.api_key))
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
			listening_addresses: Some(vec![
				SocketAddress::from_str(&format!("127.0.0.1:{p2p_port}")).unwrap()
			]),
			..Default::default()
		};

		let mut builder = Builder::from_config(config);
		let (rpc_host, rpc_port, rpc_user, rpc_password) = bitcoind.rpc_details();
		builder.set_chain_source_bitcoind_rpc(rpc_host, rpc_port, rpc_user, rpc_password);

		let seed_path = storage_dir.join("keys_seed").to_str().unwrap().to_string();
		builder.set_entropy_seed_path(seed_path);

		let node = Arc::new(builder.build().unwrap());
		node.start().unwrap();

		Self { node, p2p_port, _storage_dir: storage_dir }
	}

	pub fn node_id(&self) -> String {
		self.node.node_id().to_string()
	}

	pub fn onchain_address(&self) -> String {
		self.node.onchain_payment().new_address().unwrap().to_string()
	}

	pub fn pay_invoice(&self, invoice_str: &str) {
		let invoice = Bolt11Invoice::from_str(invoice_str).unwrap();
		self.node.bolt11_payment().send(&invoice, None).unwrap();
	}

	pub fn open_channel(&self, node_id: &str, addr: &str, amount_sats: u64) {
		use ldk_server::ldk_node::bitcoin::secp256k1::PublicKey;
		let pubkey = PublicKey::from_str(node_id).unwrap();
		let socket_addr = SocketAddress::from_str(addr).unwrap();
		self.node.open_channel(pubkey, socket_addr, amount_sats, None, None).unwrap();
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
			axum::routing::post(
				move |axum::Json(body): axum::Json<serde_json::Value>| {
					let store = Arc::clone(&payloads_clone);
					async move {
						store.lock().unwrap().push(body);
						axum::http::StatusCode::OK
					}
				},
			),
		);

		let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}"))
			.await
			.unwrap();

		tokio::spawn(async move {
			axum::serve(listener, app)
				.with_graceful_shutdown(async { let _ = shutdown_rx.await; })
				.await
				.unwrap();
		});

		Self { port, payloads, shutdown_tx: Some(shutdown_tx) }
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
// Channel setup helper
// ---------------------------------------------------------------------------

pub async fn setup_funded_channel(
	bitcoind: &TestBitcoind, payer: &PayerNode, server: &MdkServerHandle,
	channel_amount_sats: u64,
) {
	// Fund payer on-chain. The channel opener (payer) pays on-chain fees.
	// mdk-server doesn't need separate funding — it gets anchor reserves
	// from the channel open's change output.
	let payer_addr = payer.onchain_address();
	bitcoind.fund_address(&payer_addr, 1.0);
	bitcoind.mine_blocks(6);

	// Wait for payer to sync.
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

	// Open channel payer -> mdk-server.
	let server_addr_str = format!("127.0.0.1:{}", server.p2p_port);
	payer.open_channel(&server.node_id, &server_addr_str, channel_amount_sats);

	// Mine to confirm.
	bitcoind.mine_blocks(6);

	// Wait for channel to become usable on payer side.
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
			panic!("Timed out waiting for usable channel. Channels: {:?}", channels);
		}
		bitcoind.mine_blocks(1);
		tokio::time::sleep(Duration::from_secs(1)).await;
	}
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

pub fn find_available_port() -> u16 {
	TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn wait_for_file(path: &Path, timeout: Duration) {
	let start = std::time::Instant::now();
	while !path.exists() {
		if start.elapsed() > timeout {
			panic!("Timed out waiting for file: {:?}", path);
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
}
