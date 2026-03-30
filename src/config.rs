use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

use ldk_node::bitcoin::Network;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning::routing::gossip::NodeAlias;
use log::LevelFilter;
use serde::Deserialize;

#[derive(Deserialize)]
struct TomlConfig {
    node: Option<NodeSection>,
    storage: Option<StorageSection>,
    log: Option<LogSection>,
}

#[derive(Deserialize)]
struct NodeSection {
    network: Option<Network>,
    listening_addresses: Option<Vec<String>>,
    announcement_addresses: Option<Vec<String>>,
    rest_service_address: Option<String>,
    alias: Option<String>,
    pathfinding_scores_source_url: Option<String>,
}

#[derive(Deserialize)]
struct StorageSection {
    disk: Option<DiskSection>,
}

#[derive(Deserialize)]
struct DiskSection {
    dir_path: Option<String>,
}

#[derive(Deserialize)]
struct LogSection {
    level: Option<String>,
    #[allow(dead_code)]
    file: Option<String>,
}

pub struct MdkConfig {
    pub network: Network,
    pub listening_addrs: Option<Vec<SocketAddress>>,
    pub announcement_addrs: Option<Vec<SocketAddress>>,
    pub rest_service_addr: SocketAddr,
    pub alias: Option<NodeAlias>,
    pub storage_dir_path: Option<String>,
    pub log_level: LevelFilter,
    pub pathfinding_scores_source_url: Option<String>,
}

pub fn load_config(path: &str) -> io::Result<MdkConfig> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("Failed to read config file '{}': {}", path, e),
        )
    })?;

    let toml: TomlConfig = toml::from_str(&content).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid TOML in '{}': {}", path, e),
        )
    })?;

    let node = toml.node.ok_or_else(|| missing("node"))?;

    let network = node.network.ok_or_else(|| missing("node.network"))?;

    let rest_service_addr = node
        .rest_service_address
        .ok_or_else(|| missing("node.rest_service_address"))?
        .parse::<SocketAddr>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let listening_addrs = node
        .listening_addresses
        .map(|addrs| parse_socket_addresses(&addrs, "listening_addresses"))
        .transpose()?;

    let announcement_addrs = node
        .announcement_addresses
        .map(|addrs| parse_socket_addresses(&addrs, "announcement_addresses"))
        .transpose()?;

    let alias = node.alias.map(|s| parse_alias(&s)).transpose()?;

    let storage_dir_path = toml.storage.and_then(|s| s.disk).and_then(|d| d.dir_path);

    let log_level = match toml.log {
        Some(log) => log
            .level
            .map(|s| {
                LevelFilter::from_str(&s).map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Invalid log level: {e}"),
                    )
                })
            })
            .transpose()?
            .unwrap_or(LevelFilter::Debug),
        None => LevelFilter::Debug,
    };

    Ok(MdkConfig {
        network,
        listening_addrs,
        announcement_addrs,
        rest_service_addr,
        alias,
        storage_dir_path,
        log_level,
        pathfinding_scores_source_url: node.pathfinding_scores_source_url,
    })
}

pub fn get_default_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        #[allow(deprecated)]
        std::env::home_dir().map(|home| home.join("Library/Application Support/mdkd"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|appdata| PathBuf::from(appdata).join("mdkd"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        #[allow(deprecated)]
        std::env::home_dir().map(|home| home.join(".mdkd"))
    }
}

pub struct LspInfra {
    pub chain_source: ChainSource,
    pub lsp_node_id: &'static str,
    pub lsp_address: &'static str,
    pub mdk_api_base_url: &'static str,
    pub vss_url: &'static str,
}

impl LspInfra {
    pub fn for_network(network: Network) -> Option<Self> {
        match network {
            Network::Bitcoin => Some(LspInfra {
                chain_source: ChainSource::Esplora("https://esplora.moneydevkit.com/api"),
                lsp_node_id: "02a63339cc6b913b6330bd61b2f469af8785a6011a6305bb102298a8e76697473b",
                lsp_address: "lsp.moneydevkit.com:9735",
                mdk_api_base_url: "https://moneydevkit.com/rpc",
                vss_url: "https://vss.moneydevkit.com/vss",
            }),
            Network::Signet => Some(LspInfra {
                chain_source: ChainSource::Esplora("https://mutinynet.com/api"),
                lsp_node_id: "03fd9a377576df94cc7e458471c43c400630655083dee89df66c6ad38d1b7acffd",
                lsp_address: "lsp.staging.moneydevkit.com:9735",
                mdk_api_base_url: "https://staging.moneydevkit.com/rpc",
                vss_url: "https://vss.staging.moneydevkit.com/vss",
            }),
            _ => None,
        }
    }
}

pub enum ChainSource {
    Esplora(&'static str),
    Bitcoind {
        rpc_host: String,
        rpc_port: u16,
        rpc_user: String,
        rpc_password: String,
    },
}

pub enum NetworkInfra {
    Production(LspInfra),
    Regtest {
        chain_source: ChainSource,
        lsp_node_id: String,
        lsp_address: String,
        mdk_api_base_url: String,
        vss_url: String,
    },
}

impl NetworkInfra {
    pub fn resolve(network: Network) -> io::Result<Self> {
        match LspInfra::for_network(network) {
            Some(infra) => Ok(NetworkInfra::Production(infra)),
            None => {
                let rpc_port_str = env_required("MDK_BITCOIND_RPC_PORT")?;
                let rpc_port: u16 = rpc_port_str.parse().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("MDK_BITCOIND_RPC_PORT is not a valid port: {rpc_port_str}"),
                    )
                })?;
                Ok(NetworkInfra::Regtest {
                    chain_source: ChainSource::Bitcoind {
                        rpc_host: env_required("MDK_BITCOIND_RPC_HOST")?,
                        rpc_port,
                        rpc_user: env_required("MDK_BITCOIND_RPC_USER")?,
                        rpc_password: env_required("MDK_BITCOIND_RPC_PASSWORD")?,
                    },
                    lsp_node_id: env_required("MDK_LSP_NODE_ID")?,
                    lsp_address: env_required("MDK_LSP_ADDRESS")?,
                    mdk_api_base_url: env_required("MDK_API_BASE_URL")?,
                    vss_url: env_required("MDK_VSS_URL")?,
                })
            }
        }
    }

    pub fn chain_source(&self) -> &ChainSource {
        match self {
            NetworkInfra::Production(lsp_infra) => &lsp_infra.chain_source,
            NetworkInfra::Regtest { chain_source, .. } => chain_source,
        }
    }

    pub fn lsp_node_id(&self) -> &str {
        match self {
            NetworkInfra::Production(lsp_infra) => lsp_infra.lsp_node_id,
            NetworkInfra::Regtest { lsp_node_id, .. } => lsp_node_id,
        }
    }

    pub fn lsp_address(&self) -> &str {
        match self {
            NetworkInfra::Production(lsp_infra) => lsp_infra.lsp_address,
            NetworkInfra::Regtest { lsp_address, .. } => lsp_address,
        }
    }

    pub fn mdk_api_base_url(&self) -> &str {
        match self {
            NetworkInfra::Production(lsp_infra) => lsp_infra.mdk_api_base_url,
            NetworkInfra::Regtest {
                mdk_api_base_url, ..
            } => mdk_api_base_url,
        }
    }

    pub fn vss_url(&self) -> &str {
        match self {
            NetworkInfra::Production(lsp_infra) => lsp_infra.vss_url,
            NetworkInfra::Regtest { vss_url, .. } => vss_url,
        }
    }
}

fn env_required(name: &str) -> io::Result<String> {
    std::env::var(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} environment variable is required for regtest"),
        )
    })
}

fn missing(field: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("Missing required config field: {field}"),
    )
}

fn parse_socket_addresses(addrs: &[String], field: &str) -> io::Result<Vec<SocketAddress>> {
    addrs
        .iter()
        .map(|a| {
            SocketAddress::from_str(a).map_err(|e| {
                io::Error::new(io::ErrorKind::InvalidInput, format!("Invalid {field}: {e}"))
            })
        })
        .collect()
}

fn parse_alias(alias_str: &str) -> io::Result<NodeAlias> {
    let mut bytes = [0u8; 32];
    let alias_bytes = alias_str.trim().as_bytes();
    if alias_bytes.len() > 32 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "node.alias must be at most 32 bytes",
        ));
    }
    bytes[..alias_bytes.len()].copy_from_slice(alias_bytes);
    Ok(NodeAlias(bytes))
}
