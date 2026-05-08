mod daemon;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use hex::FromHex;
use log::{error, info};
use mdk::client::MdkClient;
use mdk::config::NetworkInfra;
use mdk::node::NodeConfig;
use reqwest::{Client, Proxy};
use tokio::signal::unix::SignalKind;
use tokio::sync::broadcast;

use daemon::api::{AppState, HttpAuth};
use daemon::config::{get_default_data_dir, load_config};
use daemon::store::invoice_metadata::InvoiceMetadataStore;

#[derive(Parser)]
#[command(version, about = "mdkd - MDK daemon")]
struct Args {
    /// Path to config.toml
    config_file: String,
    /// Read wallet mnemonic from this file descriptor
    #[arg(long)]
    mnemonic_fd: Option<i32>,
    /// Read webhook HMAC secret (hex) from this file descriptor
    #[arg(long)]
    webhook_secret_fd: Option<i32>,
    /// Read full-access HTTP password from this file descriptor
    #[arg(long)]
    password_full_fd: Option<i32>,
    /// Read read-only HTTP password from this file descriptor
    #[arg(long)]
    password_read_only_fd: Option<i32>,
    /// Read MDK platform access token from this file descriptor
    #[arg(long)]
    access_token_fd: Option<i32>,
    /// Route all outbound traffic through a SOCKS5 proxy (e.g. socks5://127.0.0.1:1080)
    #[arg(long)]
    socks_proxy: Option<String>,
}

fn main() {
    let args = Args::parse();

    let config_file = match load_config(&args.config_file) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Invalid configuration: {e}");
            std::process::exit(1);
        }
    };

    let infra = match NetworkInfra::resolve(config_file.network) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("Failed to resolve network infrastructure: {e}");
            std::process::exit(1);
        }
    };

    // Resolve secrets: FD flags take precedence, env vars as fallback.
    let resolve = |name, fd| {
        daemon::secret::try_resolve(name, fd).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        })
    };
    let webhook_secret = {
        let hex_str = resolve("MDK_WEBHOOK_SECRET", args.webhook_secret_fd);
        Vec::<u8>::from_hex(&hex_str).unwrap_or_else(|e| {
            eprintln!("Invalid MDK_WEBHOOK_SECRET hex: {e}");
            std::process::exit(1);
        })
    };
    let full_password = resolve("MDK_HTTP_PASSWORD_FULL", args.password_full_fd);
    let read_only_password = resolve("MDK_HTTP_PASSWORD_READ_ONLY", args.password_read_only_fd);
    let mnemonic_phrase = resolve("MDK_MNEMONIC", args.mnemonic_fd);
    let mdk_access_token = resolve("MDK_ACCESS_TOKEN", args.access_token_fd);

    let storage_dir: PathBuf = match config_file.storage_dir_path {
        None => match get_default_data_dir() {
            Some(path) => {
                info!(
                    "No storage_dir_path configured, defaulting to {}",
                    path.display()
                );
                path
            }
            None => {
                eprintln!("Unable to determine home directory for default storage path.");
                std::process::exit(1);
            }
        },
        Some(configured_path) => PathBuf::from(configured_path),
    };

    let network_dir: PathBuf = storage_dir.join(format!("{}", config_file.network));
    if let Err(e) = std::fs::create_dir_all(&network_dir) {
        eprintln!(
            "Failed to create data directory {}: {e}",
            network_dir.display()
        );
        std::process::exit(1);
    }

    daemon::logger::init(config_file.log_level);

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => Arc::new(runtime),
        Err(e) => {
            error!("Failed to setup tokio runtime: {e}");
            std::process::exit(1);
        }
    };

    let socks_proxy = args.socks_proxy;

    let node_config = NodeConfig {
        network: config_file.network,
        storage_dir_path: network_dir.to_str().unwrap().to_string(),
        listening_addresses: config_file.listening_addrs,
        announcement_addresses: config_file.announcement_addrs,
        alias: config_file.alias.map(|a| a.to_string()),
        socks_proxy: socks_proxy.clone(),
        pathfinding_scores_source_url: config_file.pathfinding_scores_source_url,
        mnemonic: mnemonic_phrase,
        infra,
        scoring_overrides: config_file.scoring_overrides,
    };

    // Separate HTTP client for daemon concerns (webhooks, expiry monitor).
    let http_client = {
        let mut b = Client::builder();
        if let Some(ref proxy_url) = socks_proxy {
            let proxy = Proxy::all(proxy_url).unwrap_or_else(|e| {
                error!("Invalid SOCKS5 proxy for reqwest: {e}");
                std::process::exit(1);
            });
            b = b.proxy(proxy);
        }
        b.build().unwrap_or_else(|e| {
            error!("Failed to build reqwest client: {e}");
            std::process::exit(1);
        })
    };

    let db_path = network_dir.join("mdkd.sqlite");
    let metadata_store = match InvoiceMetadataStore::new(&db_path) {
        Ok(store) => Arc::new(store),
        Err(e) => {
            error!("Failed to create InvoiceMetadataStore: {e}");
            std::process::exit(1);
        }
    };

    let mdk_client = match MdkClient::new(
        node_config,
        mdk_access_token,
        None,
        Some(runtime.handle().clone()),
    ) {
        Ok(client) => Arc::new(client),
        Err(e) => {
            error!("Failed to build MdkClient: {e}");
            std::process::exit(1);
        }
    };

    let node = mdk_client.node();

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

    let bind_addr = config_file.rest_service_addr;

    runtime.block_on(async {
        let mut sigterm_stream = match tokio::signal::unix::signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(e) => {
                error!("Failed to register SIGTERM handler: {e}");
                std::process::exit(1);
            }
        };

        if let Err(e) = mdk_client.start() {
            error!("Failed to start MdkClient: {e}");
            std::process::exit(1);
        }

        info!("Starting up...");

        let (ws_tx, _) = broadcast::channel::<String>(128);

        let app_state = AppState {
            node: mdk_client.node_arc(),
            metadata_store: Arc::clone(&metadata_store),
            http_auth: HttpAuth {
                full_password: full_password.clone(),
                read_only_password: read_only_password.clone(),
            },
            mdk_client: mdk_client.clone(),
            event_tx: ws_tx.clone(),
        };

        let app = daemon::api::router(app_state);

        let listener = match tokio::net::TcpListener::bind(bind_addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to bind API listener on {bind_addr}: {e}");
                std::process::exit(1);
            }
        };

        info!("MDK API listening on {bind_addr}");

        let expiry_metadata = Arc::clone(&metadata_store);
        let expiry_secret = webhook_secret.clone();
        let expiry_client = http_client.clone();

        tokio::spawn(async move {
            daemon::expiry::run_expiry_monitor(expiry_metadata, expiry_secret, expiry_client).await;
        });

        let event_mdk = Arc::clone(&mdk_client);
        let event_metadata = Arc::clone(&metadata_store);
        let event_secret = webhook_secret;
        let event_client = http_client.clone();

        tokio::spawn(async move {
            daemon::event_loop::run_event_loop(
                event_mdk,
                event_metadata,
                event_secret,
                event_client,
                ws_tx,
            )
            .await;
        });

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                error!("API server error: {e}");
            }
        });

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Received CTRL-C, shutting down...");
            }
            _ = sigterm_stream.recv() => {
                info!("Received SIGTERM, shutting down...");
            }
        }
    });

    if let Err(e) = mdk_client.stop() {
        error!("Error during shutdown: {e}");
    }
    info!("Shutdown complete.");
}
