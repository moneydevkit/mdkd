use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use ldk_node::bip39::Mnemonic;
use ldk_node::bitcoin::hashes::sha256;
use ldk_node::bitcoin::hashes::Hash;
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::bitcoin::Network;
use ldk_node::config::Config as LdkNodeConfig;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning::routing::scoring::{
    ProbabilisticScoringDecayParameters, ProbabilisticScoringFeeParameters,
};
use ldk_node::{Builder, Node, ProbabilisticScoringParameters};
use log::{info, warn};

use crate::mdk::config::{ChainSource, NetworkInfra};
use crate::mdk::error::MdkError;
use crate::mdk::max_sendable::MaxSendableConfig;

pub struct NodeConfig {
    pub network: Network,
    pub storage_dir_path: String,
    pub listening_addresses: Option<Vec<SocketAddress>>,
    pub announcement_addresses: Option<Vec<SocketAddress>>,
    pub alias: Option<String>,
    pub socks_proxy: Option<String>,
    pub pathfinding_scores_source_url: Option<String>,
    pub fee_claim: Option<String>,
    pub mnemonic: String,
    pub infra: NetworkInfra,
    pub scoring_overrides: ScoringOverrides,
    pub splice: SpliceConfig,
    pub max_sendable: MaxSendableConfig,
}

/// Per-field overrides for the probabilistic scorer's fee parameters.
///
/// Only applied on mainnet; ignored elsewhere (with a warning if non-empty).
/// Decay parameters and `manual_node_penalties` are not user-tunable.
#[derive(Debug, Default, Clone)]
pub struct ScoringOverrides {
    pub base_penalty_msat: Option<u64>,
    pub base_penalty_amount_multiplier_msat: Option<u64>,
    pub liquidity_penalty_multiplier_msat: Option<u64>,
    pub liquidity_penalty_amount_multiplier_msat: Option<u64>,
    pub historical_liquidity_penalty_multiplier_msat: Option<u64>,
    pub historical_liquidity_penalty_amount_multiplier_msat: Option<u64>,
    pub anti_probing_penalty_msat: Option<u64>,
    pub considered_impossible_penalty_msat: Option<u64>,
    pub linear_success_probability: Option<bool>,
    pub probing_diversity_penalty_msat: Option<u64>,
}

impl ScoringOverrides {
    pub fn is_empty(&self) -> bool {
        let Self {
            base_penalty_msat,
            base_penalty_amount_multiplier_msat,
            liquidity_penalty_multiplier_msat,
            liquidity_penalty_amount_multiplier_msat,
            historical_liquidity_penalty_multiplier_msat,
            historical_liquidity_penalty_amount_multiplier_msat,
            anti_probing_penalty_msat,
            considered_impossible_penalty_msat,
            linear_success_probability,
            probing_diversity_penalty_msat,
        } = self;
        base_penalty_msat.is_none()
            && base_penalty_amount_multiplier_msat.is_none()
            && liquidity_penalty_multiplier_msat.is_none()
            && liquidity_penalty_amount_multiplier_msat.is_none()
            && historical_liquidity_penalty_multiplier_msat.is_none()
            && historical_liquidity_penalty_amount_multiplier_msat.is_none()
            && anti_probing_penalty_msat.is_none()
            && considered_impossible_penalty_msat.is_none()
            && linear_success_probability.is_none()
            && probing_diversity_penalty_msat.is_none()
    }
}

/// Configuration for the auto-splice manager. The manager wakes up
/// every `poll_interval`, reads the spendable on-chain balance, and
/// splices it into an existing LSP channel when one is available.
#[derive(Debug, Clone)]
pub struct SpliceConfig {
    pub enabled: bool,
    pub poll_interval: Duration,
}

impl Default for SpliceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval: Duration::from_secs(30),
        }
    }
}

pub fn build_node(
    config: NodeConfig,
    runtime: tokio::runtime::Handle,
) -> Result<Arc<Node>, MdkError> {
    let ldk_config = LdkNodeConfig {
        storage_dir_path: config.storage_dir_path.clone(),
        listening_addresses: config.listening_addresses,
        announcement_addresses: config.announcement_addresses,
        network: config.network,
        // Don't advertise anchor channel support: anchors require a
        // 25_000 sat on-chain reserve per channel for force-close fee
        // bumping, which mdkd clients (one channel to the LSP) should not have in general.
        anchor_channels_config: None,
        ..Default::default()
    };

    let mut builder = Builder::from_config(ldk_config);
    builder.set_log_facade_logger();

    if config.network == Network::Bitcoin {
        builder.set_scoring_params(resolve_scoring_params(&config.scoring_overrides));
    } else if !config.scoring_overrides.is_empty() {
        warn!(
            "[node.scoring] overrides are mainnet-only; ignoring on {}",
            config.network
        );
    }

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
    builder.set_liquidity_source_lsps4(lsp_pubkey, lsp_addr, config.fee_claim);
    info!("LSPS4 liquidity source: {}", infra.lsp_node_id);

    builder.set_runtime(runtime);

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

/// Mainnet scoring parameters with user overrides applied on top.
///
/// Baseline biases pathfinding toward fewer hops by raising `base_penalty_msat`
/// to 100x the LDK default (1024 → 102_400 msat). All other fields are pinned
/// to upstream LDK defaults so any future drift in ldk-node's defaults does
/// not silently change routing behavior on us. Each field set in `overrides`
/// replaces the corresponding baseline value.
fn resolve_scoring_params(overrides: &ScoringOverrides) -> ProbabilisticScoringParameters {
    let mut params = ProbabilisticScoringParameters {
        fee_params: ProbabilisticScoringFeeParameters {
            base_penalty_msat: 102_400,
            base_penalty_amount_multiplier_msat: 131_072,
            liquidity_penalty_multiplier_msat: 0,
            liquidity_penalty_amount_multiplier_msat: 0,
            historical_liquidity_penalty_multiplier_msat: 10_000,
            historical_liquidity_penalty_amount_multiplier_msat: 1_250,
            manual_node_penalties: Default::default(),
            anti_probing_penalty_msat: 250,
            considered_impossible_penalty_msat: 100_000_000_000,
            linear_success_probability: false,
            probing_diversity_penalty_msat: 0,
        },
        decay_params: ProbabilisticScoringDecayParameters::default(),
    };
    let f = &mut params.fee_params;
    if let Some(v) = overrides.base_penalty_msat {
        f.base_penalty_msat = v;
    }
    if let Some(v) = overrides.base_penalty_amount_multiplier_msat {
        f.base_penalty_amount_multiplier_msat = v;
    }
    if let Some(v) = overrides.liquidity_penalty_multiplier_msat {
        f.liquidity_penalty_multiplier_msat = v;
    }
    if let Some(v) = overrides.liquidity_penalty_amount_multiplier_msat {
        f.liquidity_penalty_amount_multiplier_msat = v;
    }
    if let Some(v) = overrides.historical_liquidity_penalty_multiplier_msat {
        f.historical_liquidity_penalty_multiplier_msat = v;
    }
    if let Some(v) = overrides.historical_liquidity_penalty_amount_multiplier_msat {
        f.historical_liquidity_penalty_amount_multiplier_msat = v;
    }
    if let Some(v) = overrides.anti_probing_penalty_msat {
        f.anti_probing_penalty_msat = v;
    }
    if let Some(v) = overrides.considered_impossible_penalty_msat {
        f.considered_impossible_penalty_msat = v;
    }
    if let Some(v) = overrides.linear_success_probability {
        f.linear_success_probability = v;
    }
    if let Some(v) = overrides.probing_diversity_penalty_msat {
        f.probing_diversity_penalty_msat = v;
    }
    params
}

pub fn derive_vss_identifier(mnemonic: &Mnemonic) -> String {
    let phrase = mnemonic.to_string();
    sha256::Hash::hash(phrase.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainnet_default_uses_100x_base_penalty() {
        let params = resolve_scoring_params(&ScoringOverrides::default());
        assert_eq!(params.fee_params.base_penalty_msat, 102_400);
        assert_eq!(params.fee_params.anti_probing_penalty_msat, 250);
        assert_eq!(
            params.fee_params.considered_impossible_penalty_msat,
            100_000_000_000
        );
    }

    #[test]
    fn override_replaces_only_specified_fields() {
        let overrides = ScoringOverrides {
            base_penalty_msat: Some(7),
            linear_success_probability: Some(true),
            ..Default::default()
        };
        let params = resolve_scoring_params(&overrides);
        // Overridden fields take new values.
        assert_eq!(params.fee_params.base_penalty_msat, 7);
        assert!(params.fee_params.linear_success_probability);
        // Untouched fields keep the mainnet default.
        assert_eq!(
            params.fee_params.base_penalty_amount_multiplier_msat,
            131_072
        );
        assert_eq!(params.fee_params.anti_probing_penalty_msat, 250);
    }

    #[test]
    fn is_empty_distinguishes_default_from_any_override() {
        assert!(ScoringOverrides::default().is_empty());
        assert!(!ScoringOverrides {
            base_penalty_msat: Some(0),
            ..Default::default()
        }
        .is_empty());
        assert!(!ScoringOverrides {
            linear_success_probability: Some(false),
            ..Default::default()
        }
        .is_empty());
    }
}
