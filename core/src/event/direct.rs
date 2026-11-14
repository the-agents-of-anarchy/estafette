use iroh::EndpointId;
use serde_json::Value;

use super::transport::connection_field;
use super::{AreaEvent, EventFields};
use crate::errors::ErrorCode;
use crate::net::direct::{
    AckRefusal, ConnectionSource, DirectError, RequestSummary, SentOutcome, SentRequest,
    ServedOutcome, ServedRequest, StreamEnd, message_id_text,
};

const KIND_FIELD: &str = "kind";
const MESSAGE_FIELD: &str = "message";
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
                vec![sent.peer.field()],
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
                vec![served.peer.field(), connection_field(served.connection)],
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
            (MESSAGE_FIELD, Value::from(message_id_text(id))),
        ],
        RequestSummary::Probe => vec![(KIND_FIELD, Value::from("probe"))],
        RequestSummary::Introduce => vec![(KIND_FIELD, Value::from("introduce"))],
    }
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
        AckRefusal::TooLarge => ErrorCode::TooLarge.code(),
        AckRefusal::Refused => ErrorCode::Refused.code(),
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
    use crate::net::direct::tests::assert_each_arm;
    use crate::payload::PayloadError;

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

    #[test]
    fn observability_decision_9_each_way_a_sent_request_fails_is_written_as_a_word_of_its_own() {
        let cases = [
            (DirectError::Unreachable, "unreachable"),
            (DirectError::Timeout, "timeout"),
            (DirectError::Refused, "refused"),
            (
                DirectError::UnsealableRequest(PayloadError::SignerIsNotThePayloadSigner),
                "unsealable_request",
            ),
            (
                DirectError::InvalidAck(PayloadError::SignerIsNotThePayloadSigner),
                "invalid_ack",
            ),
            (DirectError::OversizedAck, "oversized_ack"),
            (DirectError::UnexpectedAck, "unexpected_ack"),
            (DirectError::MessageAckIdMismatch, "message_ack_id_mismatch"),
            (DirectError::ProbeNonceMismatch, "probe_nonce_mismatch"),
        ];

        assert_each_arm(&cases, |failure| failure_text(failure), cases.len());
    }

    #[test]
    fn observability_decision_9_each_way_a_served_request_goes_unanswered_is_written_as_a_word_of_its_own()
     {
        let cases = [
            (StreamEnd::OverWireLimit, "over_wire_limit"),
            (StreamEnd::ReadTimeout, "read_timeout"),
            (StreamEnd::Unanswered, "unanswered"),
            (StreamEnd::PeerGone, "peer_gone"),
        ];

        assert_each_arm(&cases, |end| stream_end_text(*end), cases.len());
    }

    const MESSAGE_ID: [u8; 16] = [
        0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98, 0xA9, 0xBA, 0xCB, 0xDC, 0xED, 0xFE,
        0x0F,
    ];
    const MESSAGE_ID_TEXT: &str = "10213243-5465-7687-98a9-bacbdcedfe0f";

    #[test]
    fn observability_decision_9_a_held_and_a_dialed_connection_are_written_apart() {
        let cases = [
            (ConnectionSource::Held, "held"),
            (ConnectionSource::Dialed, "dialed"),
        ];

        assert_each_arm(&cases, |source| source_text(*source), cases.len());
    }

    #[test]
    fn observability_decision_9_each_refusal_code_is_written_as_the_wire_writes_it() {
        let cases = [
            (AckRefusal::TooLarge, "too_large"),
            (AckRefusal::Refused, "refused"),
            (AckRefusal::Unlisted, "unlisted"),
        ];

        assert_each_arm(&cases, |refusal| refusal_text(*refusal), cases.len());
    }

    #[test]
    fn observability_decision_9_each_request_kind_is_written_as_a_word_and_a_message_with_its_id() {
        let cases = [
            (
                RequestSummary::Message(MESSAGE_ID),
                vec![
                    ("kind", Value::from("message")),
                    ("message", Value::from(MESSAGE_ID_TEXT)),
                ],
            ),
            (RequestSummary::Probe, vec![("kind", Value::from("probe"))]),
            (
                RequestSummary::Introduce,
                vec![("kind", Value::from("introduce"))],
            ),
        ];

        assert_each_arm(&cases, |request| request_fields(*request), cases.len());
    }

    #[test]
    fn observability_decision_9_each_outcome_of_a_sent_request_is_written_as_a_word_with_its_code_or_error()
     {
        let cases = [
            (
                SentOutcome::Acknowledged,
                vec![("outcome", Value::from("acknowledged"))],
            ),
            (
                SentOutcome::Refused(AckRefusal::TooLarge),
                vec![
                    ("outcome", Value::from("refused")),
                    ("code", Value::from("too_large")),
                ],
            ),
            (
                SentOutcome::Failed(DirectError::Timeout),
                vec![
                    ("outcome", Value::from("failed")),
                    ("error", Value::from("timeout")),
                ],
            ),
        ];

        assert_each_arm(&cases, sent_outcome_fields, cases.len());
    }

    #[test]
    fn observability_decision_9_each_outcome_of_a_served_request_is_written_as_a_word_with_its_code_or_error()
     {
        let cases = [
            (
                ServedOutcome::Answered,
                vec![("outcome", Value::from("answered"))],
            ),
            (
                ServedOutcome::Refused(AckRefusal::Refused),
                vec![
                    ("outcome", Value::from("refused")),
                    ("code", Value::from("refused")),
                ],
            ),
            (
                ServedOutcome::Unanswered(StreamEnd::PeerGone),
                vec![
                    ("outcome", Value::from("unanswered")),
                    ("error", Value::from("peer_gone")),
                ],
            ),
        ];

        assert_each_arm(
            &cases,
            |outcome| served_outcome_fields(*outcome),
            cases.len(),
        );
    }
}
