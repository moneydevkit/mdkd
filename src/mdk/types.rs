use ldk_node::ChannelDetails;

/// Parameters for creating a checkout (invoice + platform registration).
pub struct CreateCheckoutParams {
    pub amount_sat: Option<u64>,
    pub description: InvoiceDescription,
    pub expiry_seconds: Option<u32>,
    pub product: Option<String>,
    pub currency: Option<String>,
    pub success_url: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub customer: Option<Customer>,
}

pub enum InvoiceDescription {
    Direct(String),
    Hash([u8; 32]),
}

pub struct Customer {
    pub name: Option<String>,
    pub email: Option<String>,
    pub external_id: Option<String>,
}

pub struct CheckoutResult {
    pub checkout_id: String,
    pub invoice: String,
    pub payment_hash: String,
    pub amount_sat: Option<u64>,
    pub expires_at: Option<u64>,
}

pub struct Balance {
    pub lightning_sats: u64,
    pub onchain_sats: u64,
}

pub struct Channel {
    pub state: ChannelState,
    pub channel_id: String,
    pub balance_sats: u64,
    pub inbound_liquidity_sats: u64,
    pub capacity_sats: u64,
    pub funding_tx_id: Option<String>,
}

pub enum ChannelState {
    Online,
    Offline,
    Opening,
}

impl From<&ChannelDetails> for Channel {
    fn from(ch: &ChannelDetails) -> Self {
        let state = match (ch.is_channel_ready, ch.is_usable) {
            (true, true) => ChannelState::Online,
            (true, false) => ChannelState::Offline,
            (false, _) => ChannelState::Opening,
        };

        Self {
            state,
            channel_id: ch.channel_id.to_string(),
            balance_sats: ch.outbound_capacity_msat / 1000,
            inbound_liquidity_sats: ch.inbound_capacity_msat / 1000,
            capacity_sats: ch.channel_value_sats,
            funding_tx_id: ch.funding_txo.map(|txo| txo.txid.to_string()),
        }
    }
}

pub struct NodeInfo {
    pub node_id: String,
    pub network: String,
    pub block_height: u32,
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone)]
pub enum MdkEvent {
    PaymentReceived {
        payment_hash: String,
        amount_sats: u64,
    },
    PaymentSuccessful {
        payment_id: String,
        payment_hash: Option<String>,
        fee_paid_sats: Option<u64>,
    },
    PaymentFailed {
        payment_id: String,
        reason: Option<String>,
    },
    ChannelPending {
        channel_id: String,
        counterparty_node_id: String,
    },
    ChannelReady {
        channel_id: String,
        counterparty_node_id: String,
    },
    PaymentForwarded {
        fee_earned_sats: Option<u64>,
    },
    /// The splice manager has called `splice_in` on a channel and
    /// ldk-node accepted the request. Funding-tx broadcast follows.
    SpliceInitiated {
        channel_id: String,
    },
    /// A splice transaction has been broadcast and is awaiting
    /// on-chain confirmation. Mirrors `ldk_node::Event::SplicePending`.
    SplicePending {
        channel_id: String,
        new_funding_txid: String,
    },
    /// A splice attempt failed — either synchronously from
    /// `splice_in` (peer offline, fee too low, etc.) or after
    /// negotiation via `ldk_node::Event::SpliceFailed`.
    SpliceFailed {
        channel_id: String,
        reason: String,
    },
}

pub struct PaymentResult {
    pub payment_id: String,
    pub payment_hash: Option<String>,
    pub fee_paid_sats: Option<u64>,
}
