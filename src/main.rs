mod api;
mod config;
mod event_loop;
mod expiry;
mod logger;
mod mdk;
mod secret;
mod store;
mod time;
mod types;
mod webhook;

use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

use clap::Parser;
use hex::FromHex;
use ldk_node::bip39::Mnemonic;
use ldk_node::bitcoin::hashes::sha256;
use ldk_node::bitcoin::hashes::Hash;
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::config::Config as LdkNodeConfig;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::Builder;
use log::{error, info};
use reqwest::{Client, Proxy};
use tokio::signal::unix::SignalKind;
use tokio::sync::broadcast;

use crate::api::{AppState, HttpAuth};
use crate::config::{get_default_data_dir, load_config, ChainSource, NetworkInfra};
use crate::mdk::client::MdkApiClient;
use crate::store::invoice_metadata::InvoiceMetadataStore;

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
        secret::try_resolve(name, fd).unwrap_or_else(|e| {
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

    logger::init(config_file.log_level);

    // Optional SOCKS5 proxy for all outbound traffic.
    let socks_proxy_url = args.socks_proxy;
    let socks_proxy_addr = socks_proxy_url.as_ref().map(|raw| {
        let host_port = raw
            .strip_prefix("socks5://")
            .or_else(|| raw.strip_prefix("socks5h://"))
            .unwrap_or_else(|| {
                eprintln!("SOCKS5 proxy url must start with socks5:// or socks5h://");
                std::process::exit(1);
            });
        host_port
            .to_socket_addrs()
            .ok()
            .and_then(|mut addrs| addrs.next())
            .unwrap_or_else(|| {
                eprintln!("cannot resolve SOCKS5 proxy {}", host_port);
                std::process::exit(1);
            })
    });

    if let Some(ref url) = socks_proxy_url {
        info!("SOCKS5 proxy enabled: {}", url);
    }

    let ldk_node_config = LdkNodeConfig {
        storage_dir_path: network_dir.to_str().unwrap().to_string(),
        listening_addresses: config_file.listening_addrs,
        announcement_addresses: config_file.announcement_addrs,
        network: config_file.network,
        ..Default::default()
    };

    let mut builder = Builder::from_config(ldk_node_config);
    builder.set_log_facade_logger();

    if let Some(addr) = socks_proxy_addr {
        builder.set_socks5_proxy(addr);
    }

    if let Some(alias) = config_file.alias {
        if let Err(e) = builder.set_node_alias(alias.to_string()) {
            error!("Failed to set node alias: {e}");
            std::process::exit(1);
        }
    }

    match infra.chain_source() {
        ChainSource::Esplora(server_url) => {
            builder.set_chain_source_esplora(server_url.to_string(), None);
        }
        ChainSource::Bitcoind {
            rpc_host,
            rpc_port,
            rpc_user,
            rpc_password,
        } => {
            builder.set_chain_source_bitcoind_rpc(
                rpc_host.clone(),
                *rpc_port,
                rpc_user.clone(),
                rpc_password.clone(),
            );
        }
    }

    if let Some(url) = config_file.pathfinding_scores_source_url {
        builder.set_pathfinding_scores_source(url);
    }

    let lsp_pubkey = PublicKey::from_str(infra.lsp_node_id()).unwrap_or_else(|e| {
        error!("Bad lsp_node_id: {e}");
        std::process::exit(1);
    });
    let lsp_addr = SocketAddress::from_str(infra.lsp_address()).unwrap_or_else(|e| {
        error!("Bad lsp_address: {e}");
        std::process::exit(1);
    });
    builder.set_liquidity_source_lsps4(lsp_pubkey, lsp_addr);
    info!("LSPS4 liquidity source: {}", infra.lsp_node_id());

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

    builder.set_runtime(runtime.handle().clone());

    let mnemonic = Mnemonic::parse(&mnemonic_phrase).unwrap_or_else(|e| {
        error!("Invalid MDK_MNEMONIC: {e}");
        std::process::exit(1);
    });
    builder.set_entropy_bip39_mnemonic(mnemonic.clone(), None);

    let store_id = derive_vss_identifier(&mnemonic);
    info!(
        "VSS store: {} (store_id={}...)",
        infra.vss_url(),
        &store_id[..16]
    );

    let node = match builder.build_with_vss_store_and_fixed_headers(
        infra.vss_url().to_string(),
        store_id,
        HashMap::new(),
    ) {
        Ok(node) => Arc::new(node),
        Err(e) => {
            error!("Failed to build LDK Node: {e}");
            std::process::exit(1);
        }
    };

    let db_path = network_dir.join("mdkd.sqlite");
    let metadata_store = match InvoiceMetadataStore::new(&db_path) {
        Ok(store) => Arc::new(store),
        Err(e) => {
            error!("Failed to create InvoiceMetadataStore: {e}");
            std::process::exit(1);
        }
    };

    let http_client = {
        let mut b = Client::builder();
        if let Some(ref proxy_url) = socks_proxy_url {
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

    let base_url = infra.mdk_api_base_url().to_string();
    info!("MDK platform integration enabled ({})", base_url);
    let mdk_client = Arc::new(MdkApiClient::new(
        http_client.clone(),
        base_url,
        mdk_access_token,
    ));

    info!("Starting up...");
    match node.start() {
        Ok(()) => {}
        Err(e) => {
            error!("Failed to start LDK Node: {e}");
            std::process::exit(1);
        }
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

    let bind_addr = config_file.rest_service_addr;

    runtime.block_on(async {
        let mut sigterm_stream = match tokio::signal::unix::signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(e) => {
                error!("Failed to register SIGTERM handler: {e}");
                std::process::exit(1);
            }
        };

        let (event_tx, _) = broadcast::channel::<String>(128);

        let app_state = AppState {
            node: Arc::clone(&node),
            metadata_store: Arc::clone(&metadata_store),
            http_auth: HttpAuth {
                full_password: full_password.clone(),
                read_only_password: read_only_password.clone(),
            },
            mdk_client: mdk_client.clone(),
            event_tx: event_tx.clone(),
        };

        let app = api::router(app_state);

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
            expiry::run_expiry_monitor(expiry_metadata, expiry_secret, expiry_client).await;
        });

        let event_node = Arc::clone(&node);
        let event_metadata = Arc::clone(&metadata_store);
        let event_secret = webhook_secret;
        let event_client = http_client.clone();
        let event_mdk_client = mdk_client.clone();

        tokio::spawn(async move {
            event_loop::run_event_loop(
                event_node,
                event_metadata,
                event_secret,
                event_client,
                event_mdk_client,
                event_tx,
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

    node.stop().expect("Shutdown should always succeed.");
    info!("Shutdown complete.");
}

fn derive_vss_identifier(mnemonic: &Mnemonic) -> String {
    let mnemonic_phrase = mnemonic.to_string();
    sha256::Hash::hash(mnemonic_phrase.as_bytes()).to_string()
}
