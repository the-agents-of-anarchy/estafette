use iroh::EndpointId;
use serde_json::Value;

use super::{AreaEvent, EventFields};
use crate::net::direct::{
    AckRefusal, ConnectionSource, DirectError, RequestSummary, SentOutcome, SentRequest,
    ServedOutcome, ServedRequest, StreamEnd,
};
use crate::net::endpoint::{ConnectionId, PeerId};

const KIND_FIELD: &str = "kind";
const MESSAGE_FIELD: &str = "message";
const PEER_FIELD: &str = "peer";
const CONNECTION_FIELD: &str = "connection";
const CONNECTION_SOURCE_FIELD: &str = "connection_source";
const OUTCOME_FIELD: &str = "outcome";
const CODE_FIELD: &str = "code";
const ERROR_FIELD: &str = "error";
const ELAPSED_FIELD: &str = "elapsed_ms";

super::event_names! {
    /// The names of the direct protocol events.
    DirectEventName {
        ContactFailed => "direct.contact_failed",
        RequestSent => "direct.request_sent",
        RequestServed => "direct.request_served",
    }
}

/// What a process records of the direct requests it sends and serves.
pub enum DirectEvent {
    /// A direct request got no valid acknowledgement, naming the node it went to and why.
    ContactFailed {
        peer: EndpointId,
        reason: DirectError,
    },
    /// This node sent a direct request: its kind and message id, the peer, the connection it rode and whether that was held or dialed, its outcome and error code, and its elapsed time.
    RequestSent(SentRequest),
    /// This node served a direct request: its kind and message id once it opened, the peer, the connection, its outcome and error code, and its elapsed time.
    RequestServed(ServedRequest),
}

impl AreaEvent for DirectEvent {
    type Name = DirectEventName;

    fn name(&self) -> DirectEventName {
        match self {
            Self::ContactFailed { .. } => DirectEventName::ContactFailed,
            Self::RequestSent(_) => DirectEventName::RequestSent,
            Self::RequestServed(_) => DirectEventName::RequestServed,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        match self {
            Self::ContactFailed { .. } | Self::RequestSent(_) | Self::RequestServed(_) => None,
        }
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::ContactFailed { .. } => Vec::new(),
            Self::RequestSent(sent) => [
                request_fields(sent.request),
                vec![peer_field(sent.peer)],
                sent.connection
                    .map(|(connection, source)| {
                        vec![
                            connection_field(connection),
                            (CONNECTION_SOURCE_FIELD, Value::from(source_text(source))),
                        ]
                    })
                    .unwrap_or_default(),
                sent_outcome_fields(&sent.outcome),
                vec![(ELAPSED_FIELD, Value::from(sent.elapsed_ms))],
            ]
            .concat(),
            Self::RequestServed(served) => [
                served.request.map(request_fields).unwrap_or_default(),
                vec![peer_field(served.peer), connection_field(served.connection)],
                served_outcome_fields(served.outcome),
                vec![(ELAPSED_FIELD, Value::from(served.elapsed_ms))],
            ]
            .concat(),
        }
    }
}

/// The kind of a request and, for a Message, its id as the inbox writes it.
fn request_fields(request: RequestSummary) -> EventFields {
    match request {
        RequestSummary::Message(id) => vec![
            (KIND_FIELD, Value::from("message")),
            (
                MESSAGE_FIELD,
                Value::from(uuid::Uuid::from_bytes(id).to_string()),
            ),
        ],
        RequestSummary::Probe => vec![(KIND_FIELD, Value::from("probe"))],
        RequestSummary::Introduce => vec![(KIND_FIELD, Value::from("introduce"))],
    }
}

/// The node's five-byte id as a field.
fn peer_field(peer: PeerId) -> (&'static str, Value) {
    (PEER_FIELD, Value::from(peer.hex()))
}

/// The connection's identifier as a field.
fn connection_field(connection: ConnectionId) -> (&'static str, Value) {
    (CONNECTION_FIELD, Value::from(connection.0))
}

/// Whether a request rode a held or a dialed connection, as the events file writes it.
const fn source_text(source: ConnectionSource) -> &'static str {
    match source {
        ConnectionSource::Held => "held",
        ConnectionSource::Dialed => "dialed",
    }
}

/// The error code a Message acknowledgement refused with, as the events file writes it.
const fn refusal_text(refusal: AckRefusal) -> &'static str {
    match refusal {
        AckRefusal::TooLarge => "too_large",
        AckRefusal::Refused => "refused",
        AckRefusal::Unlisted => "unlisted",
    }
}

/// How a request this node sent ended, as fields: the outcome, and the refusal's code or the failure.
fn sent_outcome_fields(outcome: &SentOutcome) -> EventFields {
    match outcome {
        SentOutcome::Acknowledged => vec![(OUTCOME_FIELD, Value::from("acknowledged"))],
        SentOutcome::Refused(refusal) => vec![
            (OUTCOME_FIELD, Value::from("refused")),
            (CODE_FIELD, Value::from(refusal_text(*refusal))),
        ],
        SentOutcome::Failed(failure) => vec![
            (OUTCOME_FIELD, Value::from("failed")),
            (ERROR_FIELD, Value::from(failure_text(failure))),
        ],
    }
}

/// How a request this node served ended, as fields: the outcome, and the refusal's code or why it went unanswered.
fn served_outcome_fields(outcome: ServedOutcome) -> EventFields {
    match outcome {
        ServedOutcome::Answered => vec![(OUTCOME_FIELD, Value::from("answered"))],
        ServedOutcome::Refused(refusal) => vec![
            (OUTCOME_FIELD, Value::from("refused")),
            (CODE_FIELD, Value::from(refusal_text(refusal))),
        ],
        ServedOutcome::Unanswered(end) => vec![
            (OUTCOME_FIELD, Value::from("unanswered")),
            (ERROR_FIELD, Value::from(stream_end_text(end))),
        ],
    }
}

/// Why a sent request failed, as the events file writes it.
const fn failure_text(failure: &DirectError) -> &'static str {
    match failure {
        DirectError::Unreachable => "unreachable",
        DirectError::Timeout => "timeout",
        DirectError::Refused => "refused",
        DirectError::UnsealableRequest(_) => "unsealable_request",
        DirectError::InvalidAck(_) => "invalid_ack",
        DirectError::OversizedAck => "oversized_ack",
        DirectError::UnexpectedAck => "unexpected_ack",
        DirectError::MessageAckIdMismatch => "message_ack_id_mismatch",
        DirectError::ProbeNonceMismatch => "probe_nonce_mismatch",
    }
}

/// Why a served request went unanswered, as the events file writes it.
const fn stream_end_text(end: StreamEnd) -> &'static str {
    match end {
        StreamEnd::OverWireLimit => "over_wire_limit",
        StreamEnd::ReadTimeout => "read_timeout",
        StreamEnd::Unanswered => "unanswered",
        StreamEnd::PeerGone => "peer_gone",
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;
    use crate::crypto::KEY_LEN;
    use crate::event::{Event, write_run_record};

    const PEER_KEY_BYTE: u8 = 7;

    #[test]
    fn observability_decision_3_a_failed_contact_adds_no_line_to_standard_error() {
        let event: Event = DirectEvent::ContactFailed {
            peer: SecretKey::from_bytes(&[PEER_KEY_BYTE; KEY_LEN]).public(),
            reason: DirectError::Unreachable,
        }
        .into();
        let mut standard_error = Vec::new();

        write_run_record(&mut standard_error, &event).unwrap();

        assert_eq!(String::from_utf8(standard_error).unwrap(), "");
    }
}
