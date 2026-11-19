use data_encoding::HEXLOWER;
use iroh::{EndpointId, SecretKey};
use serde_json::Value;

use crate::trace_reader::TraceLine;

/// The event a member records for each gossip payload it drops, and the fields naming the payload's kind, signer and seq (spec/observability.md decisions 6 and 9).
pub const PAYLOAD_DROPPED_EVENT: &str = "gossip.payload_dropped";
const KIND_FIELD: &str = "kind";
const PEER_FIELD: &str = "peer";
const SEQ_FIELD: &str = "seq";
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
