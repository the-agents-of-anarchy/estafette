use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// How long `tail` may take to print its first payload, a host record every 5 seconds once joined (spec 3.14).
const FIRST_LINE_WAIT: Duration = Duration::from_secs(60);

/// A running `estafette tail`, its payload lines read as they arrive.
pub struct Tail {
    process: Child,
    lines: Receiver<String>,
}

impl Tail {
    /// Starts the command, a `tail` of the network, and waits for its first payload line, the sign it has joined the topic.
    pub fn start(mut command: Command) -> Self {
        let mut process = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let (forward, lines) = mpsc::channel();
        let output = BufReader::new(process.stdout.take().unwrap());
        std::thread::spawn(move || {
            for line in output.lines().map_while(Result::ok) {
                if forward.send(line).is_err() {
                    return;
                }
            }
        });
        let tail = Self { process, lines };
        tail.lines
            .recv_timeout(FIRST_LINE_WAIT)
            .unwrap_or_else(|_| panic!("tail printed no payload within {FIRST_LINE_WAIT:?}"));
        tail
    }

    /// Every line printed until the first that meets the condition, that one included, requiring it within the wait.
    pub fn lines_until(&self, within: Duration, condition: impl Fn(&str) -> bool) -> Vec<String> {
        let deadline = Instant::now().checked_add(within).unwrap();
        let mut printed = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(remaining).unwrap_or_else(|_| {
                panic!("no wanted line within {within:?}; printed: {printed:?}")
            });
            let wanted = condition(&line);
            printed.push(line);
            if wanted {
                return printed;
            }
        }
    }
}

impl Drop for Tail {
    fn drop(&mut self) {
        drop(self.process.kill());
        drop(self.process.wait());
    }
}
