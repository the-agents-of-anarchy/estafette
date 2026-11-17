use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// The column separator of the lines `tail` and `show-peers` print (spec 3.13).
pub const COLUMN_SEPARATOR: char = '\t';
/// The kind word that opens a presence line of `tail` (spec 3.13).
pub const PRESENCE_KIND: &str = "presence";
/// The column of a presence line holding its card, after the kind and `v` (spec 3.7 order).
pub const PRESENCE_CARD_COLUMN: usize = 2;
/// The column of a presence line holding its `addr` as JSON (spec 3.7 order).
pub const PRESENCE_ADDR_COLUMN: usize = 6;
/// The kind word that opens an infrastructure line of `tail` (spec 3.13).
pub const INFRASTRUCTURE_KIND: &str = "infrastructure";
/// The key of the relay list in a printed `addr`.
pub const RELAYS_KEY: &str = "relays";

/// How long `tail` may take to print its first payload, a host record every 5 seconds once joined (spec 3.14).
const FIRST_LINE_WAIT: Duration = Duration::from_secs(60);

/// A running `estafette tail`, its payload lines read as they arrive, each with the instant it arrived.
pub struct Tail {
    process: Child,
    lines: Receiver<(Instant, String)>,
}

impl Tail {
    /// Starts the command, a `tail` of the network, owning it before anything can fail so a failure ends it, and waits for its first payload line, the sign it has joined the topic, leaving that line to be read.
    #[track_caller]
    pub fn start(mut command: Command) -> Self {
        let (forward, lines) = mpsc::channel();
        let mut tail = Self {
            process: command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            lines,
        };
        let (announce_joined, joined) = mpsc::sync_channel(1);
        let output = BufReader::new(tail.process.stdout.take().unwrap());
        std::thread::spawn(move || {
            let mut announce_joined = Some(announce_joined);
            for line in output.lines().map_while(Result::ok) {
                if let Some(joined) = announce_joined.take()
                    && joined.send(()).is_err()
                {
                    return;
                }
                if forward.send((Instant::now(), line)).is_err() {
                    return;
                }
            }
        });
        assert!(
            joined.recv_timeout(FIRST_LINE_WAIT).is_ok(),
            "tail printed no payload within {FIRST_LINE_WAIT:?}"
        );
        tail
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

impl Drop for Tail {
    fn drop(&mut self) {
        drop(self.process.kill());
        drop(self.process.wait());
    }
}
