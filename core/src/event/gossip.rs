use iroh::EndpointId;
use serde_json::Value;

use super::{AreaEvent, EventFields};
use crate::config::ConfigError;
use crate::net::gossip::GossipError;
use crate::wire;

const SHORT_ID_BYTES: usize = 5;
const PEER_FIELD: &str = "peer";

super::event_names! {
    /// The names of the gossip events.
    GossipEventName {
        BroadcastFailed => "gossip.broadcast_failed",
        TopicFailed => "gossip.topic_failed",
        TopicClosed => "gossip.topic_closed",
        KnownHostsNotSaved => "gossip.known_hosts_not_saved",
        TopicJoined => "gossip.topic_joined",
        NeighborUp => "gossip.neighbor_up",
        NeighborDown => "gossip.neighbor_down",
    }
}

/// The first five bytes of a card or node id, the form iroh's own event lines name a node by (spec/observability.md decision 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortId([u8; SHORT_ID_BYTES]);

impl From<&wire::PublicKey> for ShortId {
    fn from(id: &wire::PublicKey) -> Self {
        let [first, second, third, fourth, fifth, ..] = *id;
        Self([first, second, third, fourth, fifth])
    }
}

impl From<EndpointId> for ShortId {
    fn from(id: EndpointId) -> Self {
        Self::from(id.as_bytes())
    }
}

impl ShortId {
    /// The id as the events file writes it: ten lowercase hex characters.
    fn field(self) -> Value {
        Value::from(data_encoding::HEXLOWER.encode(&self.0))
    }
}

/// What a node records of the gossip topic, its neighbors and the host records it learns from it.
pub enum GossipEvent {
    /// A payload this node broadcast did not reach the topic.
    BroadcastFailed(GossipError),
    /// The topic failed while the node read it.
    TopicFailed(GossipError),
    /// The topic closed, so the node announces nothing more.
    TopicClosed,
    /// The host records learned from the topic were not saved to the network configuration.
    KnownHostsNotSaved(ConfigError),
    /// The node joined the topic: it gained its first neighbor.
    TopicJoined,
    /// The topic made the node a neighbor.
    NeighborUp(ShortId),
    /// The topic took a neighbor away.
    NeighborDown(ShortId),
}

impl AreaEvent for GossipEvent {
    type Name = GossipEventName;

    fn name(&self) -> GossipEventName {
        match self {
            Self::BroadcastFailed(_) => GossipEventName::BroadcastFailed,
            Self::TopicFailed(_) => GossipEventName::TopicFailed,
            Self::TopicClosed => GossipEventName::TopicClosed,
            Self::KnownHostsNotSaved(_) => GossipEventName::KnownHostsNotSaved,
            Self::TopicJoined => GossipEventName::TopicJoined,
            Self::NeighborUp(_) => GossipEventName::NeighborUp,
            Self::NeighborDown(_) => GossipEventName::NeighborDown,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        match self {
            Self::BroadcastFailed(source) => Some(format!("a gossip broadcast failed: {source}")),
            Self::KnownHostsNotSaved(source) => {
                Some(format!("the known hosts were not saved: {source}"))
            }
            Self::TopicFailed(source) => Some(format!("the gossip topic failed: {source}")),
            Self::TopicClosed => {
                Some("the gossip topic closed, so this node announces nothing more".to_owned())
            }
            Self::TopicJoined | Self::NeighborUp(_) | Self::NeighborDown(_) => None,
        }
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::BroadcastFailed(_)
            | Self::TopicFailed(_)
            | Self::TopicClosed
            | Self::KnownHostsNotSaved(_)
            | Self::TopicJoined => Vec::new(),
            Self::NeighborUp(peer) | Self::NeighborDown(peer) => vec![(PEER_FIELD, peer.field())],
        }
    }
}
