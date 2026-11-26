use std::time::Duration;

use iroh::EndpointId;
use serde_json::Value;

use crate::events_lines::{
    ACKNOWLEDGED_OUTCOME, MESSAGE_FIELD, MESSAGE_KIND, PAYLOAD_ACCEPTED_EVENT, PRESENCE_KIND,
    REQUEST_SENT_EVENT, SEQ_FIELD, is_accepted_payload_of, is_acknowledged_request_of_kind,
    short_id_of_node,
};
use crate::mcp_server::{PRESENCE_WAIT, RESULT_KEY};
use crate::member_events::MemberEvents;
use crate::spec_bounds::SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE;
use crate::trace_reader::TraceLine;

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
fn acknowledged_after(
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

/// Requires the recipient to have acknowledged the Message the send result names within the spec's bound of the sender's acceptance of the new Presence (spec 3.15 test 18).
#[track_caller]
pub fn assert_acknowledged_promptly_after(
    sender: &MemberEvents,
    new_presence: &NewPresenceAccepted,
    sent: &Value,
) {
    let delivered_after = acknowledged_after(sender, new_presence, sent);
    assert!(
        delivered_after < SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE,
        "acknowledged {delivered_after:?} after the new presence, not under {SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE:?}"
    );
}

/// Whether the events line records a Presence the peer stamped after the instant, accepted and written after it.
fn is_presence_accepted_after(line: &TraceLine, peer: &str, instant_ms: u64) -> bool {
    is_accepted_payload_of(line, PRESENCE_KIND, peer)
        && line.wall_ms > instant_ms
        && line
            .value(SEQ_FIELD)
            .and_then(Value::as_u64)
            .is_some_and(|seq| seq > instant_ms)
}

/// Whether the events line records the Message of that id sent and acknowledged by its recipient.
fn is_message_acknowledged(line: &TraceLine, message: &Value) -> bool {
    is_acknowledged_request_of_kind(line, MESSAGE_KIND)
        && line.value(MESSAGE_FIELD) == Some(message)
}
