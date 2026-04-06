use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::str::FromStr;
use std::sync::Arc;

use ldk_node::bip39::Mnemonic;
use ldk_node::bitcoin::hashes::sha256;
use ldk_node::bitcoin::hashes::Hash;
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::bitcoin::Network;
use ldk_node::config::Config as LdkNodeConfig;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::{Builder, Node};
use log::info;

use crate::mdk::config::{ChainSource, NetworkInfra};
use crate::mdk::error::MdkError;

pub struct NodeConfig {
    pub network: Network,
    pub storage_dir_path: String,
    pub listening_addresses: Option<Vec<SocketAddress>>,
    pub announcement_addresses: Option<Vec<SocketAddress>>,
    pub alias: Option<String>,
    pub socks_proxy: Option<String>,
    pub pathfinding_scores_source_url: Option<String>,
    pub mnemonic: String,
    pub infra: NetworkInfra,
    pub runtime: tokio::runtime::Handle,
}

pub fn build_node(config: NodeConfig) -> Result<Arc<Node>, MdkError> {
    let ldk_config = LdkNodeConfig {
        storage_dir_path: config.storage_dir_path.clone(),
        listening_addresses: config.listening_addresses,
        announcement_addresses: config.announcement_addresses,
        network: config.network,
        ..Default::default()
    };

    let mut builder = Builder::from_config(ldk_config);
    builder.set_log_facade_logger();

    if let Some(ref proxy_url) = config.socks_proxy {
        let addr = resolve_socks_proxy(proxy_url)?;
        builder.set_socks5_proxy(addr);
        info!("SOCKS5 proxy enabled: {}", proxy_url);
    }

    if let Some(alias) = config.alias {
        builder
            .set_node_alias(alias.to_string())
            .map_err(|e| MdkError::InvalidInput(format!("invalid node alias: {e}")))?;
    }

    let infra = config.infra;

    match &infra.chain_source {
        ChainSource::Esplora(server_url) => {
            builder.set_chain_source_esplora(server_url.clone(), None);
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

    if let Some(url) = config.pathfinding_scores_source_url {
        builder.set_pathfinding_scores_source(url);
    }

    let lsp_pubkey = PublicKey::from_str(&infra.lsp_node_id)
        .map_err(|e| MdkError::InvalidInput(format!("bad lsp_node_id: {e}")))?;
    let lsp_addr = SocketAddress::from_str(&infra.lsp_address)
        .map_err(|e| MdkError::InvalidInput(format!("bad lsp_address: {e}")))?;
    builder.set_liquidity_source_lsps4(lsp_pubkey, lsp_addr);
    info!("LSPS4 liquidity source: {}", infra.lsp_node_id);

    builder.set_runtime(config.runtime);

    let mnemonic = Mnemonic::parse(&config.mnemonic)
        .map_err(|e| MdkError::InvalidInput(format!("invalid mnemonic: {e}")))?;
    builder.set_entropy_bip39_mnemonic(mnemonic.clone(), None);

    let store_id = derive_vss_identifier(&mnemonic);
    info!(
        "VSS store: {} (store_id={}...)",
        infra.vss_url,
        &store_id[..16]
    );

    let node =
        builder.build_with_vss_store_and_fixed_headers(infra.vss_url, store_id, HashMap::new())?;

    Ok(Arc::new(node))
}

fn resolve_socks_proxy(raw: &str) -> Result<std::net::SocketAddr, MdkError> {
    let host_port = raw
        .strip_prefix("socks5://")
        .or_else(|| raw.strip_prefix("socks5h://"))
        .ok_or_else(|| {
            MdkError::InvalidInput(
                "SOCKS5 proxy url must start with socks5:// or socks5h://".into(),
            )
        })?;

    host_port
        .to_socket_addrs()
        .ok()
        .and_then(|mut addrs| addrs.next())
        .ok_or_else(|| MdkError::InvalidInput(format!("cannot resolve SOCKS5 proxy: {host_port}")))
}

pub fn derive_vss_identifier(mnemonic: &Mnemonic) -> String {
    let phrase = mnemonic.to_string();
    sha256::Hash::hash(phrase.as_bytes()).to_string()
}
