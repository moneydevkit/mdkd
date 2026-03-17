mod api;
mod config;
mod event_loop;
mod store;
mod types;
mod webhook;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use hex::DisplayHex;
use ldk_server::ldk_node::config::Config as LdkNodeConfig;
use ldk_server::ldk_node::Builder;
use ldk_server::io::persist::paginated_kv_store::PaginatedKVStore;
use ldk_server::io::persist::sqlite_store::SqliteStore;
use ldk_server::util::config::{get_default_data_dir, load_config, ChainSource};
use ldk_server::util::logger::ServerLogger;
use log::{debug, error, info};
use tokio::signal::unix::SignalKind;

use crate::api::AppState;
use crate::config::load_mdk_config;
use crate::store::invoice_metadata::InvoiceMetadataStore;

const API_KEY_FILE: &str = "api_key";

#[derive(Parser)]
#[command(version, about = "MDK Server")]
struct Args {
	#[arg(help = "Path to config.toml")]
	config_file: String,
}

fn main() {
	let args = Args::parse();

	// Build ldk-server's ArgsConfig to reuse their config loading.
	// We pass the config file path as a positional arg.
	let ldk_args = ldk_server::util::config::ArgsConfig::parse_from(&[
		"mdk-server",
		&args.config_file,
	]);

	let config_file = match load_config(&ldk_args) {
		Ok(config) => config,
		Err(e) => {
			eprintln!("Invalid configuration: {e}");
			std::process::exit(1);
		},
	};

	let mdk_config = match load_mdk_config(&args.config_file) {
		Ok(c) => c,
		Err(e) => {
			eprintln!("Invalid [mdk] configuration: {e}");
			std::process::exit(1);
		},
	};

	let storage_dir: PathBuf = match config_file.storage_dir_path {
		None => match get_default_data_dir() {
			Some(path) => {
				info!("No storage_dir_path configured, defaulting to {}", path.display());
				path
			},
			None => {
				eprintln!("Unable to determine home directory for default storage path.");
				std::process::exit(1);
			},
		},
		Some(configured_path) => PathBuf::from(configured_path),
	};

	let network_dir: PathBuf = storage_dir.join(format!("{}", config_file.network));

	let log_file_path = config_file
		.log_file_path
		.map(PathBuf::from)
		.unwrap_or_else(|| network_dir.join("mdk-server.log"));

	let logger = match ServerLogger::init(config_file.log_level, &log_file_path) {
		Ok(logger) => logger,
		Err(e) => {
			eprintln!("Failed to initialize logger: {e}");
			std::process::exit(1);
		},
	};

	let api_key = match load_or_generate_api_key(&network_dir) {
		Ok(key) => key,
		Err(e) => {
			eprintln!("Failed to load or generate API key: {e}");
			std::process::exit(1);
		},
	};

	let mut ldk_node_config = LdkNodeConfig::default();
	ldk_node_config.storage_dir_path = network_dir.to_str().unwrap().to_string();
	ldk_node_config.listening_addresses = config_file.listening_addrs;
	ldk_node_config.announcement_addresses = config_file.announcement_addrs;
	ldk_node_config.network = config_file.network;

	let mut builder = Builder::from_config(ldk_node_config);
	builder.set_log_facade_logger();

	if let Some(alias) = config_file.alias {
		if let Err(e) = builder.set_node_alias(alias.to_string()) {
			error!("Failed to set node alias: {e}");
			std::process::exit(1);
		}
	}

	match config_file.chain_source {
		ChainSource::Rpc { rpc_host, rpc_port, rpc_user, rpc_password } => {
			builder.set_chain_source_bitcoind_rpc(rpc_host, rpc_port, rpc_user, rpc_password);
		},
		ChainSource::Electrum { server_url } => {
			builder.set_chain_source_electrum(server_url, None);
		},
		ChainSource::Esplora { server_url } => {
			builder.set_chain_source_esplora(server_url, None);
		},
	}

	if let Some(url) = config_file.pathfinding_scores_source_url {
		builder.set_pathfinding_scores_source(url);
	}

	let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
		Ok(runtime) => Arc::new(runtime),
		Err(e) => {
			error!("Failed to setup tokio runtime: {e}");
			std::process::exit(1);
		},
	};

	builder.set_runtime(runtime.handle().clone());

	let seed_path = storage_dir.join("keys_seed").to_str().unwrap().to_string();
	builder.set_entropy_seed_path(seed_path);

	let node = match builder.build() {
		Ok(node) => Arc::new(node),
		Err(e) => {
			error!("Failed to build LDK Node: {e}");
			std::process::exit(1);
		},
	};

	let paginated_store: Arc<dyn PaginatedKVStore> =
		Arc::new(match SqliteStore::new(network_dir.clone(), None, None) {
			Ok(store) => store,
			Err(e) => {
				error!("Failed to create SqliteStore: {e:?}");
				std::process::exit(1);
			},
		});

	// Open metadata store in the same SQLite DB file.
	let db_path = network_dir.join("ldk_server_data.sqlite");
	let metadata_store = match InvoiceMetadataStore::new(&db_path) {
		Ok(store) => Arc::new(store),
		Err(e) => {
			error!("Failed to create InvoiceMetadataStore: {e}");
			std::process::exit(1);
		},
	};

	info!("Starting up...");
	match node.start() {
		Ok(()) => {},
		Err(e) => {
			error!("Failed to start LDK Node: {e}");
			std::process::exit(1);
		},
	}

	let addrs = node
		.config()
		.announcement_addresses
		.filter(|a| !a.is_empty())
		.or(node.config().listening_addresses);
	if let Some(addresses) = addrs {
		for address in &addresses {
			info!("NODE_URI: {}@{}", node.node_id(), address);
		}
	}

	info!("NODE_ID: {}", node.node_id());

	runtime.block_on(async {
		let mut sighup_stream = match tokio::signal::unix::signal(SignalKind::hangup()) {
			Ok(stream) => stream,
			Err(e) => {
				error!("Failed to register SIGHUP handler: {e}");
				std::process::exit(1);
			},
		};

		let mut sigterm_stream = match tokio::signal::unix::signal(SignalKind::terminate()) {
			Ok(stream) => stream,
			Err(e) => {
				error!("Failed to register SIGTERM handler: {e}");
				std::process::exit(1);
			},
		};

		let http_client = reqwest::Client::new();

		let app_state = AppState {
			node: Arc::clone(&node),
			metadata_store: Arc::clone(&metadata_store),
			api_key: api_key.clone(),
		};

		let app = api::router(app_state);
		let listener = match tokio::net::TcpListener::bind(mdk_config.api_address).await {
			Ok(l) => l,
			Err(e) => {
				error!("Failed to bind API listener on {}: {e}", mdk_config.api_address);
				std::process::exit(1);
			},
		};

		info!("MDK API listening on {}", mdk_config.api_address);

		let event_node = Arc::clone(&node);
		let event_store = Arc::clone(&paginated_store);
		let event_metadata = Arc::clone(&metadata_store);
		let event_secret = mdk_config.webhook_secret.clone();
		let event_client = http_client.clone();

		tokio::spawn(async move {
			event_loop::run_event_loop(
				event_node,
				event_store,
				event_metadata,
				event_secret,
				event_client,
			)
			.await;
		});

		tokio::spawn(async move {
			if let Err(e) = axum::serve(listener, app).await {
				error!("API server error: {e}");
			}
		});

		// Wait for shutdown signal.
		tokio::select! {
			_ = tokio::signal::ctrl_c() => {
				info!("Received CTRL-C, shutting down...");
			}
			_ = sigterm_stream.recv() => {
				info!("Received SIGTERM, shutting down...");
			}
			_ = sighup_stream.recv() => {
				if let Err(e) = logger.reopen() {
					error!("Failed to reopen log file on SIGHUP: {e}");
				}
			}
		}
	});

	node.stop().expect("Shutdown should always succeed.");
	info!("Shutdown complete.");
}

fn load_or_generate_api_key(storage_dir: &Path) -> std::io::Result<String> {
	let api_key_path = storage_dir.join(API_KEY_FILE);

	if api_key_path.exists() {
		let key_bytes = fs::read(&api_key_path)?;
		Ok(key_bytes.to_lower_hex_string())
	} else {
		fs::create_dir_all(storage_dir)?;
		let mut key_bytes = [0u8; 32];
		getrandom::getrandom(&mut key_bytes).map_err(std::io::Error::other)?;
		fs::write(&api_key_path, key_bytes)?;
		let permissions = fs::Permissions::from_mode(0o400);
		fs::set_permissions(&api_key_path, permissions)?;
		debug!("Generated new API key at {}", api_key_path.display());
		Ok(key_bytes.to_lower_hex_string())
	}
}
