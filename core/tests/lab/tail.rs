use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// How long `tail` may take to print its first payload, a host record every 5 seconds once joined (spec 3.14).
const FIRST_LINE_WAIT: Duration = Duration::from_secs(60);

/// A running `estafette tail`, its payload lines read as they arrive, each with the instant it arrived.
pub struct Tail {
    process: Child,
    lines: Receiver<(Instant, String)>,
}

impl Tail {
    /// Starts the command, a `tail` of the network, and waits for its first payload line, the sign it has joined the topic, leaving that line to be read.
    #[track_caller]
    pub fn start(mut command: Command) -> Self {
        let mut process = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (forward, lines) = mpsc::channel();
        let (announce_joined, joined) = mpsc::sync_channel(1);
        let output = BufReader::new(process.stdout.take().unwrap());
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
        Self { process, lines }
    }

    /// Every line printed until the first that meets the condition, that one included, requiring it within the wait.
    #[track_caller]
    pub fn lines_until(&self, within: Duration, condition: impl Fn(&str) -> bool) -> Vec<String> {
        let deadline = Instant::now().checked_add(within).unwrap();
        let mut printed = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok((_arrived, line)) = self.lines.recv_timeout(remaining) else {
                panic!("no wanted line within {within:?}; printed: {printed:?}")
            };
            let wanted = condition(&line);
            printed.push(line);
            if wanted {
                return printed;
            }
        }
    }

    /// Every line printed from now until the span ends.
    pub fn lines_during(&self, span: Duration) -> Vec<String> {
        let deadline = Instant::now().checked_add(span).unwrap();
        let mut printed = Vec::new();
        while let Ok((_arrived, line)) = self
            .lines
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            printed.push(line);
        }
        printed
    }

    /// Waits until no line meeting the condition has been printed for the quiet span, the span restarting at each such line, requiring the quiet within the wait.
    #[track_caller]
    pub fn quiet_for(&self, quiet: Duration, within: Duration, condition: impl Fn(&str) -> bool) {
        let deadline = Instant::now().checked_add(within).unwrap();
        let mut quiet_from = Instant::now();
        loop {
            let quiet_ends = quiet_from.checked_add(quiet).unwrap();
            assert!(
                quiet_ends <= deadline,
                "no span of {quiet:?} without a wanted line within {within:?}"
            );
            match self
                .lines
                .recv_timeout(quiet_ends.saturating_duration_since(Instant::now()))
            {
                Ok((arrived, line)) if condition(&line) => quiet_from = arrived,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }

    /// The first line printed at or after the instant that meets the condition, with how long after the instant it arrived, requiring it within the wait.
    #[track_caller]
    pub fn first_line_after(
        &self,
        since: Instant,
        within: Duration,
        condition: impl Fn(&str) -> bool,
    ) -> (String, Duration) {
        let deadline = since.checked_add(within).unwrap();
        let mut printed_since = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok((arrived, line)) = self.lines.recv_timeout(remaining) else {
                panic!(
                    "no wanted line within {within:?} of the instant; printed since: {printed_since:#?}"
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
