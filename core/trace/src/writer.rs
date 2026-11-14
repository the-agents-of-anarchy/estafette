use std::cell::Cell;
use std::io::{self, Write};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::Duration;

use serde_json::Value;

use super::line::{HeaderSource, StampedLine};
use super::{
    CreateTraceFile, RenameFile, SpawnThread, TRACE_FILE_SIZED_AHEAD_BYTES, TraceFile, TraceStop,
};
use crate::directory::{TraceFileKind, rolled_over_trace_file};
use crate::files::PRODUCT_NAME;

/// The name of the line a trace file writes of its own carrying the count of events a full queue lost.
pub const DROPPED_LINE_NAME: &str = "dropped";
const DROPPED_COUNT_FIELD: &str = "count";
const LINE_END: u8 = b'\n';
const WRITER_THREAD_NAME_SUFFIX: &str = "trace-writer";

/// Where a trace writer's thread puts each line.
pub trait TraceOutput {
    /// Which of its run's trace files this output is.
    fn kind(&self) -> TraceFileKind;

    /// Writes the line, newline included, whole in one write.
    fn write_line(&mut self, line: &[u8]) -> io::Result<()>;

    /// Ends the output at the last byte written, nothing sized past it.
    fn close(&mut self) -> io::Result<()>;
}

/// A trace file, sized ahead of its lines, that rolls over into its `.1` file, replacing any earlier one, before a line would take it past its size bound.
pub struct RollingFile {
    path: PathBuf,
    file: Option<Box<dyn TraceFile>>,
    written_bytes: u64,
    sized_bytes: u64,
    ends_mid_line: bool,
    rollover_bytes: u64,
    kind: TraceFileKind,
    first_line: Option<Vec<u8>>,
    create_file: CreateTraceFile,
    rename_file: RenameFile,
}

impl RollingFile {
    /// The newly created, empty trace file of `kind` at `path`, rolled over at `rollover_bytes` by `rename_file` into a file `create_file` starts.
    pub(super) fn new(
        path: PathBuf,
        file: Box<dyn TraceFile>,
        rollover_bytes: u64,
        kind: TraceFileKind,
        create_file: CreateTraceFile,
        rename_file: RenameFile,
    ) -> Self {
        Self {
            path,
            file: Some(file),
            written_bytes: 0,
            sized_bytes: 0,
            ends_mid_line: false,
            rollover_bytes,
            kind,
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
    fn current_file(&mut self) -> io::Result<Box<dyn TraceFile>> {
        if let Some(file) = self.file.take() {
            return Ok(file);
        }
        let created = (self.create_file)(&self.path)?;
        self.written_bytes = 0;
        self.sized_bytes = 0;
        self.ends_mid_line = false;
        Ok(created)
    }

    /// Sizes the file ahead, in whole steps, so the next `bytes` land inside it.
    fn size_ahead(&mut self, file: &mut dyn TraceFile, bytes: u64) -> io::Result<()> {
        let needed = self.written_bytes.saturating_add(bytes);
        if needed <= self.sized_bytes {
            return Ok(());
        }
        let sized = needed
            .checked_next_multiple_of(TRACE_FILE_SIZED_AHEAD_BYTES)
            .unwrap_or(u64::MAX);
        file.set_length(sized)?;
        self.sized_bytes = sized;
        Ok(())
    }

    /// Writes the bytes after the last byte written, counting every byte the file takes, so a write that fails part-way leaves the file's end known.
    fn write_counted(&mut self, file: &mut dyn TraceFile, bytes: &[u8]) -> io::Result<()> {
        let mut counted = CountedFile { file, accepted: 0 };
        let written = counted.write_all(bytes);
        if let Some(last_accepted) = bytes.get(..counted.accepted).and_then(<[u8]>::last) {
            self.ends_mid_line = *last_accepted != LINE_END;
        }
        self.written_bytes = self.written_bytes.saturating_add(byte_count(
            bytes.get(..counted.accepted).unwrap_or_default(),
        ));
        written
    }

    /// Cuts the file back to the last byte written when it is sized past it, so nothing sized ahead remains.
    fn cut_to_written(&mut self) -> io::Result<()> {
        if self.sized_bytes > self.written_bytes
            && let Some(file) = self.file.as_mut()
        {
            file.set_length(self.written_bytes)?;
            self.sized_bytes = self.written_bytes;
        }
        Ok(())
    }

    /// What goes before a line in a file that holds nothing yet: for a raw file, the first line it held, once it held one; for an events file, nothing.
    fn opening_of_an_empty_file(&self) -> &[u8] {
        match (self.kind, &self.first_line) {
            (TraceFileKind::Raw, Some(first_line)) if self.written_bytes == 0 => first_line,
            (TraceFileKind::Raw | TraceFileKind::Events, _) => &[],
        }
    }
}

impl TraceOutput for RollingFile {
    fn kind(&self) -> TraceFileKind {
        self.kind
    }

    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        if self.is_due_to_roll_over(line) {
            self.cut_to_written()?;
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
        let written = self
            .size_ahead(file.as_mut(), byte_count(&whole))
            .and_then(|()| self.write_counted(file.as_mut(), &whole));
        self.file = Some(file);
        written?;
        if self.first_line.is_none() {
            self.first_line = Some(line.to_vec());
        }
        Ok(())
    }

    fn close(&mut self) -> io::Result<()> {
        self.cut_to_written()
    }
}

/// A trace file that counts the bytes it accepts, so a write that fails part-way says where the file now ends.
struct CountedFile<'file> {
    file: &'file mut dyn TraceFile,
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
pub struct FileTrace {
    kind: TraceFileKind,
    drain: Duration,
    header: Arc<HeaderSource>,
    queue: Mutex<Queue>,
    lost_at_close: Arc<Mutex<u64>>,
    ended: Mutex<Option<Receiver<io::Result<()>>>>,
    writer_moved: Arc<WriterMoved>,
}

thread_local! {
    /// The trace whose line this thread stamps under that trace's queue lock, if any.
    static TRACE_STAMPED_ON_THIS_THREAD: Cell<Option<NonNull<FileTrace>>> = const { Cell::new(None) };
}

/// This thread named as the one stamping a line of one trace under its queue's lock, until the guard drops, so a panic hook the stamp sets off on this thread never waits on that lock.
struct Stamping {
    stamped_before: Option<NonNull<FileTrace>>,
}

impl Stamping {
    /// Names this thread as the one stamping a line of `trace`.
    fn of(trace: &FileTrace) -> Self {
        Self {
            stamped_before: TRACE_STAMPED_ON_THIS_THREAD.replace(Some(NonNull::from(trace))),
        }
    }
}

impl Drop for Stamping {
    fn drop(&mut self) {
        TRACE_STAMPED_ON_THIS_THREAD.set(self.stamped_before);
    }
}

/// The writer's notice, after each line or marker it took off the queue, to every flush waiting for a slot or for its answer.
#[derive(Default)]
struct WriterMoved {
    notice: Mutex<()>,
    noticed: Condvar,
}

impl WriterMoved {
    /// Wakes every flush waiting on the writer.
    fn notify(&self) {
        drop(locked(&self.notice));
        self.noticed.notify_all();
    }
}

/// Where one flush stands with the writer: its marker waiting for a slot, its marker queued and waiting for the writer's answer, that answer, or the writer gone without one.
enum FlushProgress {
    Unqueued {
        marker: Queued,
        lines: SyncSender<Queued>,
        answered: Receiver<io::Result<()>>,
    },
    Queued(Receiver<io::Result<()>>),
    Answered(io::Result<()>),
    WriterGone,
}

impl FlushProgress {
    /// Takes the flush one step on: its marker into a free slot, or the writer's answer once given.
    fn advance(&mut self) {
        *self = match std::mem::replace(self, Self::WriterGone) {
            Self::Unqueued {
                marker,
                lines,
                answered,
            } => match lines.try_send(marker) {
                Ok(()) => Self::Queued(answered),
                Err(TrySendError::Full(marker)) => Self::Unqueued {
                    marker,
                    lines,
                    answered,
                },
                Err(TrySendError::Disconnected(_marker)) => Self::WriterGone,
            },
            Self::Queued(answered) => match answered.try_recv() {
                Ok(answer) => Self::Answered(answer),
                Err(TryRecvError::Empty) => Self::Queued(answered),
                Err(TryRecvError::Disconnected) => Self::WriterGone,
            },
            settled @ (Self::Answered(_) | Self::WriterGone) => settled,
        };
    }

    /// Whether the flush still waits on the writer.
    const fn is_waiting(&self) -> bool {
        match self {
            Self::Unqueued { .. } | Self::Queued(_) => true,
            Self::Answered(_) | Self::WriterGone => false,
        }
    }

    /// The writer's answer, when it gave one.
    fn answer(self) -> Option<io::Result<()>> {
        match self {
            Self::Answered(answer) => Some(answer),
            Self::Unqueued { .. } | Self::Queued(_) | Self::WriterGone => None,
        }
    }
}

/// The queue while it is open, and the events lost since the last line queued.
struct Queue {
    lines: Option<SyncSender<Queued>>,
    lost: u64,
}

/// What the writer thread takes from the queue: a line to write, or a caller waiting until every line queued before it is written and the file cut back to its last line.
enum Queued {
    Line(QueuedLine),
    Flush(mpsc::Sender<io::Result<()>>),
}

/// A line its caller stamped, and the events lost since the line queued before it.
struct QueuedLine {
    line: StampedLine,
    lost_before: u64,
}

impl FileTrace {
    /// Starts the writer thread of the output's file, through `spawn_thread`, on the output behind a queue of at most `queue_lines` lines, to be stopped within `drain`.
    pub(super) fn start(
        output: impl TraceOutput + Send + 'static,
        drain: Duration,
        header: Arc<HeaderSource>,
        queue_lines: usize,
        spawn_thread: &SpawnThread,
    ) -> io::Result<Self> {
        let kind = output.kind();
        let (queue, lines) = mpsc::sync_channel(queue_lines);
        let (ended_sender, ended) = mpsc::channel::<io::Result<()>>();
        let lost_at_close = Arc::new(Mutex::new(0));
        let writer_header = Arc::clone(&header);
        let writer_lost_at_close = Arc::clone(&lost_at_close);
        let writer_moved = Arc::new(WriterMoved::default());
        let moved = Arc::clone(&writer_moved);
        spawn_thread(
            &format!("{PRODUCT_NAME}-{WRITER_THREAD_NAME_SUFFIX}"),
            Box::new(move || {
                let closed = write_every_line(
                    output,
                    &writer_header,
                    &lines,
                    &writer_lost_at_close,
                    &moved,
                );
                drop(ended_sender.send(closed));
            }),
        )?;
        Ok(Self {
            kind,
            drain,
            header,
            queue: Mutex::new(Queue {
                lines: Some(queue),
                lost: 0,
            }),
            lost_at_close,
            ended: Mutex::new(Some(ended)),
            writer_moved,
        })
    }

    /// Queues the event's line, or counts it lost when the queue is full, or drops it when this thread is stamping a line already, as a panic hook does on the thread whose stamp panicked; the thread numbers each line as it writes it.
    pub fn write(&self, name: &str, fields: &[(&str, Value)]) {
        let Some(mut queue) = self.queue_unless_this_thread_stamps() else {
            return;
        };
        let line = {
            let _stamping = Stamping::of(self);
            self.header.stamp(name, fields)
        };
        let queued = QueuedLine {
            line,
            lost_before: queue.lost,
        };
        let is_queued = queue
            .lines
            .as_ref()
            .is_some_and(|lines| lines.try_send(Queued::Line(queued)).is_ok());
        queue.lost = if is_queued {
            0
        } else {
            queue.lost.saturating_add(1)
        };
    }

    /// Waits at most the drain bound, for a slot while the queue is full and then for the thread, until the thread wrote every line queued before this call and cut the file back to its last line, leaving the queue open, saying how that left this file; a queue the stop already closed is drained, and a flush on a thread stamping a line, as a panic hook on the thread whose stamp panicked, is not.
    pub fn flush(&self) -> TraceStop {
        let lines = match self.queue_unless_this_thread_stamps() {
            Some(queue) => queue.lines.clone(),
            None => return TraceStop::NotDrained { file: self.kind },
        };
        let Some(lines) = lines else {
            return TraceStop::Drained;
        };
        let (written, answered) = mpsc::channel();
        let mut progress = FlushProgress::Unqueued {
            marker: Queued::Flush(written),
            lines,
            answered,
        };
        let (_notice, _timed_out_is_read_from_the_progress) = self
            .writer_moved
            .noticed
            .wait_timeout_while(locked(&self.writer_moved.notice), self.drain, |()| {
                progress.advance();
                progress.is_waiting()
            })
            .unwrap_or_else(PoisonError::into_inner);
        self.stop_answered(progress.answer())
    }

    /// Closes the queue, handing the thread the events lost since the last line queued, and waits at most the drain bound for it to take every line it holds, report that loss and cut the file back to its last line, saying how the stop left this file.
    pub fn finish(&self) -> TraceStop {
        let mut queue = locked(&self.queue);
        *locked(&self.lost_at_close) = queue.lost;
        drop(queue.lines.take());
        drop(queue);
        let Some(ended) = locked(&self.ended).take() else {
            return TraceStop::Drained;
        };
        self.stop_answered(ended.recv_timeout(self.drain).ok())
    }

    /// The queue under its lock once any other holder lets go of it, or none when this thread holds that lock while it stamps a line, as a panic hook does on the thread whose stamp panicked.
    fn queue_unless_this_thread_stamps(&self) -> Option<MutexGuard<'_, Queue>> {
        match self.queue.try_lock() {
            Ok(queue) => Some(queue),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => {
                let is_stamped_here =
                    TRACE_STAMPED_ON_THIS_THREAD.get() == Some(NonNull::from(self));
                (!is_stamped_here).then(|| locked(&self.queue))
            }
        }
    }

    /// How the writer's answer left this file: drained when it ended at its last line, not cut when it could not, and not drained when no answer came.
    fn stop_answered(&self, answer: Option<io::Result<()>>) -> TraceStop {
        match answer {
            Some(Ok(())) => TraceStop::Drained,
            Some(Err(source)) => TraceStop::NotCut {
                file: self.kind,
                source,
            },
            None => TraceStop::NotDrained { file: self.kind },
        }
    }
}

/// Writes each queued line under the next number, cutting the file back to its last line for each caller waiting on a flush and telling every waiting flush after each, until the queue closes, then reports the events lost at its close and ends the output at its last line, saying whether it could.
fn write_every_line(
    mut output: impl TraceOutput,
    header: &HeaderSource,
    lines: &Receiver<Queued>,
    lost_at_close: &Mutex<u64>,
    moved: &WriterMoved,
) -> io::Result<()> {
    let mut numbering = Numbering::default();
    for queued in lines {
        match queued {
            Queued::Line(queued) => {
                numbering.count_unwritten(queued.lost_before);
                numbering.write(&mut output, header, &queued.line);
            }
            Queued::Flush(written) => drop(written.send(output.close())),
        }
        moved.notify();
    }
    numbering.count_unwritten(*locked(lost_at_close));
    numbering.report_unwritten(&mut output, header);
    output.close()
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
            DROPPED_LINE_NAME,
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
