use data_encoding::HEXLOWER;
use iroh::{EndpointId, SecretKey};
use serde_json::Value;

use crate::trace_reader::TraceLine;

/// The event a member records for each gossip payload it drops (spec/observability.md decisions 6 and 9).
pub const PAYLOAD_DROPPED_EVENT: &str = "gossip.payload_dropped";
/// The event a member records for each gossip payload it accepts (spec/observability.md decision 9).
pub const PAYLOAD_ACCEPTED_EVENT: &str = "gossip.payload_accepted";
/// The event a member records for each direct request it sends, with how it ended (spec/observability.md decision 9).
pub const REQUEST_SENT_EVENT: &str = "direct.request_sent";
/// The field of an events line naming a payload's or a request's kind (spec/observability.md decision 6).
pub const KIND_FIELD: &str = "kind";
/// The field of an events line naming a payload's signer or a request's peer (spec/observability.md decision 6).
pub const PEER_FIELD: &str = "peer";
/// The field of an events line naming a peer by the handle the member holds for it (spec/observability.md decision 6).
pub const HANDLE_FIELD: &str = "handle";
/// The field of an events line naming a payload's seq (spec/observability.md decision 6).
pub const SEQ_FIELD: &str = "seq";
/// The field of an events line naming a Message's id (spec/observability.md decision 6).
pub const MESSAGE_FIELD: &str = "message";
/// The field of an events line naming a request's outcome (spec/observability.md decision 6).
pub const OUTCOME_FIELD: &str = "outcome";
/// The kind of payload a Presence is, as an events line names it.
pub const PRESENCE_KIND: &str = "presence";
/// The kind of direct request a Message is, as an events line names it.
pub const MESSAGE_KIND: &str = "message";
/// The kind of direct request an Introduce is, as an events line names it.
pub const INTRODUCE_KIND: &str = "introduce";
/// The outcome of a request its recipient acknowledged, as an events line names it.
pub const ACKNOWLEDGED_OUTCOME: &str = "acknowledged";
/// How many leading bytes of its node id name a peer in an events line (spec/observability.md decision 6).
const SPEC_SHORT_ID_BYTES: usize = 5;

/// The key's node id as an events line names a peer.
pub fn short_id_text(key: &SecretKey) -> String {
    short_id_of_node(key.public())
}

/// The node id as an events line names a peer.
pub fn short_id_of_node(node: EndpointId) -> String {
    HEXLOWER.encode(&node.as_bytes()[..SPEC_SHORT_ID_BYTES])
}

/// Whether the events line records a payload of the kind whose signer is the peer dropped.
pub fn is_dropped_payload_of(line: &TraceLine, kind: &str, peer: &str) -> bool {
    line.event == PAYLOAD_DROPPED_EVENT
        && line.value(KIND_FIELD) == Some(&Value::from(kind))
        && line.value(PEER_FIELD) == Some(&Value::from(peer))
}

/// Whether the events line records the payload of the kind, signer and seq dropped, that one payload and no other its signer sent.
pub fn is_dropped_payload_of_seq(line: &TraceLine, kind: &str, peer: &str, seq: u64) -> bool {
    is_dropped_payload_of(line, kind, peer) && line.value(SEQ_FIELD) == Some(&Value::from(seq))
}

/// Whether the events line records a payload of the kind whose signer is the peer accepted.
pub fn is_accepted_payload_of(line: &TraceLine, kind: &str, peer: &str) -> bool {
    line.event == PAYLOAD_ACCEPTED_EVENT
        && line.value(KIND_FIELD) == Some(&Value::from(kind))
        && line.value(PEER_FIELD) == Some(&Value::from(peer))
}

/// Whether the events line records a request of the kind sent and acknowledged by its recipient.
pub fn is_acknowledged_request_of_kind(line: &TraceLine, kind: &str) -> bool {
    line.event == REQUEST_SENT_EVENT
        && line.value(KIND_FIELD) == Some(&Value::from(kind))
        && line.value(OUTCOME_FIELD) == Some(&Value::from(ACKNOWLEDGED_OUTCOME))
}
