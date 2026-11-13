use iroh::EndpointId;
use serde_json::Value;

use super::{AreaEvent, EventFields};
use crate::config::ConfigError;
use crate::crypto::SignedPayloadKind;
use crate::handle::LocalHandle;
use crate::net::gossip::GossipError;
use crate::payload::{PayloadDropRule, PayloadFacts, PayloadStamp};
use crate::wire;

const SHORT_ID_BYTES: usize = 5;
const PEER_FIELD: &str = "peer";
const KIND_FIELD: &str = "kind";
const HANDLE_FIELD: &str = "handle";
const SEQ_FIELD: &str = "seq";
const TS_FIELD: &str = "ts";
const RULE_FIELD: &str = "rule";

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
        PayloadAccepted => "gossip.payload_accepted",
        PayloadDropped => "gossip.payload_dropped",
        PayloadBroadcast => "gossip.payload_broadcast",
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

/// A received payload as the events file names it: its kind, its signer's five-byte id, the handle the table holds for the signer if any, and its stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedPayload {
    kind: SignedPayloadKind,
    signer: ShortId,
    signer_handle: Option<LocalHandle>,
    stamp: PayloadStamp,
}

impl From<(PayloadFacts, Option<LocalHandle>)> for ReceivedPayload {
    fn from((facts, signer_handle): (PayloadFacts, Option<LocalHandle>)) -> Self {
        Self {
            kind: facts.kind,
            signer: ShortId::from(&facts.signer),
            signer_handle,
            stamp: facts.stamp,
        }
    }
}

impl ReceivedPayload {
    /// The payload's fields in the events file: its kind, its signer, the signer's handle when the table holds one, and its stamp.
    fn fields(&self) -> EventFields {
        let mut fields = vec![
            (KIND_FIELD, Value::from(kind_name(self.kind))),
            (PEER_FIELD, self.signer.field()),
        ];
        fields.extend(
            self.signer_handle
                .as_ref()
                .map(|handle| (HANDLE_FIELD, Value::from(handle.as_str()))),
        );
        fields.push(stamp_field(self.stamp));
        fields
    }
}

/// A payload this node broadcast as the events file names it: its kind and its stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentPayload {
    kind: SignedPayloadKind,
    stamp: PayloadStamp,
}

impl From<PayloadFacts> for SentPayload {
    fn from(facts: PayloadFacts) -> Self {
        Self {
            kind: facts.kind,
            stamp: facts.stamp,
        }
    }
}

/// The name the events file gives a payload kind.
const fn kind_name(kind: SignedPayloadKind) -> &'static str {
    match kind {
        SignedPayloadKind::Presence => "presence",
        SignedPayloadKind::Departure => "departure",
        SignedPayloadKind::Unreachable => "unreachable",
        SignedPayloadKind::Infrastructure => "infrastructure",
        SignedPayloadKind::PresenceRequest => "presence_request",
    }
}

/// The name the events file gives a drop rule.
const fn rule_name(rule: PayloadDropRule) -> &'static str {
    match rule {
        PayloadDropRule::FromOwnCard => "from_own_card",
        PayloadDropRule::OlderThanMaxAge => "older_than_max_age",
        PayloadDropRule::NotNewerThanHeld => "not_newer_than_held",
        PayloadDropRule::NotNewerThanDeparture => "not_newer_than_departure",
        PayloadDropRule::HandleOutsideGrammar => "handle_outside_grammar",
        PayloadDropRule::ReportOlderThanHeldSeq => "report_older_than_held_seq",
        PayloadDropRule::ReportBeforeDirectContact => "report_before_direct_contact",
        PayloadDropRule::ReportedCardUnknown => "reported_card_unknown",
        PayloadDropRule::ReportedCardGone => "reported_card_gone",
        PayloadDropRule::ReportOlderThanLastPresence => "report_older_than_last_presence",
        PayloadDropRule::NotAboveRequesterSeq => "not_above_requester_seq",
        PayloadDropRule::AheadOfMaxAge => "ahead_of_max_age",
        PayloadDropRule::RecordNotNewerThanHeld => "record_not_newer_than_held",
        PayloadDropRule::RecordPastFutureBound => "record_past_future_bound",
        PayloadDropRule::PastWireLimit => "past_wire_limit",
        PayloadDropRule::TrailingBytes => "trailing_bytes",
        PayloadDropRule::Unencodable => "unencodable",
        PayloadDropRule::Malformed => "malformed",
        PayloadDropRule::UnsupportedVersion => "unsupported_version",
        PayloadDropRule::RelayUrlTooLong => "relay_url_too_long",
        PayloadDropRule::AddrOverCap => "addr_over_cap",
        PayloadDropRule::MacMismatch => "mac_mismatch",
        PayloadDropRule::SignatureMismatch => "signature_mismatch",
        PayloadDropRule::HostNotCertified => "host_not_certified",
        PayloadDropRule::HostRelayRefused => "host_relay_refused",
        PayloadDropRule::AddrRelaysNotTheHostRelay => "addr_relays_not_the_host_relay",
        PayloadDropRule::PublicNotAdmitted => "public_not_admitted",
    }
}

/// The stamp as the events file writes it: a seq or a ts.
fn stamp_field(stamp: PayloadStamp) -> (&'static str, Value) {
    match stamp {
        PayloadStamp::Seq(seq) => (SEQ_FIELD, Value::from(seq)),
        PayloadStamp::Ts(ts) => (TS_FIELD, Value::from(ts)),
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
    /// The node took a payload it received.
    PayloadAccepted(ReceivedPayload),
    /// The node dropped a payload it received by the rule named, telling what the payload said of itself when it decoded.
    PayloadDropped {
        payload: Option<ReceivedPayload>,
        rule: PayloadDropRule,
    },
    /// The node broadcast a payload on the topic.
    PayloadBroadcast(SentPayload),
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
            Self::PayloadAccepted(_) => GossipEventName::PayloadAccepted,
            Self::PayloadDropped { .. } => GossipEventName::PayloadDropped,
            Self::PayloadBroadcast(_) => GossipEventName::PayloadBroadcast,
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
            Self::TopicJoined
            | Self::NeighborUp(_)
            | Self::NeighborDown(_)
            | Self::PayloadAccepted(_)
            | Self::PayloadDropped { .. }
            | Self::PayloadBroadcast(_) => None,
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
            Self::PayloadAccepted(payload) => payload.fields(),
            Self::PayloadDropped { payload, rule } => {
                let mut fields = payload
                    .as_ref()
                    .map_or_else(Vec::new, ReceivedPayload::fields);
                fields.push((RULE_FIELD, Value::from(rule_name(*rule))));
                fields
            }
            Self::PayloadBroadcast(sent) => vec![
                (KIND_FIELD, Value::from(kind_name(sent.kind))),
                stamp_field(sent.stamp),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, write_run_record};

    const PEER_ID: wire::PublicKey = [7; 32];
    const STAMP: u64 = 1_800_000_000_000;

    fn facts() -> PayloadFacts {
        PayloadFacts {
            kind: SignedPayloadKind::Presence,
            signer: PEER_ID,
            stamp: PayloadStamp::Seq(STAMP),
        }
    }

    #[test]
    fn observability_decision_3_the_topic_and_payload_events_add_no_line_to_standard_error() {
        let events: [Event; 6] = [
            GossipEvent::TopicJoined.into(),
            GossipEvent::NeighborUp(ShortId::from(&PEER_ID)).into(),
            GossipEvent::NeighborDown(ShortId::from(&PEER_ID)).into(),
            GossipEvent::PayloadAccepted((facts(), None).into()).into(),
            GossipEvent::PayloadDropped {
                payload: None,
                rule: PayloadDropRule::Malformed,
            }
            .into(),
            GossipEvent::PayloadBroadcast(facts().into()).into(),
        ];

        for event in events {
            let mut standard_error = Vec::new();
            write_run_record(&mut standard_error, &event).unwrap();
            assert_eq!(
                String::from_utf8(standard_error).unwrap(),
                "",
                "{}",
                event.name()
            );
        }
    }
}
