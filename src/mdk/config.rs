use std::io;

use ldk_node::bitcoin::Network;

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
