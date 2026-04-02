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
