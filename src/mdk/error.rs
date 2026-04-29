use std::fmt;

use crate::mdk::mdk_api::types::MdkApiError;

#[derive(Debug)]
pub enum MdkError {
    InvalidInput(String),
    Node(String),
    Platform {
        code: String,
        message: String,
        status: u16,
    },
    Network(String),
    NotFound(String),
    Splice(SpliceError),
}

/// Typed splice failure modes. Modeled as an ADT so the splice
/// manager can decide what to do (skip vs. emit failure event)
/// without inspecting log strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpliceError {
    /// The target channel exists but is not currently usable
    /// (mid-splice, peer disconnected, mid-monitor-update). The
    /// splice manager should skip this tick and try again.
    ChannelNotUsable,
    /// The on-chain wallet does not have enough confirmed funds
    /// for the requested splice amount. Retried next tick.
    InsufficientFunds,
    /// ldk-node refused the splice (coin selection failed under
    /// fee pressure, channel not yet ready, peer rejected, etc.).
    /// ldk-node currently collapses these into one error variant;
    /// we do too.
    Rejected,
}

impl fmt::Display for SpliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpliceError::ChannelNotUsable => write!(f, "channel not usable"),
            SpliceError::InsufficientFunds => write!(f, "insufficient confirmed on-chain funds"),
            SpliceError::Rejected => write!(f, "splice rejected by ldk-node"),
        }
    }
}

impl fmt::Display for MdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MdkError::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            MdkError::Node(msg) => write!(f, "node error: {msg}"),
            MdkError::Platform {
                code,
                message,
                status,
            } => write!(f, "platform API error ({status}): [{code}] {message}"),
            MdkError::Network(msg) => write!(f, "network error: {msg}"),
            MdkError::NotFound(msg) => write!(f, "not found: {msg}"),
            MdkError::Splice(e) => write!(f, "splice error: {e}"),
        }
    }
}

impl std::error::Error for MdkError {}

impl From<ldk_node::NodeError> for MdkError {
    fn from(e: ldk_node::NodeError) -> Self {
        MdkError::Node(e.to_string())
    }
}

impl From<ldk_node::BuildError> for MdkError {
    fn from(e: ldk_node::BuildError) -> Self {
        MdkError::Node(e.to_string())
    }
}

impl From<MdkApiError> for MdkError {
    fn from(e: MdkApiError) -> Self {
        match e {
            MdkApiError::Network(inner) => MdkError::Network(inner.to_string()),
            MdkApiError::Api {
                code,
                message,
                status,
            } => MdkError::Platform {
                code,
                message,
                status,
            },
            MdkApiError::Deserialize(msg) => MdkError::Network(msg),
        }
    }
}
