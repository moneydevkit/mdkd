use std::io;

use ldk_node::bitcoin::Network;

pub enum ChainSource {
    Esplora(String),
    Bitcoind {
        rpc_host: String,
        rpc_port: u16,
        rpc_user: String,
        rpc_password: String,
    },
}

pub struct NetworkInfra {
    pub chain_source: ChainSource,
    pub lsp_node_id: String,
    pub lsp_address: String,
    pub mdk_api_base_url: String,
    pub vss_url: String,
}

impl NetworkInfra {
    pub fn resolve(network: Network) -> io::Result<Self> {
        match network {
            Network::Bitcoin => Ok(Self {
                chain_source: ChainSource::Esplora("https://esplora.moneydevkit.com/api".into()),
                lsp_node_id: "02a63339cc6b913b6330bd61b2f469af8785a6011a6305bb102298a8e76697473b"
                    .into(),
                lsp_address: "lsp.moneydevkit.com:9735".into(),
                mdk_api_base_url: "https://moneydevkit.com/rpc".into(),
                vss_url: "https://vss.moneydevkit.com/vss".into(),
            }),
            Network::Signet => Ok(Self {
                chain_source: ChainSource::Esplora("https://mutinynet.com/api".into()),
                lsp_node_id: "03fd9a377576df94cc7e458471c43c400630655083dee89df66c6ad38d1b7acffd"
                    .into(),
                lsp_address: "lsp.staging.moneydevkit.com:9735".into(),
                mdk_api_base_url: "https://staging.moneydevkit.com/rpc".into(),
                vss_url: "https://vss.staging.moneydevkit.com/vss".into(),
            }),
            Network::Testnet | Network::Testnet4 => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported network: {network}"),
            )),
            _ => {
                let rpc_port_str = env_required("MDK_BITCOIND_RPC_PORT")?;
                let rpc_port: u16 = rpc_port_str.parse().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("MDK_BITCOIND_RPC_PORT is not a valid port: {rpc_port_str}"),
                    )
                })?;
                Ok(Self {
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
}

fn env_required(name: &str) -> io::Result<String> {
    std::env::var(name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} environment variable is required for regtest"),
        )
    })
}
