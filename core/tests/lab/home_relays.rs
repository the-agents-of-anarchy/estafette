use std::path::Path;
use std::time::Duration;

use estafette::dirs::TraceFileKind;
use serde_json::Value;

use crate::dropped_payloads::short_id_of_node;
use crate::live_network::LiveNetwork;
use crate::member_events::MemberEvents;
use crate::trace_reader::{TraceLine, lines_of_files_still_present, trace_files_in};

/// How long a member may take to accept a record of the live host, which sends one every 5 s: chosen, not measured.
const LIVE_HOST_RECORD_WAIT: Duration = Duration::from_secs(60);
/// The event a member records for each gossip payload it accepts, and the fields naming the payload's kind and signer (spec/observability.md decisions 6 and 9).
const PAYLOAD_ACCEPTED_EVENT: &str = "gossip.payload_accepted";
const KIND_FIELD: &str = "kind";
const PEER_FIELD: &str = "peer";
/// The kind of payload a host's infrastructure record is, as an events line names it.
const INFRASTRUCTURE_PAYLOAD_KIND: &str = "infrastructure";

/// The event of a raw line holding one of iroh's own tracing lines (spec/observability.md decision 7).
const IROH_LINE_EVENT: &str = "iroh_line";
/// The keys of a raw line holding its target and its fields as iroh words them.
const TARGET_KEY: &str = "target";
const FIELDS_KEY: &str = "fields";
/// The target of the line iroh writes when a relay becomes or stops being the home relay (iroh 1.3.0 `socket/transports/relay/actor.rs:439`).
const HOME_CHANGED_TARGET: &str = "iroh::_events::relay::home_changed";
/// The fields of that line naming the relay and whether it is now the home.
const URL_FIELD: &str = "url";
const HOME_RELAY_FIELD: &str = "home_relay";
/// How the raw file words a home flag that is set.
const HOME_RELAY_SET: &str = "true";
/// What a refusal names as the home of a member whose record names none.
const NO_HOME: &str = "none";

/// Waits until the member's own events record a record of the live host accepted, after which iroh ranks the live host's relay among the relays it may home on (spec 3.15 tests 23 and 28).
#[track_caller]
pub fn live_host_record_accepted(member_events: &MemberEvents, network: &LiveNetwork) {
    let live_host = short_id_of_node(network.host_address().id);
    member_events.wait_until(
        &format!("{PAYLOAD_ACCEPTED_EVENT} of an {INFRASTRUCTURE_PAYLOAD_KIND} record of the live host {live_host}"),
        LIVE_HOST_RECORD_WAIT,
        |lines| {
            lines
                .iter()
                .any(|line| {
                    line.event == PAYLOAD_ACCEPTED_EVENT
                        && line.value(KIND_FIELD) == Some(&Value::from(INFRASTRUCTURE_PAYLOAD_KIND))
                        && line.value(PEER_FIELD) == Some(&Value::from(live_host.as_str()))
                })
                .then_some(())
        },
    );
}

/// Reads each member's own raw trace once, by the reader handed in, and requires every one to name the relay as its home relay, the refusal naming each member that does not and the home it reports (spec 3.15 tests 23 and 28).
pub fn every_member_homed_on<'member>(
    relay: &str,
    members: impl IntoIterator<Item = (&'member str, &'member Path)>,
    mut own_raw_lines: impl FnMut(&Path) -> Vec<TraceLine>,
) -> Result<(), String> {
    let homed_elsewhere: Vec<String> = members
        .into_iter()
        .filter_map(|(member, traces)| {
            let home = home_relay_named_by(&own_raw_lines(traces));
            (home.as_deref() != Some(relay)).then(|| {
                format!(
                    "{member} reports its home relay as {}",
                    home.as_deref().unwrap_or(NO_HOME)
                )
            })
        })
        .collect();
    if homed_elsewhere.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "spec 3.15 tests 23 and 28 kill the host every member is homed on, and every member's own raw trace, read once, must name its relay {relay} as the home relay: {}",
            homed_elsewhere.join("; ")
        ))
    }
}

/// Every whole line of the raw files a running member wrote in its trace directory so far, failing the test naming the directory when they cannot be read.
#[track_caller]
pub fn raw_lines_of_running_member(traces: &Path) -> Vec<TraceLine> {
    trace_files_in(traces, TraceFileKind::Raw)
        .and_then(|files| lines_of_files_still_present(&files))
        .unwrap_or_else(|failure| panic!("{}: {failure}", traces.display()))
}

/// The relay the raw lines leave as the home relay: the last one iroh made the home, unless iroh unset it after.
fn home_relay_named_by(lines: &[TraceLine]) -> Option<String> {
    lines
        .iter()
        .filter_map(home_change_of)
        .fold(None, |home, (relay, is_home)| {
            if is_home {
                Some(relay)
            } else if home == Some(relay) {
                None
            } else {
                home
            }
        })
        .map(str::to_owned)
}

/// The relay and the home flag of a raw line iroh writes when a relay becomes or stops being the home relay; none for any other line.
fn home_change_of(line: &TraceLine) -> Option<(&str, bool)> {
    if line.event != IROH_LINE_EVENT
        || line.value(TARGET_KEY) != Some(&Value::from(HOME_CHANGED_TARGET))
    {
        return None;
    }
    let fields = line.value(FIELDS_KEY)?;
    Some((
        fields.get(URL_FIELD)?.as_str()?,
        fields.get(HOME_RELAY_FIELD)?.as_str()? == HOME_RELAY_SET,
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::path::{Path, PathBuf};

    use super::every_member_homed_on;
    use crate::trace_reader::{TraceLine, WritingProcess, parse_trace_lines};

    const DOOMED_RELAY: &str = "https://doomed.example/";
    const LIVE_RELAY: &str = "https://live.example/";
    const AMY: &str = "amyhzqv";
    const BOB: &str = "bobxkwt";
    const CID: &str = "cidpmrf";

    /// A raw line of iroh's own, of the target, with the relay URL and the home flag as iroh words them.
    fn relay_line(n: u64, target: &str, url: &str, home: &str) -> String {
        format!(
            r#"{{"v":1,"run":"6c935f82c118f7b3","n":{n},"mono_ms":{n},"wall_ms":1794524353720,"node":"8f43abc249","event":"iroh_line","target":"{target}","level":"DEBUG","fields":{{"home_relay":"{home}","url":"{url}"}}}}"#
        )
    }

    /// The raw line iroh writes when a relay becomes or stops being the home relay.
    fn home_changed(n: u64, url: &str, home: bool) -> String {
        relay_line(
            n,
            "iroh::_events::relay::home_changed",
            url,
            &home.to_string(),
        )
    }

    /// The raw line iroh writes when it connects to a relay, which says nothing of the home.
    fn connected(n: u64, url: &str) -> String {
        relay_line(n, "iroh::_events::relay::connected", url, "true")
    }

    /// The trace lines of the raw lines given.
    fn raw(lines: &[String]) -> Vec<TraceLine> {
        let text = format!("{}\n", lines.join("\n"));
        parse_trace_lines(text.as_bytes(), WritingProcess::Ended).unwrap()
    }

    /// Hands each member's records in the order given, one per read, noting each read.
    struct HandedRecords {
        records: RefCell<BTreeMap<PathBuf, VecDeque<Vec<TraceLine>>>>,
        reads: RefCell<Vec<PathBuf>>,
    }

    impl HandedRecords {
        /// The records each member's successive reads return.
        fn of(members: Vec<(&str, Vec<Vec<TraceLine>>)>) -> Self {
            Self {
                records: RefCell::new(
                    members
                        .into_iter()
                        .map(|(member, reads)| (PathBuf::from(member), reads.into()))
                        .collect(),
                ),
                reads: RefCell::new(Vec::new()),
            }
        }

        /// The member's next record, its read noted.
        fn read(&self, traces: &Path) -> Vec<TraceLine> {
            self.reads.borrow_mut().push(traces.to_path_buf());
            self.records
                .borrow_mut()
                .get_mut(traces)
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| panic!("no record left for {}", traces.display()))
        }

        /// The members whose records were read, one entry a read, in the order read.
        fn reads(&self) -> Vec<PathBuf> {
            self.reads.borrow().clone()
        }
    }

    /// The members as the check takes them: each handle with the path its records are read from.
    fn trio() -> [(&'static str, &'static Path); 3] {
        [AMY, BOB, CID].map(|member| (member, Path::new(member)))
    }

    #[test]
    fn spec_3_15_test_23_every_member_whose_own_record_names_the_doomed_relay_as_its_home_is_homed()
    {
        let handed = HandedRecords::of(vec![
            (
                AMY,
                vec![raw(&[
                    connected(1, LIVE_RELAY),
                    home_changed(2, DOOMED_RELAY, true),
                    connected(3, LIVE_RELAY),
                ])],
            ),
            (
                BOB,
                vec![raw(&[
                    home_changed(1, LIVE_RELAY, true),
                    home_changed(2, DOOMED_RELAY, true),
                    home_changed(3, LIVE_RELAY, false),
                ])],
            ),
            (CID, vec![raw(&[home_changed(1, DOOMED_RELAY, true)])]),
        ]);

        let homed = every_member_homed_on(DOOMED_RELAY, trio(), |traces| handed.read(traces));

        assert_eq!(homed, Ok(()));
    }

    #[test]
    fn spec_3_15_test_23_members_whose_own_record_names_another_home_or_none_are_refused_by_name_and_home()
     {
        let handed = HandedRecords::of(vec![
            (AMY, vec![raw(&[home_changed(1, DOOMED_RELAY, true)])]),
            (
                BOB,
                vec![raw(&[
                    home_changed(1, DOOMED_RELAY, true),
                    home_changed(2, LIVE_RELAY, true),
                    home_changed(3, DOOMED_RELAY, false),
                ])],
            ),
            (
                CID,
                vec![raw(&[
                    connected(1, DOOMED_RELAY),
                    home_changed(2, DOOMED_RELAY, true),
                    home_changed(3, DOOMED_RELAY, false),
                ])],
            ),
        ]);

        let refusal =
            every_member_homed_on(DOOMED_RELAY, trio(), |traces| handed.read(traces)).unwrap_err();

        assert!(
            refusal.ends_with(&format!(
                "must name its relay {DOOMED_RELAY} as the home relay: {BOB} reports its home relay as {LIVE_RELAY}; {CID} reports its home relay as none"
            )),
            "{refusal}"
        );
    }

    #[test]
    fn spec_3_15_test_23_each_members_own_record_is_read_once_so_a_home_a_second_read_would_show_is_not_taken()
     {
        let handed = HandedRecords::of(vec![
            (AMY, vec![raw(&[home_changed(1, DOOMED_RELAY, true)])]),
            (
                BOB,
                vec![
                    raw(&[home_changed(1, LIVE_RELAY, true)]),
                    raw(&[home_changed(1, DOOMED_RELAY, true)]),
                ],
            ),
            (CID, vec![raw(&[home_changed(1, DOOMED_RELAY, true)])]),
        ]);

        let homed = every_member_homed_on(DOOMED_RELAY, trio(), |traces| handed.read(traces));

        assert!(
            homed.as_ref().is_err_and(|refusal| refusal
                .ends_with(&format!("{BOB} reports its home relay as {LIVE_RELAY}"))),
            "{homed:?}"
        );
        assert_eq!(handed.reads(), [AMY, BOB, CID].map(PathBuf::from));
    }
}
