use std::net::SocketAddr;
use std::{fs, io};

use hex::FromHex;
use serde::Deserialize;

#[derive(Debug)]
pub struct MdkConfig {
    pub api_address: SocketAddr,
    pub webhook_secret: Vec<u8>,
    pub lsp_node_id: String,
    pub lsp_address: String,
    pub mdk_access_token: String,
    pub mdk_api_base_url: Option<String>,
}

#[derive(Deserialize)]
struct MdkTomlRoot {
    mdk: Option<MdkSection>,
}

#[derive(Deserialize)]
struct MdkSection {
    api_address: Option<String>,
    webhook_secret: Option<String>,
    lsp_node_id: Option<String>,
    lsp_address: Option<String>,
    mdk_access_token: Option<String>,
    mdk_api_base_url: Option<String>,
}

pub fn load_mdk_config(config_path: &str) -> io::Result<MdkConfig> {
    let content = fs::read_to_string(config_path)?;
    let root: MdkTomlRoot = toml::from_str(&content).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to parse config: {}", e),
        )
    })?;

    let section = root.mdk.unwrap_or(MdkSection {
        api_address: None,
        webhook_secret: None,
        lsp_node_id: None,
        lsp_address: None,
        mdk_access_token: None,
        mdk_api_base_url: None,
    });

    let api_address = section
        .api_address
        .unwrap_or_else(|| "127.0.0.1:8080".to_string())
        .parse::<SocketAddr>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

    let webhook_secret = match section.webhook_secret {
        Some(hex_str) => Vec::<u8>::from_hex(&hex_str).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid webhook_secret hex: {}", e),
            )
        })?,
        None => {
            let mut secret = vec![0u8; 32];
            getrandom::getrandom(&mut secret).map_err(|e| {
                io::Error::other(format!("Failed to generate webhook secret: {}", e))
            })?;
            secret
        }
    };

    let lsp_node_id = section
        .lsp_node_id
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "lsp_node_id is required"))?;
    let lsp_address = section
        .lsp_address
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "lsp_address is required"))?;

    let mdk_access_token = section.mdk_access_token.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "mdk_access_token is required")
    })?;

    Ok(MdkConfig {
        api_address,
        webhook_secret,
        lsp_node_id,
        lsp_address,
        mdk_access_token,
        mdk_api_base_url: section.mdk_api_base_url,
    })
}
