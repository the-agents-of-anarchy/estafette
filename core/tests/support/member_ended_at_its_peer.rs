use std::time::{Duration, Instant};

use serde_json::json;

use crate::mcp_server::{McpServer, SEND_TOOL, gone};
use crate::member_events::MemberEvents;
use crate::message_body::BODY;

/// Spec 3.15 test 4: a member lists a peer that stopped cleanly gone within this long.
const SPEC_GONE_WITHIN: Duration = Duration::from_millis(1000);
/// The event a member records when its standard input ends (spec/observability.md decision 9).
const INPUT_ENDED_EVENT: &str = "process.input_ended";
/// The event a member records for each tool call it answers (spec/observability.md decision 9).
const TOOL_ANSWERED_EVENT: &str = "tool.answered";
/// The field of the tool-answered event that names the tool.
const TOOL_FIELD: &str = "tool";
/// How long a member that has exited may take to be read for lines it wrote before: chosen, not measured, far above a read of a written file.
const EVENTS_OF_AN_ENDED_MEMBER_WITHIN: Duration = Duration::from_secs(10);
/// The code of a send to a member that is no longer there (spec 3.10).
pub const UNREACHABLE_CODE: &str = "unreachable";

/// Requires bob, whose standard input closed at that instant, listed gone by amy within the spec bound, exited cleanly, and unreachable to amy's send.
#[track_caller]
pub fn assert_stopped_member_is_gone_then_unreachable(
    amy: &mut McpServer,
    bob: &mut McpServer,
    bob_handle: &str,
    input_closed: Instant,
) {
    amy.peer_within(bob_handle, SPEC_GONE_WITHIN, gone);
    let gone_after = input_closed.elapsed();
    let exit = bob.wait_for_exit();
    let sent = amy.send(bob_handle, BODY);

    assert!(gone_after <= SPEC_GONE_WITHIN, "{gone_after:?}");
    assert!(exit.success(), "{exit:?}");
    assert_eq!(sent["error"]["code"], json!(UNREACHABLE_CODE), "{sent}");
}

/// Waits until the member's events hold the end of its standard input, failing with the member's timeline when the bound runs out.
#[track_caller]
pub fn wait_for_input_ended(events: &MemberEvents) {
    events.wait_until(
        INPUT_ENDED_EVENT,
        EVENTS_OF_AN_ENDED_MEMBER_WITHIN,
        |lines| {
            lines
                .iter()
                .any(|line| line.event == INPUT_ENDED_EVENT)
                .then_some(())
        },
    );
}

/// Requires the member's events to hold its answer of the send tool after the end of its input, blaming the test's own setup when a recipient that held the call unanswered let it be answered first.
#[track_caller]
pub fn assert_send_answered_after_input_ended(events: &MemberEvents) {
    let (input_ended, answered) = events.wait_until(
        &format!("{TOOL_ANSWERED_EVENT} of {SEND_TOOL} and {INPUT_ENDED_EVENT}"),
        EVENTS_OF_AN_ENDED_MEMBER_WITHIN,
        |lines| {
            let input_ended = lines.iter().find(|line| line.event == INPUT_ENDED_EVENT)?.n;
            let answered = lines
                .iter()
                .find(|line| {
                    line.event == TOOL_ANSWERED_EVENT
                        && line.value(TOOL_FIELD) == Some(&json!(SEND_TOOL))
                })?
                .n;
            Some((input_ended, answered))
        },
    );
    assert!(
        answered > input_ended,
        "the test's own setup failed, not the member: the call was answered at line {answered}, before the input ended at line {input_ended}, though the recipient held it unanswered"
    );
}
