use super::{AreaEvent, EventFields};
use crate::config::ConfigError;
use crate::net::gossip::GossipError;

super::event_names! {
    /// The names of the gossip events.
    GossipEventName {
        BroadcastFailed => "gossip.broadcast_failed",
        TopicFailed => "gossip.topic_failed",
        TopicClosed => "gossip.topic_closed",
        KnownHostsNotSaved => "gossip.known_hosts_not_saved",
    }
}

/// What a node records of the gossip topic and of the host records it learns from it.
pub enum GossipEvent {
    /// A payload this node broadcast did not reach the topic.
    BroadcastFailed(GossipError),
    /// The topic failed while the node read it.
    TopicFailed(GossipError),
    /// The topic closed, so the node announces nothing more.
    TopicClosed,
    /// The host records learned from the topic were not saved to the network configuration.
    KnownHostsNotSaved(ConfigError),
}

impl AreaEvent for GossipEvent {
    type Name = GossipEventName;

    fn name(&self) -> GossipEventName {
        match self {
            Self::BroadcastFailed(_) => GossipEventName::BroadcastFailed,
            Self::TopicFailed(_) => GossipEventName::TopicFailed,
            Self::TopicClosed => GossipEventName::TopicClosed,
            Self::KnownHostsNotSaved(_) => GossipEventName::KnownHostsNotSaved,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        Some(match self {
            Self::BroadcastFailed(source) => format!("a gossip broadcast failed: {source}"),
            Self::KnownHostsNotSaved(source) => {
                format!("the known hosts were not saved: {source}")
            }
            Self::TopicFailed(source) => format!("the gossip topic failed: {source}"),
            Self::TopicClosed => {
                "the gossip topic closed, so this node announces nothing more".to_owned()
            }
        })
    }

    fn fields(&self) -> EventFields {
        Vec::new()
    }
}
