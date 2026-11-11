use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use super::files::create_private_file;
use super::line::HeaderSource;
use crate::dirs::rolled_over_trace_file;
use crate::event::{EventName, TraceLineName};

const DROPPED_COUNT_FIELD: &str = "count";
const WRITER_THREAD_NAME: &str = "estafette-trace-writer";

/// Where a trace writer's thread puts each line.
pub(super) trait TraceOutput {
    /// Writes the line, newline included, whole in one write.
    fn write_line(&mut self, line: &[u8]) -> io::Result<()>;
}

/// What each file a rollover starts holds before the line that rolled the file over.
pub(super) enum RolloverOpening {
    /// Nothing.
    Nothing,
    /// The first line the trace file held.
    FirstLineAgain,
}

/// A trace file that rolls over into its `.1` file, replacing any earlier one, before a line would take it past its size bound.
pub(super) struct RollingFile {
    path: PathBuf,
    file: File,
    written_bytes: u64,
    rollover_bytes: u64,
    opening: RolloverOpening,
    first_line: Option<Vec<u8>>,
}

impl RollingFile {
    /// The newly created, empty file at `path`, rolled over at `rollover_bytes` into a file that starts with `opening`.
    pub(super) const fn new(
        path: PathBuf,
        file: File,
        rollover_bytes: u64,
        opening: RolloverOpening,
    ) -> Self {
        Self {
            path,
            file,
            written_bytes: 0,
            rollover_bytes,
            opening,
            first_line: None,
        }
    }

    /// Renames the file to its `.1` file and starts one in its place holding its opening.
    fn roll_over(&mut self) -> io::Result<()> {
        fs::rename(&self.path, rolled_over_trace_file(&self.path))?;
        self.file = create_private_file(&self.path)?;
        self.written_bytes = 0;
        match (&self.opening, &self.first_line) {
            (RolloverOpening::FirstLineAgain, Some(first_line)) => {
                self.file.write_all(first_line)?;
                self.written_bytes = byte_count(first_line);
            }
            (RolloverOpening::FirstLineAgain, None) | (RolloverOpening::Nothing, _) => {}
        }
        Ok(())
    }
}

impl TraceOutput for RollingFile {
    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let line_bytes = byte_count(line);
        if self.written_bytes > 0
            && self.written_bytes.saturating_add(line_bytes) > self.rollover_bytes
        {
            self.roll_over()?;
        }
        self.file.write_all(line)?;
        if self.first_line.is_none() {
            self.first_line = Some(line.to_vec());
        }
        self.written_bytes = self.written_bytes.saturating_add(line_bytes);
        Ok(())
    }
}

/// The bytes a line takes on disk.
fn byte_count(line: &[u8]) -> u64 {
    u64::try_from(line.len()).unwrap_or(u64::MAX)
}

/// One trace file's writer: a bounded queue to a thread of its own, so a caller never waits for the disk.
pub(super) struct TraceWriter {
    sequence: Mutex<Sequence>,
    header: Arc<HeaderSource>,
    ended: Mutex<Option<Receiver<()>>>,
}

/// The next line number, the events a full queue lost since the last line queued, and the queue while it is open.
struct Sequence {
    next_n: u64,
    dropped: u64,
    queue: Option<SyncSender<String>>,
}

impl TraceWriter {
    /// Starts the writer thread on the output behind a queue of at most `queue_lines` lines.
    pub(super) fn start(
        output: impl TraceOutput + Send + 'static,
        header: Arc<HeaderSource>,
        queue_lines: usize,
    ) -> io::Result<Self> {
        let (queue, lines) = mpsc::sync_channel(queue_lines);
        let (ended_sender, ended) = mpsc::channel();
        thread::Builder::new()
            .name(WRITER_THREAD_NAME.to_owned())
            .spawn(move || write_every_line(output, &lines, ended_sender))?;
        Ok(Self {
            sequence: Mutex::new(Sequence {
                next_n: 0,
                dropped: 0,
                queue: Some(queue),
            }),
            header,
            ended: Mutex::new(Some(ended)),
        })
    }

    /// Queues the event's line, or counts it lost when the queue is full; the count of earlier losses is queued first, as a `dropped` line.
    pub(super) fn write(&self, name: &str, fields: &[(&str, Value)]) {
        let mut sequence = self.sequence.lock().unwrap_or_else(PoisonError::into_inner);
        if sequence.dropped > 0 {
            let lost = [(DROPPED_COUNT_FIELD, Value::from(sequence.dropped))];
            if self.try_queue(&mut sequence, TraceLineName::Dropped.as_str(), &lost) {
                sequence.dropped = 0;
            }
        }
        if !self.try_queue(&mut sequence, name, fields) {
            sequence.next_n = sequence.next_n.saturating_add(1);
            sequence.dropped = sequence.dropped.saturating_add(1);
        }
    }

    /// Closes the queue and waits at most `within` for the thread to write every line it holds, saying whether it did.
    pub(super) fn finish(&self, within: Duration) -> bool {
        drop(
            self.sequence
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .queue
                .take(),
        );
        self.ended
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .is_none_or(|ended| {
                matches!(
                    ended.recv_timeout(within),
                    Err(RecvTimeoutError::Disconnected)
                )
            })
    }

    /// Queues the line numbered with the next number, taking that number only when the line is queued.
    fn try_queue(&self, sequence: &mut Sequence, name: &str, fields: &[(&str, Value)]) -> bool {
        let line = self.header.line(sequence.next_n, name, fields);
        let queued = sequence
            .queue
            .as_ref()
            .is_some_and(|queue| queue.try_send(line).is_ok());
        if queued {
            sequence.next_n = sequence.next_n.saturating_add(1);
        }
        queued
    }
}

/// Writes each queued line to the output until the queue closes, then drops `ended` to say so.
fn write_every_line(mut output: impl TraceOutput, lines: &Receiver<String>, ended: Sender<()>) {
    for line in lines {
        drop(output.write_line(line.as_bytes()));
    }
    drop(ended);
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, Sender};
    use std::time::Duration;

    use serde_json::Value;

    use super::*;
    use crate::trace::tests::frozen_clocks;
    use crate::trace::{RUN_ID_BYTES, RunId};

    const SMALL_QUEUE_LINES: usize = 2;
    const EVENT_NAME: &str = "process.clock_before_unix_epoch";
    const WALL_MS: u64 = 1_800_000_000_000;
    const WRITER_STEP_BOUND: Duration = Duration::from_secs(10);
    const SPEC_DRAIN_BOUND: Duration = Duration::from_secs(10);

    /// An output the test holds back: each write announces itself, waits for the test's release, then hands the line over.
    struct HeldBackOutput {
        entered: Sender<()>,
        released: Receiver<()>,
        written: Sender<String>,
    }

    impl TraceOutput for HeldBackOutput {
        fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
            self.entered.send(()).map_err(io::Error::other)?;
            self.released.recv().map_err(io::Error::other)?;
            self.written
                .send(String::from_utf8_lossy(line).into_owned())
                .map_err(io::Error::other)
        }
    }

    fn within_bound<T>(receiver: &Receiver<T>, condition: &str) -> T {
        receiver
            .recv_timeout(WRITER_STEP_BOUND)
            .unwrap_or_else(|failure| panic!("{condition} within {WRITER_STEP_BOUND:?}: {failure}"))
    }

    fn written_line(
        written: &Receiver<String>,
        released: &Sender<()>,
    ) -> serde_json::Map<String, Value> {
        released.send(()).unwrap();
        match serde_json::from_str(&within_bound(written, "a released line is written")).unwrap() {
            Value::Object(line) => line,
            other => panic!("a JSON object, not {other}"),
        }
    }

    #[test]
    fn observability_decision_12_a_full_queue_counts_each_lost_event_and_the_next_line_is_a_dropped_event_where_n_skips()
     {
        let (entered, entering) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (written_sender, written) = mpsc::channel();
        let writer = TraceWriter::start(
            HeldBackOutput {
                entered,
                released,
                written: written_sender,
            },
            Arc::new(HeaderSource::new(
                RunId::from_random_bytes([0; RUN_ID_BYTES]),
                frozen_clocks(WALL_MS),
            )),
            SMALL_QUEUE_LINES,
        )
        .unwrap();

        writer.write(EVENT_NAME, &[]);
        within_bound(&entering, "the writer holds the first line");
        for _queued_or_lost in 0..4 {
            writer.write(EVENT_NAME, &[]);
        }
        let held: Vec<_> = (0..3)
            .map(|_line| written_line(&written, &release))
            .collect();
        writer.write(EVENT_NAME, &[]);
        let after: Vec<_> = (0..2)
            .map(|_line| written_line(&written, &release))
            .collect();

        assert!(writer.finish(SPEC_DRAIN_BOUND));
        let numbers: Vec<&Value> = held.iter().chain(&after).map(|line| &line["n"]).collect();
        assert_eq!(numbers, [0, 1, 2, 5, 6]);
        let [dropped, last] = after.as_slice() else {
            panic!("two lines after the release: {after:?}");
        };
        assert_eq!(dropped["event"], "dropped");
        assert_eq!(dropped["count"], 2);
        assert_eq!(last["event"], EVENT_NAME);
        assert!(held.iter().all(|line| line["event"] == EVENT_NAME));
    }

    #[test]
    fn observability_decision_12_finish_returns_once_every_queued_line_is_written() {
        let (entered, entering) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (written_sender, written) = mpsc::channel();
        let writer = TraceWriter::start(
            HeldBackOutput {
                entered,
                released,
                written: written_sender,
            },
            Arc::new(HeaderSource::new(
                RunId::from_random_bytes([0; RUN_ID_BYTES]),
                frozen_clocks(WALL_MS),
            )),
            SMALL_QUEUE_LINES,
        )
        .unwrap();
        for _queued in 0..=SMALL_QUEUE_LINES {
            writer.write(EVENT_NAME, &[]);
        }
        let releasing = thread::spawn(move || {
            for _line in 0..=SMALL_QUEUE_LINES {
                within_bound(&entering, "the writer holds a line");
                release.send(()).unwrap();
            }
        });

        let drained = writer.finish(SPEC_DRAIN_BOUND);

        assert!(drained);
        assert_eq!(written.try_iter().count(), SMALL_QUEUE_LINES + 1);
        releasing.join().unwrap();
    }
}
