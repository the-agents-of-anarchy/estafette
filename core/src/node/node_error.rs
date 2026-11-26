use std::fmt;

use crate::config::AcceptedValue;
use crate::keys::KeyError;
use crate::net::gossip::GossipError;

/// Why the node did not start.
#[derive(Debug)]
pub enum NodeError {
    NetworkKeyInvalid {
        field: &'static str,
        source: KeyError,
    },
    KnownHostInvalid {
        id: String,
    },
    Topic(GossipError),
    TopicClosedBeforeHandle,
}

impl fmt::Display for NodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NetworkKeyInvalid { field, source } => write!(
                formatter,
                "the network configuration's {field} is refused: {source}"
            ),
            Self::KnownHostInvalid { id } => write!(
                formatter,
                "known host {id} needs a node id of {} and a relay of {}",
                AcceptedValue::HexKey,
                AcceptedValue::RelayUrl
            ),
            Self::Topic(source) => {
                write!(formatter, "cannot subscribe to the gossip topic: {source}")
            }
            Self::TopicClosedBeforeHandle => formatter
                .write_str("the gossip topic closed before this node assigned its own handle"),
        }
    }
}

impl std::error::Error for NodeError {}
