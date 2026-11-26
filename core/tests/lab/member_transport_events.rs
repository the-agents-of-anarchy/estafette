use std::time::Duration;

use serde_json::Value;

use crate::home_relays::is_dropped_report;
use crate::member_events::MemberEvents;
use crate::trace_reader::{TraceLine, WallMs};

/// How long a member may take to record the addition of the added host's relay to its relay map after the host's first record: chosen, not measured, so only a line never written fails the wait and a late one fails by its stamp.
const RELAY_MAP_EDIT_WAIT: Duration = Duration::from_secs(30);
/// How long a member may take to write the line of a Message it served or sent, which its test already holds the answer of: chosen, not measured.
const SERVED_MESSAGE_LINE_WAIT: Duration = Duration::from_secs(10);
/// How long reading a member's events files once may take, the first look finding them: chosen, not measured.
const EVENTS_READ_WAIT: Duration = Duration::from_secs(10);

/// The event a member records when the relay map it dials through gains or loses a relay (spec/observability.md decision 9).
const RELAY_MAP_EDITED_EVENT: &str = "transport.relay_map_edited";
/// The event a member records when a path of a connection is selected (spec/observability.md decision 9).
const PATH_SELECTED_EVENT: &str = "transport.path_selected";
/// The event a member records when the path events of a connection arrived faster than they were read.
const PATH_EVENTS_MISSED_EVENT: &str = "transport.path_events_missed";
/// The event a member records when it rebinds its endpoint.
const REBINDING_EVENT: &str = "transport.rebinding";
/// The event a member records for each direct request it serves.
const REQUEST_SERVED_EVENT: &str = "direct.request_served";
/// The event a member records for each direct request it sends, with how it ended.
const REQUEST_SENT_EVENT: &str = "direct.request_sent";

/// The fields of those events naming the edit, the known host a relay belongs to, the kind of a location or of a request, the connection and the Message.
const EDIT_FIELD: &str = "edit";
const HOST_FIELD: &str = "host";
const KIND_FIELD: &str = "kind";
const CONNECTION_FIELD: &str = "connection";
const MESSAGE_FIELD: &str = "message";
const OUTCOME_FIELD: &str = "outcome";
/// How the events file words an edit that adds a relay, a location that is a relay, and a request that is a Message.
const ADDED_EDIT: &str = "added";
const RELAY_KIND: &str = "relay";
const MESSAGE_KIND: &str = "message";
const ACKNOWLEDGED_OUTCOME: &str = "acknowledged";

/// Waits for the member's own line adding the relay of the host to its relay map, and requires its stamp to lie neither before the instant the test started the host nor more than `within` after the host's first record, all read on one machine's clock (spec 3.15 test 22).
#[track_caller]
pub fn relay_added_within(
    events: &MemberEvents,
    host: &str,
    host_started_wall_ms: u64,
    first_record_wall_ms: u64,
    within: Duration,
) -> Result<(), String> {
    events.wait_until(
        &format!("{RELAY_MAP_EDITED_EVENT} {ADDED_EDIT} of the relay of host {host}"),
        RELAY_MAP_EDIT_WAIT,
        |lines| {
            first_relay_added_position_for(&lines, host)
                .is_some()
                .then(|| {
                    relay_added_verdict(
                        &lines,
                        host,
                        host_started_wall_ms,
                        first_record_wall_ms,
                        within,
                    )
                })
        },
    )
}

/// Waits for the member's own line recording that it served the Message, and requires every path selected for that Message's connection before the line to be a relay path, at least one having been selected (spec 3.15 test 3).
#[track_caller]
pub fn message_rode_a_relay_path(events: &MemberEvents, message: &str) -> Result<(), String> {
    events.wait_until(
        &format!("{REQUEST_SERVED_EVENT} of the {MESSAGE_KIND} {message}"),
        SERVED_MESSAGE_LINE_WAIT,
        |lines| {
            served_message_position(&lines, message).map(|_| relay_path_verdict(&lines, message))
        },
    )
}

/// Waits for the sender's own line recording that the Message was acknowledged, and requires every path selected for the connection that carried it before the line to be a relay path, at least one having been selected (spec 3.15 test 3).
#[track_caller]
pub fn acknowledged_message_rode_a_relay_path(
    events: &MemberEvents,
    message: &str,
) -> Result<(), String> {
    events.wait_until(
        &format!("{REQUEST_SENT_EVENT} of the {MESSAGE_KIND} {message} {ACKNOWLEDGED_OUTCOME}"),
        SERVED_MESSAGE_LINE_WAIT,
        |lines| {
            acknowledged_message_position(&lines, message)
                .map(|_| acknowledged_path_verdict(&lines, message))
        },
    )
}

/// Reads each member's events files once and requires none to hold a rebinding line, naming each member that does or whose files hold a report of lines lost (spec 3.15 test 24).
#[track_caller]
pub fn no_member_rebound<'member>(
    members: impl IntoIterator<Item = (&'member str, &'member MemberEvents)>,
) -> Result<(), String> {
    let rebound: Vec<String> = members
        .into_iter()
        .filter_map(|(member, events)| {
            match events.wait_until("its events files, read once", EVENTS_READ_WAIT, |lines| {
                Some(rebinds_read_from(&lines))
            }) {
                RebindsRead::Counted(0) => None,
                RebindsRead::Counted(rebinds) => Some(format!(
                    "{member} recorded {rebinds} {REBINDING_EVENT} lines"
                )),
                RebindsRead::LinesLost => Some(format!(
                    "{member}'s trace lost lines, so its {REBINDING_EVENT} lines are not all known"
                )),
            }
        })
        .collect();
    if rebound.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "spec 3.15 test 24 runs three members with no host killed and none may rebind: {}",
            rebound.join("; ")
        ))
    }
}

/// The position and the line of the first line adding the relay of the host to the member's relay map.
fn first_relay_added_position_for<'line>(
    lines: &'line [TraceLine],
    host: &str,
) -> Option<(usize, &'line TraceLine)> {
    lines.iter().enumerate().find(|(_position, line)| {
        line.event == RELAY_MAP_EDITED_EVENT
            && line.value(EDIT_FIELD) == Some(&Value::from(ADDED_EDIT))
            && line.value(HOST_FIELD) == Some(&Value::from(host))
    })
}

/// Whether the first line adding the host's relay is stamped no earlier than the host's start and no later than `within` after the first record, refusing with what was found when it is not, when no line adds it, or when the trace lost lines before it.
fn relay_added_verdict(
    lines: &[TraceLine],
    host: &str,
    host_started_wall_ms: u64,
    first_record_wall_ms: u64,
    within: Duration,
) -> Result<(), String> {
    let (position, added) = first_relay_added_position_for(lines, host).ok_or_else(|| {
        format!("no {RELAY_MAP_EDITED_EVENT} {ADDED_EDIT} of the relay of host {host}")
    })?;
    if lines.iter().take(position).any(is_dropped_report) {
        return Err(format!(
            "lines of the trace holding the relay of host {host} were lost before the line adding it, so it may not be the first"
        ));
    }
    if added.wall_ms < WallMs(host_started_wall_ms) {
        return Err(format!(
            "the relay of host {host} was added to the relay map at {} ms, {} ms before the test started the host at {host_started_wall_ms} ms, so that line is not of the host's record",
            added.wall_ms,
            host_started_wall_ms.saturating_sub(added.wall_ms.0)
        ));
    }
    let bound_ms = u128::from(first_record_wall_ms).saturating_add(within.as_millis());
    if u128::from(added.wall_ms.0) <= bound_ms {
        Ok(())
    } else {
        Err(format!(
            "the relay of host {host} was added to the relay map at {} ms, {} ms after the host's first record at {first_record_wall_ms} ms, beyond {within:?}",
            added.wall_ms,
            added.wall_ms.0.saturating_sub(first_record_wall_ms)
        ))
    }
}

/// The position of the line recording that the member served the Message.
fn served_message_position(lines: &[TraceLine], message: &str) -> Option<usize> {
    lines
        .iter()
        .position(|line| is_message_served(line, message))
}

/// Whether the line records that the member served the Message.
fn is_message_served(line: &TraceLine, message: &str) -> bool {
    line.event == REQUEST_SERVED_EVENT
        && line.value(KIND_FIELD) == Some(&Value::from(MESSAGE_KIND))
        && line.value(MESSAGE_FIELD) == Some(&Value::from(message))
}

/// The position of the line recording that the sender's Message was acknowledged.
fn acknowledged_message_position(lines: &[TraceLine], message: &str) -> Option<usize> {
    lines
        .iter()
        .position(|line| is_message_acknowledged(line, message))
}

/// Whether the line records that the member sent the Message and its recipient acknowledged it.
fn is_message_acknowledged(line: &TraceLine, message: &str) -> bool {
    line.event == REQUEST_SENT_EVENT
        && line.value(KIND_FIELD) == Some(&Value::from(MESSAGE_KIND))
        && line.value(MESSAGE_FIELD) == Some(&Value::from(message))
        && line.value(OUTCOME_FIELD) == Some(&Value::from(ACKNOWLEDGED_OUTCOME))
}

/// Whether every path selected for the connection of the served Message before the serving line is a relay path, refusing when none was selected, one was not a relay path, or path events of the connection were lost.
fn relay_path_verdict(lines: &[TraceLine], message: &str) -> Result<(), String> {
    relay_paths_before_the_line_at(
        lines,
        served_message_position(lines, message),
        &format!("{REQUEST_SERVED_EVENT} of the {MESSAGE_KIND} {message}"),
    )
}

/// Whether every path selected for the connection that carried the acknowledged Message before the sender's acknowledged line is a relay path, with the same refusals.
fn acknowledged_path_verdict(lines: &[TraceLine], message: &str) -> Result<(), String> {
    relay_paths_before_the_line_at(
        lines,
        acknowledged_message_position(lines, message),
        &format!("{REQUEST_SENT_EVENT} of the {MESSAGE_KIND} {message} {ACKNOWLEDGED_OUTCOME}"),
    )
}

/// Whether every path selected for the connection the line at the position names, before that line, is a relay path, refusing when there is no such line, none was selected, one was not a relay path, or path events of the connection were lost.
fn relay_paths_before_the_line_at(
    lines: &[TraceLine],
    position: Option<usize>,
    line_named: &str,
) -> Result<(), String> {
    let at = position.ok_or_else(|| format!("no {line_named}"))?;
    let (before, from) = lines.split_at(at);
    let connection = from
        .first()
        .and_then(|anchor| anchor.value(CONNECTION_FIELD));
    let of_the_connection = |event: &'static str| {
        before
            .iter()
            .filter(move |line| line.event == event && line.value(CONNECTION_FIELD) == connection)
    };
    if before.iter().any(is_dropped_report) {
        return Err(format!(
            "lines of the trace holding connection {connection:?} were lost before the {line_named}, so its selected paths are not all known"
        ));
    }
    if of_the_connection(PATH_EVENTS_MISSED_EVENT).next().is_some() {
        return Err(format!(
            "path events of connection {connection:?} were lost before the {line_named}, so its selected paths are not all known"
        ));
    }
    let selected_kinds: Vec<Option<&str>> = of_the_connection(PATH_SELECTED_EVENT)
        .map(|line| line.value(KIND_FIELD).and_then(Value::as_str))
        .collect();
    if selected_kinds.is_empty() {
        return Err(format!(
            "no {PATH_SELECTED_EVENT} for connection {connection:?} before the {line_named}"
        ));
    }
    if selected_kinds.iter().any(|kind| *kind != Some(RELAY_KIND)) {
        return Err(format!(
            "paths selected for connection {connection:?} before the {line_named}: {selected_kinds:?}, and each must be a relay path"
        ));
    }
    Ok(())
}

/// What reading a member's events files tells of its rebinds: how many lines record one, or that the trace lost lines which could have.
enum RebindsRead {
    Counted(usize),
    LinesLost,
}

/// The rebinds the lines tell of, none being known when a report of lost lines stands among them.
fn rebinds_read_from(lines: &[TraceLine]) -> RebindsRead {
    if lines.iter().any(is_dropped_report) {
        RebindsRead::LinesLost
    } else {
        RebindsRead::Counted(rebinding_lines_in(lines))
    }
}

/// How many lines record a rebind of the member's endpoint.
fn rebinding_lines_in(lines: &[TraceLine]) -> usize {
    lines
        .iter()
        .filter(|line| line.event == REBINDING_EVENT)
        .count()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use super::{
        acknowledged_message_rode_a_relay_path, message_rode_a_relay_path, no_member_rebound,
        rebinding_lines_in, relay_added_verdict, relay_added_within,
    };
    use crate::member_events::MemberEvents;
    use crate::this_process_directory::fresh_directory_of_this_process_in;
    use crate::trace_reader::{TraceLine, WritingProcess, parse_trace_lines};

    const ADDED_HOST: &str = "a1b2c3d4e5";
    const OTHER_HOST: &str = "f6e5d4c3b2";
    const MESSAGE: &str = "0000018c7f3a1e5b";
    const SERVING_CONNECTION: u64 = 7;
    const OTHER_CONNECTION: u64 = 9;
    const WITHIN: Duration = Duration::from_millis(1000);
    const FIRST_RECORD_WALL_MS: u64 = 1_794_524_400_000;
    const HOST_STARTED_WALL_MS: u64 = FIRST_RECORD_WALL_MS - 3000;
    const WALL_MS_OF_LINE_ZERO: u64 = 1_794_524_353_720;
    const EVENTS_FILE_NAME: &str = "20261112T225914Z-6c935f82c118f7b3-estafette.events.jsonl";
    const AMY: &str = "amyhzqv";
    const BOB: &str = "bobxkwt";
    const CID: &str = "cidpmrf";

    /// An events line of the member at the wall-clock stamp, with the fields given after the event's name.
    fn line_at(wall_ms: u64, event: &str, fields: &str) -> String {
        format!(
            r#"{{"v":2,"run":"6c935f82c118f7b3","n":0,"mono_ms":0,"wall_ms":{wall_ms},"node":"8f43abc249","event":"{event}"{fields}}}"#
        )
    }

    /// An events line stamped by its place in the file, which orders it before the lines after it.
    fn line(n: u64, event: &str, fields: &str) -> String {
        line_at(WALL_MS_OF_LINE_ZERO.saturating_add(n), event, fields)
    }

    /// The line a member writes when its relay map gains or loses the relay of a known host, at the stamp.
    fn relay_map_edit_at(wall_ms: u64, edit: &str, host: &str) -> String {
        line_at(
            wall_ms,
            "transport.relay_map_edited",
            &format!(
                r#","edit":"{edit}","host":"{host}","kind":"relay","port":443,"location":"3f9a""#
            ),
        )
    }

    /// The line a member writes when a path of the connection is selected, leading to a location of the kind.
    fn path_selected(n: u64, connection: u64, kind: &str) -> String {
        line(
            n,
            "transport.path_selected",
            &format!(
                r#","connection":{connection},"path":"0","kind":"{kind}","location":"3f9a","rtt_ms":12"#
            ),
        )
    }

    /// The line a member writes when it served the Message on the connection.
    fn message_served(n: u64, message: &str, connection: u64) -> String {
        line(
            n,
            "direct.request_served",
            &format!(
                r#","kind":"message","message":"{message}","peer":"8f43abc249","connection":{connection},"outcome":"answered","elapsed_ms":3"#
            ),
        )
    }

    /// The trace lines of the text lines given.
    fn parsed(lines: &[String]) -> Vec<TraceLine> {
        let text = format!("{}\n", lines.join("\n"));
        parse_trace_lines(text.as_bytes(), WritingProcess::Ended).unwrap()
    }

    /// A member's events reader over a fresh directory named for the test, holding one events file of the lines given.
    fn events_holding(test: &str, lines: &[String]) -> (MemberEvents, PathBuf) {
        let traces = fresh_directory_of_this_process_in(
            &std::env::temp_dir(),
            &format!("member-transport-events-{test}"),
        )
        .unwrap();
        fs::write(
            traces.join(EVENTS_FILE_NAME),
            format!("{}\n", lines.join("\n")),
        )
        .unwrap();
        (MemberEvents::new(test, &traces), traces)
    }

    fn remove_traces(traces: &Path) {
        fs::remove_dir_all(traces).unwrap();
    }

    fn relay_added_for_the_added_host(lines: &[String]) -> Result<(), String> {
        relay_added_verdict(
            &parsed(lines),
            ADDED_HOST,
            HOST_STARTED_WALL_MS,
            FIRST_RECORD_WALL_MS,
            WITHIN,
        )
    }

    fn path_verdict_of(test: &str, lines: &[String]) -> Result<(), String> {
        let (events, traces) = events_holding(test, lines);
        let verdict = message_rode_a_relay_path(&events, MESSAGE);
        remove_traces(&traces);
        verdict
    }

    fn rebind_verdict_of(test: &str, members: &[(&str, Vec<String>)]) -> Result<(), String> {
        let held: Vec<(&str, MemberEvents, PathBuf)> = members
            .iter()
            .map(|(member, lines)| {
                let (events, traces) = events_holding(&format!("{test}-{member}"), lines);
                (*member, events, traces)
            })
            .collect();
        let verdict = no_member_rebound(held.iter().map(|(member, events, _)| (*member, events)));
        for (_member, _events, traces) in &held {
            remove_traces(traces);
        }
        verdict
    }

    #[test]
    fn spec_3_15_test_22_a_line_adding_the_added_hosts_relay_inside_the_bound_passes() {
        let inside = relay_map_edit_at(FIRST_RECORD_WALL_MS + 400, "added", ADDED_HOST);

        assert_eq!(relay_added_for_the_added_host(&[inside]), Ok(()));
    }

    #[test]
    fn spec_3_15_test_22_a_line_adding_the_relay_exactly_1000_ms_after_the_first_record_passes() {
        let at_the_bound = relay_map_edit_at(FIRST_RECORD_WALL_MS + 1000, "added", ADDED_HOST);

        assert_eq!(relay_added_for_the_added_host(&[at_the_bound]), Ok(()));
    }

    #[test]
    fn spec_3_15_test_22_a_line_adding_the_relay_1001_ms_after_the_first_record_fails_naming_the_lateness()
     {
        let outside = relay_map_edit_at(FIRST_RECORD_WALL_MS + 1001, "added", ADDED_HOST);

        let refusal = relay_added_for_the_added_host(&[outside]).unwrap_err();

        assert!(
            refusal.contains("1001 ms after the host's first record"),
            "{refusal}"
        );
    }

    #[test]
    fn spec_3_15_test_22_a_line_adding_the_relay_of_another_host_fails_though_it_is_inside_the_bound()
     {
        let another_host = relay_map_edit_at(FIRST_RECORD_WALL_MS + 100, "added", OTHER_HOST);

        let refusal = relay_added_for_the_added_host(&[another_host]).unwrap_err();

        assert!(refusal.contains(ADDED_HOST), "{refusal}");
    }

    #[test]
    fn spec_3_15_test_22_a_line_removing_the_added_hosts_relay_fails_though_it_is_inside_the_bound()
    {
        let removal = relay_map_edit_at(FIRST_RECORD_WALL_MS + 100, "removed", ADDED_HOST);

        assert!(relay_added_for_the_added_host(&[removal]).is_err());
    }

    #[test]
    fn spec_3_15_test_22_the_first_line_adding_the_relay_decides_so_a_later_line_does_not_pass_a_late_first()
     {
        let late = relay_map_edit_at(FIRST_RECORD_WALL_MS + 5000, "added", ADDED_HOST);
        let stamped_inside = relay_map_edit_at(FIRST_RECORD_WALL_MS + 100, "added", ADDED_HOST);

        assert!(relay_added_for_the_added_host(&[late, stamped_inside]).is_err());
    }

    #[test]
    fn spec_3_15_test_22_a_line_adding_the_relay_before_the_test_started_the_host_fails_naming_the_start()
     {
        let before_the_start = relay_map_edit_at(HOST_STARTED_WALL_MS - 1, "added", ADDED_HOST);

        let refusal = relay_added_for_the_added_host(&[before_the_start]).unwrap_err();

        assert!(
            refusal.contains("1 ms before the test started the host"),
            "{refusal}"
        );
    }

    #[test]
    fn spec_3_15_test_22_a_line_adding_the_relay_at_the_instant_the_test_started_the_host_passes() {
        let at_the_start = relay_map_edit_at(HOST_STARTED_WALL_MS, "added", ADDED_HOST);

        assert_eq!(relay_added_for_the_added_host(&[at_the_start]), Ok(()));
    }

    #[test]
    fn spec_3_15_test_22_the_first_line_adding_the_relay_decides_so_a_later_one_after_the_start_does_not_pass_an_early_first()
     {
        let before_the_start = relay_map_edit_at(HOST_STARTED_WALL_MS - 500, "added", ADDED_HOST);
        let after_the_start = relay_map_edit_at(FIRST_RECORD_WALL_MS + 100, "added", ADDED_HOST);

        assert!(relay_added_for_the_added_host(&[before_the_start, after_the_start]).is_err());
    }

    #[test]
    fn spec_3_15_test_3_a_connection_whose_selected_path_is_a_relay_path_alone_passes() {
        let verdict = path_verdict_of(
            "relay-alone",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                message_served(2, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert_eq!(verdict, Ok(()));
    }

    #[test]
    fn spec_3_15_test_3_a_direct_path_selected_for_the_connection_before_the_send_was_served_fails()
    {
        let verdict = path_verdict_of(
            "direct-selected",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                path_selected(2, SERVING_CONNECTION, "direct"),
                message_served(3, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert!(verdict.unwrap_err().contains(r#"Some("direct")"#));
    }

    #[test]
    fn spec_3_15_test_3_a_direct_path_selected_after_the_send_was_served_does_not_fail_it() {
        let verdict = path_verdict_of(
            "direct-after",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                message_served(2, MESSAGE, SERVING_CONNECTION),
                path_selected(3, SERVING_CONNECTION, "direct"),
            ],
        );

        assert_eq!(verdict, Ok(()));
    }

    #[test]
    fn spec_3_15_test_3_a_direct_path_of_another_connection_does_not_fail_the_send() {
        let verdict = path_verdict_of(
            "other-connection",
            &[
                path_selected(1, OTHER_CONNECTION, "direct"),
                path_selected(2, SERVING_CONNECTION, "relay"),
                message_served(3, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert_eq!(verdict, Ok(()));
    }

    #[test]
    fn spec_3_15_test_3_a_connection_with_no_selected_path_recorded_fails_rather_than_passes() {
        let verdict = path_verdict_of(
            "none-selected",
            &[
                path_selected(1, OTHER_CONNECTION, "relay"),
                message_served(2, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert!(verdict.unwrap_err().contains("no transport.path_selected"));
    }

    #[test]
    fn spec_3_15_test_3_lost_path_events_of_the_connection_fail_the_send_as_unknown() {
        let verdict = path_verdict_of(
            "events-lost",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                line(
                    2,
                    "transport.path_events_missed",
                    &format!(r#","connection":{SERVING_CONNECTION},"missed":4"#),
                ),
                message_served(3, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert!(verdict.unwrap_err().contains("were lost"));
    }

    /// The line a member's trace writes before the next line it writes after it lost lines.
    fn dropped_report(n: u64) -> String {
        line(n, "dropped", r#","count":3"#)
    }

    #[test]
    fn spec_3_15_test_3_a_dropped_report_before_the_serving_line_refuses_the_receiver_as_lines_lost()
     {
        let verdict = path_verdict_of(
            "dropped-before-served",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                dropped_report(2),
                message_served(3, MESSAGE, SERVING_CONNECTION),
            ],
        );

        let refusal = verdict.unwrap_err();
        assert!(
            refusal.contains("lines of the trace holding connection")
                && refusal.contains(&SERVING_CONNECTION.to_string()),
            "{refusal}"
        );
    }

    #[test]
    fn spec_3_15_test_3_a_dropped_report_after_the_serving_line_does_not_refuse_the_receiver() {
        let verdict = path_verdict_of(
            "dropped-after-served",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                message_served(2, MESSAGE, SERVING_CONNECTION),
                dropped_report(3),
            ],
        );

        assert_eq!(verdict, Ok(()));
    }

    #[test]
    fn spec_3_15_test_3_a_path_of_neither_relay_nor_direct_kind_fails_the_send() {
        let verdict = path_verdict_of(
            "other-kind",
            &[
                path_selected(1, SERVING_CONNECTION, "other"),
                message_served(2, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert!(verdict.is_err());
    }

    /// The line a sender writes when the Message it sent on the connection was acknowledged.
    fn message_acknowledged(n: u64, message: &str, connection: u64) -> String {
        line(
            n,
            "direct.request_sent",
            &format!(
                r#","kind":"message","message":"{message}","peer":"8f43abc249","connection":{connection},"connection_source":"dialed","outcome":"acknowledged","elapsed_ms":3"#
            ),
        )
    }

    fn sender_path_verdict_of(test: &str, lines: &[String]) -> Result<(), String> {
        let (events, traces) = events_holding(test, lines);
        let verdict = acknowledged_message_rode_a_relay_path(&events, MESSAGE);
        remove_traces(&traces);
        verdict
    }

    #[test]
    fn spec_3_15_test_3_a_sender_whose_connection_selected_a_relay_path_alone_up_to_the_acknowledged_line_passes()
     {
        let verdict = sender_path_verdict_of(
            "sender-relay-alone",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                message_acknowledged(2, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert_eq!(verdict, Ok(()));
    }

    #[test]
    fn spec_3_15_test_3_a_direct_path_selected_at_the_sender_before_its_acknowledged_line_fails() {
        let verdict = sender_path_verdict_of(
            "sender-direct-selected",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                path_selected(2, SERVING_CONNECTION, "direct"),
                message_acknowledged(3, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert!(verdict.unwrap_err().contains(r#"Some("direct")"#));
    }

    #[test]
    fn spec_3_15_test_3_a_direct_path_selected_at_the_sender_after_its_acknowledged_line_does_not_fail_it()
     {
        let verdict = sender_path_verdict_of(
            "sender-direct-after",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                message_acknowledged(2, MESSAGE, SERVING_CONNECTION),
                path_selected(3, SERVING_CONNECTION, "direct"),
            ],
        );

        assert_eq!(verdict, Ok(()));
    }

    #[test]
    fn spec_3_15_test_3_a_direct_path_of_another_connection_at_the_sender_does_not_fail_it() {
        let verdict = sender_path_verdict_of(
            "sender-other-connection",
            &[
                path_selected(1, OTHER_CONNECTION, "direct"),
                path_selected(2, SERVING_CONNECTION, "relay"),
                message_acknowledged(3, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert_eq!(verdict, Ok(()));
    }

    #[test]
    fn spec_3_15_test_3_no_selected_path_recorded_at_the_sender_for_the_connection_fails() {
        let verdict = sender_path_verdict_of(
            "sender-none-selected",
            &[
                path_selected(1, OTHER_CONNECTION, "relay"),
                message_acknowledged(2, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert!(verdict.unwrap_err().contains("no transport.path_selected"));
    }

    #[test]
    fn spec_3_15_test_3_lost_path_events_of_the_senders_connection_fail_it_as_unknown() {
        let verdict = sender_path_verdict_of(
            "sender-events-lost",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                line(
                    2,
                    "transport.path_events_missed",
                    &format!(r#","connection":{SERVING_CONNECTION},"missed":4"#),
                ),
                message_acknowledged(3, MESSAGE, SERVING_CONNECTION),
            ],
        );

        assert!(verdict.unwrap_err().contains("were lost"));
    }

    #[test]
    fn spec_3_15_test_3_a_failed_attempt_of_the_message_is_not_the_acknowledged_line_the_paths_are_counted_up_to()
     {
        let sent_but_failed = line(
            2,
            "direct.request_sent",
            &format!(
                r#","kind":"message","message":"{MESSAGE}","peer":"8f43abc249","connection":{SERVING_CONNECTION},"outcome":"failed","error":"timeout","elapsed_ms":3"#
            ),
        );
        let lines = [
            path_selected(1, SERVING_CONNECTION, "relay"),
            sent_but_failed,
            path_selected(3, SERVING_CONNECTION, "direct"),
            message_acknowledged(4, MESSAGE, SERVING_CONNECTION),
        ];

        let verdict = sender_path_verdict_of("sender-failed-then-acknowledged", &lines);

        assert!(verdict.unwrap_err().contains(r#"Some("direct")"#));
    }

    #[test]
    fn spec_3_15_test_3_a_dropped_report_before_the_senders_acknowledged_line_refuses_the_sender_as_lines_lost()
     {
        let verdict = sender_path_verdict_of(
            "sender-dropped-before-acknowledged",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                dropped_report(2),
                message_acknowledged(3, MESSAGE, SERVING_CONNECTION),
            ],
        );

        let refusal = verdict.unwrap_err();
        assert!(
            refusal.contains("lines of the trace holding connection")
                && refusal.contains(&SERVING_CONNECTION.to_string()),
            "{refusal}"
        );
    }

    #[test]
    fn spec_3_15_test_3_a_dropped_report_after_the_senders_acknowledged_line_does_not_refuse_the_sender()
     {
        let verdict = sender_path_verdict_of(
            "sender-dropped-after-acknowledged",
            &[
                path_selected(1, SERVING_CONNECTION, "relay"),
                message_acknowledged(2, MESSAGE, SERVING_CONNECTION),
                dropped_report(3),
            ],
        );

        assert_eq!(verdict, Ok(()));
    }

    fn relay_added_within_of(test: &str, lines: &[String]) -> Result<(), String> {
        let (events, traces) = events_holding(test, lines);
        let verdict = relay_added_within(
            &events,
            ADDED_HOST,
            HOST_STARTED_WALL_MS,
            FIRST_RECORD_WALL_MS,
            WITHIN,
        );
        remove_traces(&traces);
        verdict
    }

    #[test]
    fn spec_3_15_test_22_a_dropped_report_before_the_line_adding_the_relay_refuses_naming_the_host_and_the_lost_lines()
     {
        let added = relay_map_edit_at(FIRST_RECORD_WALL_MS + 400, "added", ADDED_HOST);

        let verdict =
            relay_added_within_of("relay-dropped-before-added", &[dropped_report(1), added]);

        let refusal = verdict.unwrap_err();
        assert!(
            refusal.contains(&format!(
                "lines of the trace holding the relay of host {ADDED_HOST} were lost"
            )),
            "{refusal}"
        );
    }

    #[test]
    fn spec_3_15_test_22_a_dropped_report_after_the_line_adding_the_relay_does_not_refuse() {
        let added = relay_map_edit_at(FIRST_RECORD_WALL_MS + 400, "added", ADDED_HOST);

        let verdict =
            relay_added_within_of("relay-dropped-after-added", &[added, dropped_report(1)]);

        assert_eq!(verdict, Ok(()));
    }

    /// The events a member records while it runs with no rebind: a path and a counters snapshot.
    fn quiet_member_lines() -> Vec<String> {
        vec![
            path_selected(1, SERVING_CONNECTION, "relay"),
            line(2, "state.counters", r#","moment":"interval""#),
        ]
    }

    /// The events of a member that rebound once.
    fn rebound_member_lines() -> Vec<String> {
        let mut lines = quiet_member_lines();
        lines.push(line(3, "transport.rebinding", r#","cause":"deaf""#));
        lines
    }

    #[test]
    fn spec_3_15_test_24_three_members_none_of_whom_records_a_rebinding_pass() {
        let verdict = rebind_verdict_of(
            "none-rebound",
            &[
                (AMY, quiet_member_lines()),
                (BOB, quiet_member_lines()),
                (CID, quiet_member_lines()),
            ],
        );

        assert_eq!(verdict, Ok(()));
    }

    #[test]
    fn spec_3_15_test_24_one_of_three_members_recording_a_rebinding_fails_naming_it_though_it_is_not_the_first()
     {
        let verdict = rebind_verdict_of(
            "third-rebound",
            &[
                (AMY, quiet_member_lines()),
                (BOB, quiet_member_lines()),
                (CID, rebound_member_lines()),
            ],
        );

        let refusal = verdict.unwrap_err();
        assert!(
            refusal.contains(&format!("{CID} recorded 1 transport.rebinding lines")),
            "{refusal}"
        );
        assert!(
            !refusal.contains(AMY) && !refusal.contains(BOB),
            "{refusal}"
        );
    }

    #[test]
    fn spec_3_15_test_24_each_rebinding_line_of_a_member_is_counted() {
        let mut lines = rebound_member_lines();
        lines.push(line(
            4,
            "transport.rebinding",
            r#","cause":"join_gained_no_neighbor""#,
        ));

        assert_eq!(rebinding_lines_in(&parsed(&lines)), 2);
    }

    #[test]
    fn spec_3_15_test_24_a_dropped_report_anywhere_in_a_members_files_refuses_naming_it_as_lines_lost()
     {
        let mut lost_lines = quiet_member_lines();
        lost_lines.push(dropped_report(3));
        lost_lines.push(line(4, "state.counters", r#","moment":"interval""#));

        let verdict = rebind_verdict_of(
            "dropped-in-one-member",
            &[
                (AMY, quiet_member_lines()),
                (BOB, lost_lines),
                (CID, quiet_member_lines()),
            ],
        );

        let refusal = verdict.unwrap_err();
        assert!(
            refusal.contains(&format!(
                "{BOB}'s trace lost lines, so its transport.rebinding lines are not all known"
            )),
            "{refusal}"
        );
        assert!(
            !refusal.contains(AMY) && !refusal.contains(CID),
            "{refusal}"
        );
    }
}
