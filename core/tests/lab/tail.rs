use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::scratch_machine::ScratchMachine;
use crate::scratch_machine::owned_process::OwnedProcess;

/// The column separator of the lines `tail` and `show-peers` print (spec 3.13).
pub const COLUMN_SEPARATOR: char = '\t';
/// The kind word that opens a presence line of `tail` (spec 3.13).
pub const PRESENCE_KIND: &str = "presence";
/// The column of a presence line holding its card, after the kind and `v` (spec 3.7 order).
pub const PRESENCE_CARD_COLUMN: usize = 2;
/// The column of a presence line holding its `addr` as JSON (spec 3.7 order).
pub const PRESENCE_ADDR_COLUMN: usize = 6;
/// The column of a presence line holding its seq.
pub const PRESENCE_SEQ_COLUMN: usize = 7;
/// The column of a `show-peers` line holding the peer's status, after its handle (spec 3.13).
pub const PEER_STATUS_COLUMN: usize = 1;
/// The kind word that opens an infrastructure line of `tail` (spec 3.13).
pub const INFRASTRUCTURE_KIND: &str = "infrastructure";
/// The column of an infrastructure line holding the host's node id.
pub const RECORD_HOST_COLUMN: usize = 2;
/// The column of an infrastructure line holding its `relay`, after the kind, `v` and `host` (spec 3.7 order).
pub const HOST_RELAY_COLUMN: usize = 3;
/// The column of an infrastructure line holding the record's ts.
pub const RECORD_TS_COLUMN: usize = 5;
/// The key of the relay list in a printed `addr`.
const RELAYS_KEY: &str = "relays";

/// How long `tail` may take to print its first payload, a host record every 5 seconds once joined (spec 3.14).
const FIRST_LINE_WAIT: Duration = Duration::from_secs(60);
/// How a failed end of a `tail` process is marked beside what its machine keeps.
const TAIL_LABEL: &str = "tail";

/// The columns of a `tail` line that opens with the kind word, none for a line of another kind.
pub fn columns_of_kind<'line>(line: &'line str, kind: &str) -> Option<Vec<&'line str>> {
    let columns: Vec<&str> = line.split(COLUMN_SEPARATOR).collect();
    (columns.first() == Some(&kind)).then_some(columns)
}

/// The relay URLs a printed `addr` column lists, none when it is no JSON object whose relays are a list of strings.
pub fn announced_relays(addr_column: &str) -> Option<Vec<String>> {
    let addr: Value = serde_json::from_str(addr_column).ok()?;
    addr.get(RELAYS_KEY)?
        .as_array()?
        .iter()
        .map(|relay| relay.as_str().map(str::to_owned))
        .collect()
}

/// A running `estafette tail`, its payload lines read as they arrive, each with the instant it arrived.
pub struct Tail {
    _process: OwnedProcess,
    lines: Receiver<(Instant, String)>,
}

impl Tail {
    /// Starts the command, a `tail` of the network, on the observer's machine, owning it before anything can fail so a failure ends it, and waits for its first payload line, the sign it has joined the topic, leaving that line to be read.
    #[track_caller]
    pub fn start(observer: &ScratchMachine, mut command: Command) -> Self {
        let (process, lines) = observer.start_printing_lines(
            command.stdin(Stdio::null()).stderr(Stdio::null()),
            TAIL_LABEL,
            FIRST_LINE_WAIT,
        );
        Self {
            _process: process,
            lines,
        }
    }

    /// The next line printed with the instant it arrived, none when no line arrives before the deadline.
    fn next_line_before(&self, deadline: Instant) -> Option<(Instant, String)> {
        self.lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
    }

    /// Every line printed until the first that meets the condition, that one included, requiring it within the wait and naming what was awaited when it does not come.
    #[track_caller]
    pub fn lines_until(
        &self,
        awaited: &str,
        within: Duration,
        condition: impl Fn(&str) -> bool,
    ) -> Vec<String> {
        let deadline = Instant::now().checked_add(within).unwrap();
        let mut printed = Vec::new();
        loop {
            let Some((_arrived, line)) = self.next_line_before(deadline) else {
                panic!("no {awaited} within {within:?}; printed: {printed:?}")
            };
            let wanted = condition(&line);
            printed.push(line);
            if wanted {
                return printed;
            }
        }
    }

    /// Every line printed so far and not yet read, waiting for nothing.
    pub fn lines_printed_so_far(&self) -> Vec<String> {
        self.lines.try_iter().map(|(_arrived, line)| line).collect()
    }

    /// The first line printed at or after the instant that meets the condition, with how long after the instant it arrived, requiring it within the wait and naming what was awaited when it does not come.
    #[track_caller]
    pub fn first_line_after(
        &self,
        awaited: &str,
        since: Instant,
        within: Duration,
        condition: impl Fn(&str) -> bool,
    ) -> (String, Duration) {
        let deadline = since.checked_add(within).unwrap();
        let mut printed_since = Vec::new();
        loop {
            let Some((arrived, line)) = self.next_line_before(deadline) else {
                panic!(
                    "no {awaited} within {within:?} of the instant; printed since: {printed_since:#?}"
                )
            };
            if arrived < since {
                continue;
            }
            if condition(&line) {
                return (line, arrived.duration_since(since));
            }
            printed_since.push(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{INFRASTRUCTURE_KIND, PRESENCE_KIND, announced_relays, columns_of_kind};

    #[test]
    fn spec_3_13_a_line_opening_with_the_kind_word_yields_its_columns_in_order() {
        let line = format!("{PRESENCE_KIND}\t1\tcard\thandle");

        assert_eq!(
            columns_of_kind(&line, PRESENCE_KIND),
            Some(vec![PRESENCE_KIND, "1", "card", "handle"])
        );
    }

    #[test]
    fn spec_3_13_a_line_opening_with_another_kind_word_yields_no_columns() {
        let line = format!("{INFRASTRUCTURE_KIND}\t1\thost");

        assert_eq!(columns_of_kind(&line, PRESENCE_KIND), None);
    }

    #[test]
    fn spec_3_7_a_printed_addr_listing_relays_yields_them_in_order() {
        let addr = r#"{"relays":["https://first.example/","https://second.example/"],"direct":[]}"#;

        assert_eq!(
            announced_relays(addr),
            Some(vec![
                "https://first.example/".to_owned(),
                "https://second.example/".to_owned()
            ])
        );
    }

    #[test]
    fn spec_3_7_a_printed_addr_whose_relays_are_not_all_strings_yields_none() {
        let addr = r#"{"relays":["https://first.example/",7],"direct":[]}"#;

        assert_eq!(announced_relays(addr), None);
    }

    #[test]
    fn spec_3_7_a_printed_addr_without_relays_or_without_json_yields_none() {
        assert_eq!(announced_relays(r#"{"direct":[]}"#), None);
        assert_eq!(announced_relays("not json"), None);
    }
}
