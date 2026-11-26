use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use estafette::dirs::TraceFileKind;
use estafette::event::{EventName, TraceLineName};
use estafette_trace::DROPPED_LINE_NAME;
use iroh::EndpointId;
use serde_json::Value;

use crate::events_lines::{PAYLOAD_ACCEPTED_EVENT, is_accepted_payload_of, short_id_of_node};
use crate::file_growth::{GrowthWatch, WaitEnded, first_found_as_files_grow};
use crate::killable_host::KillInstant;
use crate::member_events::MemberEvents;
use crate::trace_reader::{TraceLine, lines_of_files_still_present, trace_files_in};

/// How long a member may take to accept a record of the live host, which sends one every 5 s: chosen, not measured.
const LIVE_HOST_RECORD_WAIT: Duration = Duration::from_secs(60);
/// The kind of payload a host's infrastructure record is, as an events line names it.
const INFRASTRUCTURE_PAYLOAD_KIND: &str = "infrastructure";

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
/// How long a member's own raw trace may take, after the kill, to hold a line stamped after it, iroh writing lines as the killed relay's connection fails: chosen, not measured.
const RAW_LINE_PAST_THE_KILL_WAIT: Duration = Duration::from_secs(60);

/// Waits until the member's own events record a record of the live host accepted, after which iroh ranks the live host's relay among the relays it may home on (spec 3.15 tests 23 and 28).
#[track_caller]
pub fn live_host_record_accepted(member_events: &MemberEvents, live_host_id: EndpointId) {
    let live_host = short_id_of_node(live_host_id);
    member_events.wait_until(
        &format!("{PAYLOAD_ACCEPTED_EVENT} of an {INFRASTRUCTURE_PAYLOAD_KIND} record of the live host {live_host}"),
        LIVE_HOST_RECORD_WAIT,
        |lines| {
            lines
                .iter()
                .any(|line| is_accepted_payload_of(line, INFRASTRUCTURE_PAYLOAD_KIND, &live_host))
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
    refused_naming_each(&homed_elsewhere, || {
        format!(
            "spec 3.15 tests 23 and 28 kill the host every member is homed on, and every member's own raw trace, read once, must name its relay {relay} as the home relay"
        )
    })
}

/// Accepts when no member is refused, else refuses with the rule the members broke, then each refused member.
fn refused_naming_each(
    refused_members: &[String],
    rule_broken: impl FnOnce() -> String,
) -> Result<(), String> {
    if refused_members.is_empty() {
        Ok(())
    } else {
        Err(format!("{}: {}", rule_broken(), refused_members.join("; ")))
    }
}

/// Requires every member's home at the kill, by the home changes its own raw trace stamps at or before the kill's wall-clock instant, to be the killed host's relay, reading each member's trace once by the reader handed in, the refusal naming each member homed elsewhere and the home it held, or whose trace lost lines that could hold such a change (spec 3.15 tests 23 and 28).
pub fn every_member_homed_at_the_kill_on<'member>(
    killed_relay: &str,
    kill: KillInstant,
    members: impl IntoIterator<Item = (&'member str, &'member Path)>,
    mut own_raw_lines: impl FnMut(&Path) -> Vec<TraceLine>,
) -> Result<(), String> {
    let homed_elsewhere: Vec<String> = members
        .into_iter()
        .filter_map(
            |(member, traces)| match home_at_the_kill(&own_raw_lines(traces), kill) {
                HomeAtTheKill::On(relay) if relay == killed_relay => None,
                HomeAtTheKill::On(relay) => Some(format!("{member} was homed on {relay} at the kill")),
                HomeAtTheKill::Nowhere => Some(format!("{member} was homed on {NO_HOME} at the kill")),
                HomeAtTheKill::LinesLost => Some(format!(
                    "{member}'s raw trace lost lines that could hold a home change at or before the kill"
                )),
            },
        )
        .collect();
    refused_naming_each(&homed_elsewhere, || {
        format!(
            "spec 3.15 tests 23 and 28 kill the host every member is homed on, and every member's own raw trace must name the killed relay {killed_relay} as the home relay by its home changes stamped at or before the kill's {} ms",
            kill.wall_ms()
        )
    })
}

/// Where a member's own raw trace leaves its home at the kill.
enum HomeAtTheKill {
    On(String),
    Nowhere,
    LinesLost,
}

/// The home the raw lines leave by the home changes stamped at or before the kill's wall-clock instant, unless the trace lost lines that could hold one.
fn home_at_the_kill(lines: &[TraceLine], kill: KillInstant) -> HomeAtTheKill {
    if lost_lines_could_hold_a_change_by_the_kill(lines, kill) {
        return HomeAtTheKill::LinesLost;
    }
    home_relay_named_by(lines.iter().filter(|line| line.wall_ms <= kill.wall_ms()))
        .map_or(HomeAtTheKill::Nowhere, HomeAtTheKill::On)
}

/// Whether a `dropped` line stands in file order up to and including the first queued line stamped after the kill, since a `dropped` line is stamped as written and can report lines queued before the kill.
fn lost_lines_could_hold_a_change_by_the_kill(lines: &[TraceLine], kill: KillInstant) -> bool {
    let through_the_first_queued_after_the_kill = lines
        .iter()
        .position(|line| is_queued_after_the_kill(line, kill))
        .map_or(lines.len(), |first| first.saturating_add(1));
    lines
        .iter()
        .take(through_the_first_queued_after_the_kill)
        .any(is_dropped_report)
}

/// Whether the line is the report a trace writes, before the next line it writes, of the lines it lost (`estafette_trace`'s `dropped` line).
pub fn is_dropped_report(line: &TraceLine) -> bool {
    line.event == DROPPED_LINE_NAME
}

/// Whether the raw line was stamped as it was queued, which every line but a `dropped` one is, at a wall-clock instant after the kill's.
fn is_queued_after_the_kill(line: &TraceLine, kill: KillInstant) -> bool {
    !is_dropped_report(line) && line.wall_ms > kill.wall_ms()
}

/// Every whole line of the member's own raw trace once it holds a line other than a `dropped` one stamped after the kill's wall-clock instant, which its writer puts on disk only after every line stamped at or before it, failing the test naming that condition when none comes within the bound.
#[track_caller]
pub fn raw_lines_once_stamped_past(traces: &Path, kill: KillInstant) -> Vec<TraceLine> {
    raw_lines_once_stamped_past_read_by(traces, kill, Instant::now)
}

/// As `raw_lines_once_stamped_past`, reading the time from `clock`.
#[track_caller]
fn raw_lines_once_stamped_past_read_by(
    traces: &Path,
    kill: KillInstant,
    clock: impl FnMut() -> Instant,
) -> Vec<TraceLine> {
    let mut watch = GrowthWatch::of_path(traces)
        .unwrap_or_else(|failure| panic!("{}: {failure}", traces.display()));
    let past_the_kill = first_found_as_files_grow(
        &mut watch,
        RAW_LINE_PAST_THE_KILL_WAIT,
        clock,
        || {},
        |watch| {
            let files = raw_files_of_running_member(traces);
            for file in &files {
                watch
                    .watch_path(file)
                    .unwrap_or_else(|failure| panic!("{}: {failure}", traces.display()));
            }
            let lines = raw_lines_of_files(traces, &files);
            lines
                .iter()
                .any(|line| is_queued_after_the_kill(line, kill))
                .then_some(lines)
        },
    );
    match past_the_kill {
        Ok(lines) => lines,
        Err(WaitEnded::RanOut) => panic!(
            "{}: no raw line other than a {DROPPED_LINE_NAME} line stamped after the kill's {} ms came within {RAW_LINE_PAST_THE_KILL_WAIT:?}",
            traces.display(),
            kill.wall_ms()
        ),
        Err(WaitEnded::WatcherFailed(failure)) => {
            panic!("{}: the file watcher failed: {failure}", traces.display())
        }
        Err(WaitEnded::WatcherEnded) => panic!("{}: the file watcher ended", traces.display()),
    }
}

/// The raw files a running member wrote in its trace directory so far, failing the test naming the directory when it cannot be listed.
#[track_caller]
fn raw_files_of_running_member(traces: &Path) -> Vec<PathBuf> {
    trace_files_in(traces, TraceFileKind::Raw)
        .unwrap_or_else(|failure| panic!("{}: {failure}", traces.display()))
}

/// Every whole line of the raw files a running member wrote in its trace directory so far, failing the test naming the directory when they cannot be read.
#[track_caller]
pub fn raw_lines_of_running_member(traces: &Path) -> Vec<TraceLine> {
    raw_lines_of_files(traces, &raw_files_of_running_member(traces))
}

/// Every whole line of the given raw files of a running member's trace directory, failing the test naming the directory when they cannot be read.
#[track_caller]
fn raw_lines_of_files(traces: &Path, files: &[PathBuf]) -> Vec<TraceLine> {
    lines_of_files_still_present(files)
        .unwrap_or_else(|failure| panic!("{}: {failure}", traces.display()))
}

/// The relay the raw lines leave as the home relay: the last one iroh made the home, unless iroh unset it after.
fn home_relay_named_by<'line>(lines: impl IntoIterator<Item = &'line TraceLine>) -> Option<String> {
    lines
        .into_iter()
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
    if line.event != TraceLineName::IrohLine.as_str()
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
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use estafette_trace::DROPPED_LINE_NAME;

    use super::{
        HOME_CHANGED_TARGET, RAW_LINE_PAST_THE_KILL_WAIT, every_member_homed_at_the_kill_on,
        every_member_homed_on, raw_lines_once_stamped_past, raw_lines_once_stamped_past_read_by,
    };
    use crate::killable_host::KillInstant;
    use crate::scratch_machine::this_process_directory::fresh_directory_of_this_process_in;
    use crate::trace_reader::{TraceLine, WritingProcess, parse_trace_lines};

    const DOOMED_RELAY: &str = "https://doomed.example/";
    const LIVE_RELAY: &str = "https://live.example/";
    const AMY: &str = "amyhzqv";
    const BOB: &str = "bobxkwt";
    const CID: &str = "cidpmrf";
    /// The wall-clock stamp a fixture's line 0 would carry; line `n` is stamped `n` ms later, its `mono_ms` being `n` alone.
    const WALL_MS_OF_LINE_ZERO: u64 = 1_794_524_353_720;
    /// The line whose stamp is the kill's instant in the fixtures of the home at the kill: lines 1 to 4 come before it, 6 and 7 after.
    const KILL_LINE: u64 = 5;
    /// The run every fixture's lines and raw file name.
    const RUN_ID: &str = "6c935f82c118f7b3";

    /// A raw line of iroh's own, of the target, with the relay URL and the home flag as iroh words them.
    fn relay_line(n: u64, target: &str, url: &str, home: &str) -> String {
        let head = head_of_line(n);
        format!(
            r#"{head},"event":"iroh_line","target":"{target}","level":"DEBUG","fields":{{"home_relay":"{home}","url":"{url}"}}}}"#
        )
    }

    /// The opening of a fixture's raw line `n` up to its node, which every line of the run shares, the object left open.
    fn head_of_line(n: u64) -> String {
        let wall_ms = stamp_of_line(n);
        format!(
            r#"{{"v":2,"run":"{RUN_ID}","n":{n},"mono_ms":{n},"wall_ms":{wall_ms},"node":"8f43abc249""#
        )
    }

    /// The wall-clock stamp of a fixture's line `n`.
    const fn stamp_of_line(n: u64) -> u64 {
        WALL_MS_OF_LINE_ZERO.saturating_add(n)
    }

    /// The kill's instant of a fixture that kills at the stamp of its kill line.
    const fn kill_instant() -> KillInstant {
        KillInstant::at_wall_ms(stamp_of_line(KILL_LINE))
    }

    /// The raw line iroh writes when a relay becomes or stops being the home relay.
    fn home_changed(n: u64, url: &str, home: bool) -> String {
        relay_line(n, HOME_CHANGED_TARGET, url, &home.to_string())
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

    #[test]
    fn spec_3_15_tests_23_and_28_members_whose_last_home_change_at_or_before_the_kill_names_the_killed_relay_were_homed_on_it()
     {
        let homed = homed_at_the_kill(vec![
            (
                AMY,
                vec![
                    home_changed(1, LIVE_RELAY, true),
                    home_changed(2, DOOMED_RELAY, true),
                    connected(3, LIVE_RELAY),
                    connected(6, LIVE_RELAY),
                ],
            ),
            (BOB, vec![home_changed(4, DOOMED_RELAY, true)]),
        ]);

        assert_eq!(homed, Ok(()));
    }

    /// Whether each member was homed on the doomed relay at a kill at the stamp of the kill line, by the one raw record each member is handed.
    fn homed_at_the_kill(members: Vec<(&str, Vec<String>)>) -> Result<(), String> {
        let names: Vec<&str> = members.iter().map(|(member, _lines)| *member).collect();
        let handed = HandedRecords::of(
            members
                .into_iter()
                .map(|(member, lines)| (member, vec![raw(&lines)]))
                .collect(),
        );
        every_member_homed_at_the_kill_on(
            DOOMED_RELAY,
            kill_instant(),
            names.iter().map(|member| (*member, Path::new(member))),
            |traces| handed.read(traces),
        )
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_member_rehomed_on_the_living_relay_before_the_kill_is_refused_by_name_and_home()
     {
        let homed = homed_at_the_kill(vec![
            (AMY, vec![home_changed(1, DOOMED_RELAY, true)]),
            (
                BOB,
                vec![
                    home_changed(1, DOOMED_RELAY, true),
                    home_changed(3, LIVE_RELAY, true),
                ],
            ),
        ]);

        assert_eq!(
            homed,
            Err(format!(
                "spec 3.15 tests 23 and 28 kill the host every member is homed on, and every member's own raw trace must name the killed relay {DOOMED_RELAY} as the home relay by its home changes stamped at or before the kill's {} ms: {BOB} was homed on {LIVE_RELAY} at the kill",
                stamp_of_line(KILL_LINE)
            ))
        );
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_member_rehomed_on_the_living_relay_after_the_kill_was_homed_on_the_killed_relay()
     {
        let homed = homed_at_the_kill(vec![(
            AMY,
            vec![
                home_changed(1, DOOMED_RELAY, true),
                home_changed(6, LIVE_RELAY, true),
                home_changed(7, DOOMED_RELAY, false),
            ],
        )]);

        assert_eq!(homed, Ok(()));
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_home_change_stamped_at_the_kills_instant_counts_as_before_the_kill()
     {
        let homed = homed_at_the_kill(vec![(
            AMY,
            vec![
                home_changed(1, DOOMED_RELAY, true),
                home_changed(KILL_LINE, LIVE_RELAY, true),
            ],
        )]);

        assert!(
            homed.as_ref().is_err_and(|refusal| refusal
                .ends_with(&format!("{AMY} was homed on {LIVE_RELAY} at the kill"))),
            "{homed:?}"
        );
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_member_with_no_home_change_before_the_kill_is_refused_as_homed_nowhere()
     {
        let homed = homed_at_the_kill(vec![(
            AMY,
            vec![
                connected(1, DOOMED_RELAY),
                home_changed(6, DOOMED_RELAY, true),
            ],
        )]);

        assert!(
            homed.as_ref().is_err_and(
                |refusal| refusal.ends_with(&format!("{AMY} was homed on none at the kill"))
            ),
            "{homed:?}"
        );
    }

    /// Requires the refusal to end by naming the member's raw trace as having lost lines that could hold a home change at or before the kill.
    #[track_caller]
    fn assert_refused_for_lost_lines(homed: &Result<(), String>, member: &str) {
        assert!(
            homed.as_ref().is_err_and(|refusal| refusal.ends_with(&format!(
                "{member}'s raw trace lost lines that could hold a home change at or before the kill"
            ))),
            "{homed:?}"
        );
    }

    /// The text the wait fails with when no raw line other than a `dropped` one stamped after the kill line comes in the trace directory.
    fn no_raw_line_past_the_kill_refusal(traces: &Path) -> String {
        format!(
            "{}: no raw line other than a {DROPPED_LINE_NAME} line stamped after the kill's {} ms came within {RAW_LINE_PAST_THE_KILL_WAIT:?}",
            traces.display(),
            stamp_of_line(KILL_LINE)
        )
    }

    /// A raw trace file's name, as a member's run names its raw file.
    fn raw_file_name() -> String {
        format!("20261112T225914Z-{RUN_ID}-estafette.raw.jsonl")
    }

    /// A fresh trace directory named for the test, holding one raw file of the lines given.
    fn traces_holding(test: &str, lines: &[String]) -> PathBuf {
        let traces = fresh_directory_of_this_process_in(
            &std::env::temp_dir(),
            &format!("home-relays-{test}"),
        )
        .unwrap();
        fs::write(
            traces.join(raw_file_name()),
            format!("{}\n", lines.join("\n")),
        )
        .unwrap();
        traces
    }

    #[test]
    fn spec_3_15_tests_23_and_28_once_a_raw_line_stamped_after_the_kill_is_on_disk_every_line_is_read()
     {
        let lines = [
            home_changed(1, DOOMED_RELAY, true),
            connected(KILL_LINE, LIVE_RELAY),
            home_changed(6, LIVE_RELAY, true),
        ];
        let traces = traces_holding("past-the-kill", &lines);

        let read = raw_lines_once_stamped_past(&traces, kill_instant());
        fs::remove_dir_all(&traces).unwrap();

        assert_eq!(
            read.iter().map(|line| line.n).collect::<Vec<_>>(),
            [1, KILL_LINE, 6]
        );
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_raw_line_stamped_at_the_kills_instant_does_not_end_the_wait() {
        let traces = traces_holding(
            "at-the-kill",
            &[
                home_changed(1, DOOMED_RELAY, true),
                connected(KILL_LINE, LIVE_RELAY),
            ],
        );
        let started = Instant::now();
        let past_the_bound = started.checked_add(RAW_LINE_PAST_THE_KILL_WAIT).unwrap();

        let ran_out = wait_past_the_kill_line(&traces, started, past_the_bound);
        fs::remove_dir_all(&traces).unwrap();

        assert_eq!(
            ran_out,
            Err(Some(no_raw_line_past_the_kill_refusal(&traces)))
        );
    }

    /// The line a member's raw trace writes, stamped as it is written, for lines its full queue lost before then.
    fn dropped(n: u64) -> String {
        let head = head_of_line(n);
        format!(r#"{head},"event":"{DROPPED_LINE_NAME}","count":3}}"#)
    }

    /// The numbers of the raw lines the wait past a kill at the kill line returns from the trace directory, its clock reading `started` and then `past_the_bound`, or the text it fails with.
    fn wait_past_the_kill_line(
        traces: &Path,
        started: Instant,
        past_the_bound: Instant,
    ) -> Result<Vec<u64>, Option<String>> {
        let mut readings = std::iter::once(started);
        std::panic::catch_unwind(|| {
            raw_lines_once_stamped_past_read_by(traces, kill_instant(), move || {
                readings.next().unwrap_or(past_the_bound)
            })
            .iter()
            .map(|line| line.n)
            .collect()
        })
        .map_err(|payload| payload.downcast_ref::<String>().cloned())
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_dropped_line_stamped_after_the_kill_does_not_end_the_wait_and_an_ordinary_one_does()
     {
        let only_dropped_past_the_kill = traces_holding(
            "dropped-past-the-kill",
            &[home_changed(1, DOOMED_RELAY, true), dropped(6)],
        );
        let ordinary_past_the_kill = traces_holding(
            "ordinary-past-the-kill",
            &[
                home_changed(1, DOOMED_RELAY, true),
                dropped(6),
                connected(7, LIVE_RELAY),
            ],
        );
        let started = Instant::now();
        let past_the_bound = started.checked_add(RAW_LINE_PAST_THE_KILL_WAIT).unwrap();

        let ended_by_the_dropped_line =
            wait_past_the_kill_line(&only_dropped_past_the_kill, started, past_the_bound);
        let ended_by_the_ordinary_line =
            wait_past_the_kill_line(&ordinary_past_the_kill, started, past_the_bound);
        fs::remove_dir_all(&only_dropped_past_the_kill).unwrap();
        fs::remove_dir_all(&ordinary_past_the_kill).unwrap();

        assert_eq!(
            ended_by_the_dropped_line,
            Err(Some(no_raw_line_past_the_kill_refusal(
                &only_dropped_past_the_kill
            )))
        );
        assert_eq!(ended_by_the_ordinary_line, Ok(vec![1, 6, 7]));
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_dropped_line_before_the_last_line_stamped_by_the_kill_is_refused_as_lines_lost()
     {
        let homed = homed_at_the_kill(vec![
            (AMY, vec![home_changed(1, DOOMED_RELAY, true)]),
            (
                BOB,
                vec![
                    dropped(2),
                    connected(3, DOOMED_RELAY),
                    connected(6, DOOMED_RELAY),
                ],
            ),
        ]);

        assert_eq!(
            homed,
            Err(format!(
                "spec 3.15 tests 23 and 28 kill the host every member is homed on, and every member's own raw trace must name the killed relay {DOOMED_RELAY} as the home relay by its home changes stamped at or before the kill's {} ms: {BOB}'s raw trace lost lines that could hold a home change at or before the kill",
                stamp_of_line(KILL_LINE)
            ))
        );
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_dropped_line_stamped_after_the_kill_before_the_first_ordinary_line_after_it_is_refused()
     {
        let homed = homed_at_the_kill(vec![(
            AMY,
            vec![
                home_changed(1, DOOMED_RELAY, true),
                dropped(6),
                connected(7, LIVE_RELAY),
            ],
        )]);

        assert_refused_for_lost_lines(&homed, AMY);
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_dropped_line_after_the_first_ordinary_line_stamped_after_the_kill_refuses_nothing()
     {
        let homed = homed_at_the_kill(vec![(
            AMY,
            vec![
                home_changed(1, DOOMED_RELAY, true),
                connected(6, LIVE_RELAY),
                dropped(7),
            ],
        )]);

        assert_eq!(homed, Ok(()));
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_member_with_lost_lines_whose_other_lines_name_the_killed_relay_is_still_refused()
     {
        let homed = homed_at_the_kill(vec![(
            AMY,
            vec![
                dropped(1),
                home_changed(2, DOOMED_RELAY, true),
                connected(6, LIVE_RELAY),
            ],
        )]);

        assert_refused_for_lost_lines(&homed, AMY);
    }

    #[test]
    fn spec_3_15_tests_23_and_28_a_dropped_line_in_a_trace_with_no_ordinary_line_after_the_kill_is_refused_as_lines_lost()
     {
        let homed = homed_at_the_kill(vec![(
            AMY,
            vec![
                home_changed(1, DOOMED_RELAY, true),
                connected(3, DOOMED_RELAY),
                dropped(6),
            ],
        )]);

        assert_refused_for_lost_lines(&homed, AMY);
    }
}
