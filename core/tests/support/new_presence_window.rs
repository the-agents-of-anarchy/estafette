use std::time::Duration;

use iroh::EndpointId;
use serde_json::Value;

use crate::dropped_payloads::short_id_of_node;
use crate::mcp_server::{PRESENCE_WAIT, RESULT_KEY};
use crate::member_events::MemberEvents;
use crate::trace_reader::TraceLine;

/// The event a member records for each gossip payload it accepts (spec/observability.md decision 9).
const PAYLOAD_ACCEPTED_EVENT: &str = "gossip.payload_accepted";
/// The event a member records for each direct request it sends, with how it ended (spec/observability.md decision 9).
const REQUEST_SENT_EVENT: &str = "direct.request_sent";
/// The field of an events line naming a payload's or a request's kind (spec/observability.md decision 6).
const KIND_FIELD: &str = "kind";
/// The field of an events line naming a payload's signer (spec/observability.md decision 6).
const PEER_FIELD: &str = "peer";
/// The field of an events line naming a payload's seq (spec/observability.md decision 6).
const SEQ_FIELD: &str = "seq";
/// The field of an events line naming a Message's id (spec/observability.md decision 6).
const MESSAGE_FIELD: &str = "message";
/// The field of an events line naming a request's outcome (spec/observability.md decision 6).
const OUTCOME_FIELD: &str = "outcome";
/// The kind of payload a Presence is, as an events line names it.
const PRESENCE_KIND: &str = "presence";
/// The kind of direct request a Message is, as an events line names it.
const MESSAGE_KIND: &str = "message";
/// The outcome of a request its recipient acknowledged, as an events line names it.
const ACKNOWLEDGED_OUTCOME: &str = "acknowledged";
/// The key of a send result holding the id of the Message it sent (spec 3.10).
const MESSAGE_ID_KEY: &str = "id";
/// How long a member may take to write the line of a request whose answer the test already holds: chosen, not measured, far above a queued line's write, so only a line never written fails it.
const ANSWERED_REQUEST_LINE_WAIT: Duration = Duration::from_secs(10);

/// The sender's acceptance of a Presence the card stamped after the kill, the instant spec 3.15 test 18 counts its second from, held as the sender's monotonic reading of it.
pub struct NewPresenceAccepted {
    mono_ms: u64,
}

/// The sender's line recording its acceptance of a Presence the card stamped after the kill, written after the kill: the instant spec 3.15 test 18 counts its second from, awaited within the presence wait.
#[track_caller]
pub fn new_presence_accepted(
    sender: &MemberEvents,
    card: EndpointId,
    killed_ms: u64,
) -> NewPresenceAccepted {
    let peer = short_id_of_node(card);
    let accepted = sender.wait_until(
        &format!(
            "{PAYLOAD_ACCEPTED_EVENT} of a {PRESENCE_KIND} from {peer} stamped and written after the kill at {killed_ms} ms"
        ),
        PRESENCE_WAIT,
        |lines| {
            lines
                .into_iter()
                .find(|line| is_presence_accepted_after(line, &peer, killed_ms))
        },
    );
    NewPresenceAccepted {
        mono_ms: accepted.mono_ms,
    }
}

/// How long after the sender accepted the new Presence its recipient acknowledged the Message the send result names, both read on the sender's monotonic clock from its own events (spec 3.15 test 18).
#[track_caller]
pub fn acknowledged_after(
    sender: &MemberEvents,
    new_presence: &NewPresenceAccepted,
    sent: &Value,
) -> Duration {
    let message = &sent[RESULT_KEY][MESSAGE_ID_KEY];
    let acknowledged = sender.wait_until(
        &format!("{REQUEST_SENT_EVENT} of the {MESSAGE_KIND} {message} {ACKNOWLEDGED_OUTCOME}"),
        ANSWERED_REQUEST_LINE_WAIT,
        |lines| {
            lines
                .into_iter()
                .find(|line| is_message_acknowledged(line, message))
        },
    );
    Duration::from_millis(
        acknowledged
            .mono_ms
            .checked_sub(new_presence.mono_ms)
            .unwrap_or_else(|| {
                panic!(
                    "the {REQUEST_SENT_EVENT} of the {MESSAGE_KIND} {message} {ACKNOWLEDGED_OUTCOME} is stamped {} ms, before the new {PRESENCE_KIND}'s {} ms",
                    acknowledged.mono_ms, new_presence.mono_ms
                )
            }),
    )
}

/// Whether the events line records a Presence the peer stamped after the instant, accepted and written after it.
fn is_presence_accepted_after(line: &TraceLine, peer: &str, instant_ms: u64) -> bool {
    line.event == PAYLOAD_ACCEPTED_EVENT
        && line.wall_ms > instant_ms
        && line.value(KIND_FIELD) == Some(&Value::from(PRESENCE_KIND))
        && line.value(PEER_FIELD) == Some(&Value::from(peer))
        && line
            .value(SEQ_FIELD)
            .and_then(Value::as_u64)
            .is_some_and(|seq| seq > instant_ms)
}

/// Whether the events line records the Message of that id sent and acknowledged by its recipient.
fn is_message_acknowledged(line: &TraceLine, message: &Value) -> bool {
    line.event == REQUEST_SENT_EVENT
        && line.value(KIND_FIELD) == Some(&Value::from(MESSAGE_KIND))
        && line.value(MESSAGE_FIELD) == Some(message)
        && line.value(OUTCOME_FIELD) == Some(&Value::from(ACKNOWLEDGED_OUTCOME))
}
