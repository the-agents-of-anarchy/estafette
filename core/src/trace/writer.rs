use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;

use super::line::{HeaderSource, StampedLine};
use super::{CreateTraceFile, RenameFile, SpawnThread};
use crate::dirs::rolled_over_trace_file;
use crate::event::{EventName, TraceLineName};

const DROPPED_COUNT_FIELD: &str = "count";
const LINE_END: u8 = b'\n';
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
    file: Option<Box<dyn Write + Send>>,
    written_bytes: u64,
    ends_mid_line: bool,
    rollover_bytes: u64,
    opening: RolloverOpening,
    first_line: Option<Vec<u8>>,
    create_file: CreateTraceFile,
    rename_file: RenameFile,
}

impl RollingFile {
    /// The newly created, empty file at `path`, rolled over at `rollover_bytes` by `rename_file` into a file `create_file` starts that holds `opening`.
    pub(super) fn new(
        path: PathBuf,
        file: Box<dyn Write + Send>,
        rollover_bytes: u64,
        opening: RolloverOpening,
        create_file: CreateTraceFile,
        rename_file: RenameFile,
    ) -> Self {
        Self {
            path,
            file: Some(file),
            written_bytes: 0,
            ends_mid_line: false,
            rollover_bytes,
            opening,
            first_line: None,
            create_file,
            rename_file,
        }
    }

    /// Whether the line would take a file that already holds lines past the size bound.
    fn is_due_to_roll_over(&self, line: &[u8]) -> bool {
        self.file.is_some()
            && self.written_bytes > 0
            && self.written_bytes.saturating_add(byte_count(line)) > self.rollover_bytes
    }

    /// The file lines go to now, created in place of one a rollover renamed.
    fn current_file(&mut self) -> io::Result<Box<dyn Write + Send>> {
        if let Some(file) = self.file.take() {
            return Ok(file);
        }
        let created = (self.create_file)(&self.path)?;
        self.written_bytes = 0;
        self.ends_mid_line = false;
        Ok(created)
    }

    /// What goes before a line in a file that holds nothing yet: the opening, once the trace file held a first line.
    fn opening_of_an_empty_file(&self) -> &[u8] {
        match (&self.opening, &self.first_line) {
            (RolloverOpening::FirstLineAgain, Some(first_line)) if self.written_bytes == 0 => {
                first_line
            }
            (RolloverOpening::FirstLineAgain | RolloverOpening::Nothing, _) => &[],
        }
    }
}

impl TraceOutput for RollingFile {
    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        if self.is_due_to_roll_over(line) {
            (self.rename_file)(&self.path, &rolled_over_trace_file(&self.path))?;
            self.file = None;
        }
        let mut file = self.current_file()?;
        let line_end_after_a_torn_line: &[u8] = if self.ends_mid_line { &[LINE_END] } else { &[] };
        let whole = [
            line_end_after_a_torn_line,
            self.opening_of_an_empty_file(),
            line,
        ]
        .concat();
        let mut counted = CountedFile {
            file: &mut *file,
            accepted: 0,
        };
        let written = counted.write_all(&whole);
        if let Some(last_accepted) = whole.get(..counted.accepted).and_then(<[u8]>::last) {
            self.ends_mid_line = *last_accepted != LINE_END;
        }
        self.file = Some(file);
        written?;
        if self.first_line.is_none() {
            self.first_line = Some(line.to_vec());
        }
        self.written_bytes = self.written_bytes.saturating_add(byte_count(&whole));
        Ok(())
    }
}

/// A trace file that counts the bytes it accepts, so a write that fails part-way says where the file now ends.
struct CountedFile<'file> {
    file: &'file mut (dyn Write + Send),
    accepted: usize,
}

impl Write for CountedFile<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let accepted = self.file.write(bytes)?;
        self.accepted = self.accepted.saturating_add(accepted);
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// The bytes a line takes on disk.
fn byte_count(line: &[u8]) -> u64 {
    u64::try_from(line.len()).unwrap_or(u64::MAX)
}

/// One trace file's writer: a bounded queue to a thread of its own, so a caller never waits for the disk.
pub(super) struct TraceWriter {
    header: Arc<HeaderSource>,
    queue: Mutex<Queue>,
    lost_at_close: Arc<Mutex<u64>>,
    ended: Mutex<Option<Receiver<()>>>,
}

/// The queue while it is open, and the events lost since the last line queued.
struct Queue {
    lines: Option<SyncSender<QueuedLine>>,
    lost: u64,
}

/// A line its caller stamped, and the events lost since the line queued before it.
struct QueuedLine {
    line: StampedLine,
    lost_before: u64,
}

impl TraceWriter {
    /// Starts the writer thread, through `spawn_thread`, on the output behind a queue of at most `queue_lines` lines.
    pub(super) fn start(
        output: impl TraceOutput + Send + 'static,
        header: Arc<HeaderSource>,
        queue_lines: usize,
        spawn_thread: &SpawnThread,
    ) -> io::Result<Self> {
        let (queue, lines) = mpsc::sync_channel(queue_lines);
        let (ended_sender, ended) = mpsc::channel::<()>();
        let lost_at_close = Arc::new(Mutex::new(0));
        let writer_header = Arc::clone(&header);
        let writer_lost_at_close = Arc::clone(&lost_at_close);
        spawn_thread(
            WRITER_THREAD_NAME,
            Box::new(move || {
                write_every_line(output, &writer_header, &lines, &writer_lost_at_close);
                drop(ended_sender);
            }),
        )?;
        Ok(Self {
            header,
            queue: Mutex::new(Queue {
                lines: Some(queue),
                lost: 0,
            }),
            lost_at_close,
            ended: Mutex::new(Some(ended)),
        })
    }

    /// Queues the event's line, or counts it lost when the queue is full; the thread numbers each line as it writes it.
    pub(super) fn write(&self, name: &str, fields: &[(&str, Value)]) {
        let mut queue = locked(&self.queue);
        let queued = QueuedLine {
            line: self.header.stamp(name, fields),
            lost_before: queue.lost,
        };
        let is_queued = queue
            .lines
            .as_ref()
            .is_some_and(|lines| lines.try_send(queued).is_ok());
        queue.lost = if is_queued {
            0
        } else {
            queue.lost.saturating_add(1)
        };
    }

    /// Closes the queue, handing the thread the events lost since the last line queued, and waits at most `within` for it to write every line it holds and report that loss, saying whether it did.
    pub(super) fn finish(&self, within: Duration) -> bool {
        let mut queue = locked(&self.queue);
        *locked(&self.lost_at_close) = queue.lost;
        drop(queue.lines.take());
        drop(queue);
        locked(&self.ended).take().is_none_or(|ended| {
            matches!(
                ended.recv_timeout(within),
                Err(RecvTimeoutError::Disconnected)
            )
        })
    }
}

/// Writes each queued line under the next number until the queue closes, then reports the events lost at its close.
fn write_every_line(
    mut output: impl TraceOutput,
    header: &HeaderSource,
    lines: &Receiver<QueuedLine>,
    lost_at_close: &Mutex<u64>,
) {
    let mut numbering = Numbering::default();
    for queued in lines {
        numbering.count_unwritten(queued.lost_before);
        numbering.write(&mut output, header, &queued.line);
    }
    numbering.count_unwritten(*locked(lost_at_close));
    numbering.report_unwritten(&mut output, header);
}

/// The number of the next line, and how many numbered lines never reached the output since the last `dropped` line.
#[derive(Default)]
struct Numbering {
    next_n: u64,
    unwritten: u64,
}

impl Numbering {
    /// Counts `lost` lines as numbered and never written.
    const fn count_unwritten(&mut self, lost: u64) {
        self.next_n = self.next_n.saturating_add(lost);
        self.unwritten = self.unwritten.saturating_add(lost);
    }

    /// Writes the line under the next number once every earlier loss is reported, counting it unwritten when either write fails.
    fn write(&mut self, output: &mut impl TraceOutput, header: &HeaderSource, line: &StampedLine) {
        let is_written = self.report_unwritten(output, header)
            && output
                .write_line(line.numbered(self.next_n).as_bytes())
                .is_ok();
        if is_written {
            self.next_n = self.next_n.saturating_add(1);
        } else {
            self.count_unwritten(1);
        }
    }

    /// Writes a `dropped` line carrying the count of unwritten lines when there are any, saying whether none is left unreported.
    fn report_unwritten(&mut self, output: &mut impl TraceOutput, header: &HeaderSource) -> bool {
        if self.unwritten == 0 {
            return true;
        }
        let dropped = header.stamp(
            TraceLineName::Dropped.as_str(),
            &[(DROPPED_COUNT_FIELD, Value::from(self.unwritten))],
        );
        let is_written = output
            .write_line(dropped.numbered(self.next_n).as_bytes())
            .is_ok();
        if is_written {
            self.next_n = self.next_n.saturating_add(1);
            self.unwritten = 0;
        }
        is_written
    }
}

/// The value behind the lock, whether or not an earlier holder panicked.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
