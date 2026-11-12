use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use data_encoding::HEXLOWER;
use iroh::EndpointId;

use serde_json::Value;

use crate::dirs::{DirectoryFilesystem, TraceDirectory, TraceFiles};
use crate::event::{Event, EventName, TraceLineName};

mod files;
mod line;
mod operating_system;
mod writer;

pub use files::run_name_pattern;
use files::{remove_oldest_runs, run_name};
use line::HeaderSource;
use writer::{RollingFile, RolloverOpening, TraceWriter};

/// The version of the trace line format, raised when an event name is renamed or removed or a field changes meaning.
pub const FORMAT_VERSION: u64 = 1;
/// The size at which a trace file rolls over into its `.1` file: 64 MiB.
pub const TRACE_FILE_ROLLOVER_BYTES: u64 = 67_108_864;
/// The files one run keeps of one trace file: the file and the one `.1` file it rolls over into.
const FILES_A_RUN_KEEPS_PER_TRACE_FILE: u64 = 2;
/// The most one run keeps of one trace file, the file and its `.1` file together: 128 MiB.
pub const TRACE_RUN_FILE_MAX_BYTES: u64 =
    FILES_A_RUN_KEEPS_PER_TRACE_FILE * TRACE_FILE_ROLLOVER_BYTES;
/// The most the trace directory holds once the oldest runs are removed at a start: 1 GiB.
pub const TRACE_DIRECTORY_MAX_BYTES: u64 = 1_073_741_824;
/// How recently a run's events file was written for the run to be kept whatever the directory holds: ten minutes.
pub const TRACE_FRESHNESS: Duration = Duration::from_secs(600);
/// The most lines a trace file's queue holds before an event is counted lost.
pub const TRACE_QUEUE_LINES: usize = 4096;
/// The longest the stopping sequence waits for each trace file's queue to empty before the process exits.
pub const TRACE_DRAIN_LIMIT: Duration = Duration::from_millis(1000);
/// The bytes of a run id drawn from the secure random source, written as sixteen hex characters.
pub const RUN_ID_BYTES: usize = 8;

const SAFE_TO_POST_FIELD: &str = "safe_to_post";

/// A clock reading whole milliseconds.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The bounds a trace runs under: `TraceBounds::SPEC` in production, smaller ones in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceBounds {
    /// The size at which a trace file rolls over into its `.1` file.
    pub rollover_bytes: u64,
    /// The most the trace directory holds once the oldest runs are removed at a start.
    pub directory_bytes: u64,
    /// How recently a run's events file was written for the run to be kept.
    pub freshness: Duration,
    /// The most lines a trace file's queue holds.
    pub queue_lines: usize,
    /// The longest the stopping sequence waits for each queue to empty.
    pub drain: Duration,
}

impl TraceBounds {
    /// The bounds of `spec/observability.md` decisions 12 and 13.
    pub const SPEC: Self = Self {
        rollover_bytes: TRACE_FILE_ROLLOVER_BYTES,
        directory_bytes: TRACE_DIRECTORY_MAX_BYTES,
        freshness: TRACE_FRESHNESS,
        queue_lines: TRACE_QUEUE_LINES,
        drain: TRACE_DRAIN_LIMIT,
    };
}

/// The two clocks every trace line reads, handed in by the composition root.
#[derive(Clone)]
pub struct TraceClocks {
    /// Milliseconds on a clock that never goes back, from the process start.
    pub monotonic_ms: Clock,
    /// Milliseconds since the Unix epoch by this machine's clock.
    pub wall_ms: Clock,
}

/// How far ahead of its lines a trace file is sized, in whole steps, so each line is written into zeros the file already holds and never extends it.
const TRACE_FILE_SIZED_AHEAD_BYTES: u64 = 65_536;

/// A trace file its writer sizes ahead of the lines it writes, the bytes past them reading as zeros, and cuts back to its lines at a clean stop.
pub trait TraceFile: Write + Send {
    /// Sets the file's length, the part past what was written reading as zeros.
    ///
    /// # Errors
    ///
    /// Fails when the file's length cannot be set.
    fn set_length(&mut self, length: u64) -> io::Result<()>;
}

/// Creates a trace file new, readable and writable by its owner alone, and returns it for its writer to size and write from its start.
pub type CreateTraceFile = Arc<dyn Fn(&Path) -> io::Result<Box<dyn TraceFile>> + Send + Sync>;
/// Renames a file onto another path, replacing any file there.
pub type RenameFile = Arc<dyn Fn(&Path, &Path) -> io::Result<()> + Send + Sync>;
/// Every file in a directory with its size and when it was last written.
pub type ListFiles = Arc<dyn Fn(&Path) -> io::Result<Vec<StoredFile>> + Send + Sync>;
/// Removes a file.
pub type RemoveFile = Arc<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;
/// Runs the work on a new thread of the name given.
pub type SpawnThread = Arc<dyn Fn(&str, Box<dyn FnOnce() + Send>) -> io::Result<()> + Send + Sync>;

/// One file in the trace directory: its name, its bytes, and when it was last written, in milliseconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    /// The file's name in its directory.
    pub name: OsString,
    /// The file's size in bytes.
    pub bytes: u64,
    /// When the file was last written, in milliseconds since the Unix epoch.
    pub written_ms: u64,
}

/// Every side effect a trace performs, built at the composition root and handed in.
pub struct TraceEffects {
    /// The two clocks every line reads.
    pub clocks: TraceClocks,
    /// Creates each trace file, at the start and at each rollover.
    pub create_file: CreateTraceFile,
    /// Renames a trace file to its `.1` file at a rollover.
    pub rename_file: RenameFile,
    /// Lists the trace directory at the start, to remove the oldest runs.
    pub list_files: ListFiles,
    /// Removes a file of an old run at the start.
    pub remove_file: RemoveFile,
    /// Starts the writer thread of each trace file.
    pub spawn_thread: SpawnThread,
}

/// The id of one process start, drawn from the secure random source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunId([u8; RUN_ID_BYTES]);

impl RunId {
    /// The run id of these random bytes.
    #[must_use]
    pub const fn from_random_bytes(bytes: [u8; RUN_ID_BYTES]) -> Self {
        Self(bytes)
    }

    /// The run id as sixteen lowercase hex characters.
    fn hex(self) -> String {
        HEXLOWER.encode(&self.0)
    }
}

/// The debug trace of one process: its events file and its raw file, each written by a thread of its own.
pub struct Trace {
    files: TraceFiles,
    header: Arc<HeaderSource>,
    events: TraceWriter,
    raw: TraceWriter,
    drain: Duration,
}

/// How a trace's stop left its files.
#[derive(Debug)]
pub enum TraceStop {
    /// Every queued line is on disk and each file ends at its last line.
    Drained,
    /// The drain bound ran out before every queued line was on disk.
    NotDrained,
    /// Every queued line is on disk, but a file keeps the zero bytes sized past its last line.
    NotCut(io::Error),
}

/// A trace that could not start, refusing the command before any other side effect.
#[derive(Debug)]
pub struct TraceRefused {
    action: TraceAction,
    path: PathBuf,
    source: io::Error,
}

/// What the trace could not do to the path it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceAction {
    CreateDirectory,
    ListDirectory,
    RemoveOldFile,
    CreateFile,
    StartWriter,
}

impl TraceRefused {
    /// The refusal of `action` on `path` for `source`.
    fn new(action: TraceAction, path: &Path, source: io::Error) -> Self {
        Self {
            action,
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for TraceAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CreateDirectory => "create the trace directory",
            Self::ListDirectory => "list the trace directory",
            Self::RemoveOldFile => "remove the old trace file",
            Self::CreateFile => "create the trace file",
            Self::StartWriter => "start the writer of the trace file",
        })
    }
}

impl fmt::Display for TraceRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot {} {}: {}; run with --no-debug to write no trace",
            self.action,
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for TraceRefused {}

impl Trace {
    /// Removes the oldest runs past the bounds, creates this run's two files in the trace directory, starts a writer on each, and writes the raw file's first line.
    ///
    /// # Errors
    /// `TraceRefused` naming the path that could not be created, listed, removed or written.
    pub fn start(
        directory: &TraceDirectory,
        filesystem: &impl DirectoryFilesystem,
        run: RunId,
        effects: &TraceEffects,
        bounds: TraceBounds,
    ) -> Result<Self, TraceRefused> {
        for path in directory.private_directories() {
            filesystem
                .create_private_directory(&path)
                .map_err(|source| TraceRefused::new(TraceAction::CreateDirectory, &path, source))?;
        }
        let traces = directory.path();
        let header = Arc::new(HeaderSource::new(run, effects.clocks.clone()));
        let started_ms = header.wall_ms();
        remove_oldest_runs(
            &traces,
            started_ms,
            &bounds,
            &effects.list_files,
            &effects.remove_file,
        )?;
        let files = directory.files_of(&run_name(started_ms, &run.hex()));
        let events = start_writer(
            &files.events,
            &header,
            &bounds,
            RolloverOpening::Nothing,
            effects,
        )?;
        let raw = start_writer(
            &files.raw,
            &header,
            &bounds,
            RolloverOpening::FirstLineAgain,
            effects,
        )?;
        raw.write(
            TraceLineName::RawFileOpened.as_str(),
            &[(SAFE_TO_POST_FIELD, Value::Bool(false))],
        );
        Ok(Self {
            files,
            header,
            events,
            raw,
            drain: bounds.drain,
        })
    }

    /// The two files this trace writes.
    #[must_use]
    pub const fn files(&self) -> &TraceFiles {
        &self.files
    }

    /// The line standard error opens with: the two files and which of them is safe to post.
    #[must_use]
    pub fn announcement(&self) -> String {
        format!(
            "writing the debug trace: {} (safe to post) and {} (not safe to post)",
            self.files.events.display(),
            self.files.raw.display()
        )
    }

    /// Queues the event's line on the events file, never waiting for the writer.
    pub fn record(&self, event: &Event) {
        self.events.write(event.name(), &event.fields());
    }

    /// Names the endpoint this process now runs on in every later line.
    pub fn note_endpoint(&self, id: EndpointId) {
        self.header.note_node(id);
    }

    /// Closes both queues and waits, within the drain bound for each, until their lines are on disk and each file ends at its last line, saying how the stop left them.
    pub fn finish(&self) -> TraceStop {
        match (self.events.finish(self.drain), self.raw.finish(self.drain)) {
            (TraceStop::Drained, TraceStop::Drained) => TraceStop::Drained,
            (TraceStop::NotDrained, _) | (_, TraceStop::NotDrained) => TraceStop::NotDrained,
            (TraceStop::NotCut(source), _) | (_, TraceStop::NotCut(source)) => {
                TraceStop::NotCut(source)
            }
        }
    }
}

/// Creates the trace file and starts its writer, each rollover starting a file that holds `opening`.
fn start_writer(
    path: &Path,
    header: &Arc<HeaderSource>,
    bounds: &TraceBounds,
    opening: RolloverOpening,
    effects: &TraceEffects,
) -> Result<TraceWriter, TraceRefused> {
    let file = (effects.create_file)(path)
        .map_err(|source| TraceRefused::new(TraceAction::CreateFile, path, source))?;
    TraceWriter::start(
        RollingFile::new(
            path.to_path_buf(),
            file,
            bounds.rollover_bytes,
            opening,
            Arc::clone(&effects.create_file),
            Arc::clone(&effects.rename_file),
        ),
        Arc::clone(header),
        bounds.queue_lines,
        &effects.spawn_thread,
    )
    .map_err(|source| TraceRefused::new(TraceAction::StartWriter, path, source))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fs::{self, File};
    use std::path::Path;
    use std::sync::Mutex;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::{Duration, UNIX_EPOCH};

    use iroh::SecretKey;

    use super::files::run_name;
    use super::*;
    use crate::dirs::OperatingSystemFilesystem;
    use crate::event::{ClaimFailure, InboxEvent, ProcessEvent};
    use crate::node::NodeError;
    use crate::seeded_draws::Draws;

    const SPEC_RAW_FIRST_LINE_AFTER_HEADER: &str =
        ",\"event\":\"raw_file_opened\",\"safe_to_post\":false}\n";
    const LEAP_DAY_LAST_SECOND_MS: u64 = 951_868_799_123;
    const FROZEN_MONOTONIC_MS: u64 = 7;
    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
    const RUN_HEX: &str = "a1b2c3d4e5f60718";
    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const STAMPED_STARTS: [(u64, &str); 5] = [
        (0, "19700101T000000Z"),
        (951_868_800_123, "20000301T000000Z"),
        (1_735_689_599_999, "20241231T235959Z"),
        (4_107_542_399_000, "21000228T235959Z"),
        (4_107_542_400_000, "21000301T000000Z"),
    ];
    const ENDPOINT_SEED: u8 = 3;
    const SPEC_NODE_HEX_CHARS: usize = 10;
    const LINES_WRITTEN: u64 = 5;
    const LINES_PER_FILE: u64 = 2;
    const NOW_MS: u64 = 2_000_000_000_000;
    const SMALL_FRESHNESS_MS: u64 = 10_000;
    const SMALL_DIRECTORY_BYTES: u64 = 1000;
    const RAW_EVENT: &str = "iroh_line";
    const CLOCK_EVENT: &str = "process.clock_before_unix_epoch";
    const SMALL_QUEUE_LINES: usize = 2;
    const WRITER_STEP_BOUND: Duration = Duration::from_secs(10);
    const TEST_DRAIN_BOUND: Duration = Duration::from_secs(10);
    const DROPPED_EVENT: &str = "dropped";
    const EVENTS_FILE_SUFFIX: &str = ".events.jsonl";
    const STAND_IN_FAILURE: &str = "a stand-in failure";
    const STALE_EVENTS_FILE: &str = "20000101T000000Z-0000000000000001-estafette.events.jsonl";

    /// A fresh scratch root for the test and the trace directory under its data home.
    fn scratch_trace_directory(test_name: &str) -> (PathBuf, TraceDirectory) {
        let root = std::env::temp_dir().join(format!(
            "estafette-trace-{test_name}-{}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        let data_home = root.join("data");
        let directory = TraceDirectory::resolve(&move |name: &str| {
            (name == DATA_HOME_VARIABLE).then(|| OsString::from(&data_home))
        })
        .unwrap();
        (root, directory)
    }

    /// Clocks that always read `FROZEN_MONOTONIC_MS` and `wall_ms`.
    fn frozen_clocks(wall_ms: u64) -> TraceClocks {
        TraceClocks {
            monotonic_ms: Arc::new(|| FROZEN_MONOTONIC_MS),
            wall_ms: Arc::new(move || wall_ms),
        }
    }

    /// The names of the files in the directory, sorted.
    fn names_in(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn observability_decision_4_a_start_creates_two_files_named_for_its_utc_start_and_run_and_the_raw_file_says_it_is_not_safe_to_post()
     {
        let (root, directory) = scratch_trace_directory("start");

        let trace = Trace::start(
            &directory,
            &OperatingSystemFilesystem,
            RunId::from_random_bytes(RUN_BYTES),
            &TraceEffects::of_the_operating_system(frozen_clocks(LEAP_DAY_LAST_SECOND_MS)),
            TraceBounds::SPEC,
        )
        .unwrap();
        trace.finish();

        let run_name = format!("20000229T235959Z-{RUN_HEX}-estafette");
        let traces = directory.path();
        assert_eq!(
            names_in(&traces),
            [
                format!("{run_name}.events.jsonl"),
                format!("{run_name}.raw.jsonl")
            ]
        );
        assert_eq!(trace.files(), &directory.files_of(&run_name));
        assert_eq!(
            trace.announcement(),
            format!(
                "writing the debug trace: {} (safe to post) and {} (not safe to post)",
                traces.join(format!("{run_name}.events.jsonl")).display(),
                traces.join(format!("{run_name}.raw.jsonl")).display()
            )
        );
        assert_eq!(
            fs::read_to_string(traces.join(format!("{run_name}.raw.jsonl"))).unwrap(),
            format!(
                "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":0,\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{LEAP_DAY_LAST_SECOND_MS}{SPEC_RAW_FIRST_LINE_AFTER_HEADER}"
            )
        );
        assert_eq!(
            fs::read_to_string(traces.join(format!("{run_name}.events.jsonl"))).unwrap(),
            ""
        );
        drop(fs::remove_dir_all(&root));
    }

    /// A trace started on the operating system's files with its wall clock frozen at `wall_ms`.
    fn started(directory: &TraceDirectory, wall_ms: u64, bounds: TraceBounds) -> Trace {
        Trace::start(
            directory,
            &OperatingSystemFilesystem,
            RunId::from_random_bytes(RUN_BYTES),
            &TraceEffects::of_the_operating_system(frozen_clocks(wall_ms)),
            bounds,
        )
        .unwrap()
    }

    /// The events line numbered `n` of a clock event, naming `node` when there is one.
    fn clock_line(n: u64, node: Option<&str>) -> String {
        let node = node.map_or_else(String::new, |node| format!(",\"node\":\"{node}\""));
        format!(
            "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":{n},\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{NOW_MS}{node},\"event\":\"process.clock_before_unix_epoch\"}}\n"
        )
    }

    #[test]
    fn observability_decision_4_the_start_in_a_name_is_its_utc_second_across_month_year_and_leap_edges()
     {
        let (root, directory) = scratch_trace_directory("stamps");

        for (wall_ms, _stamp) in STAMPED_STARTS {
            assert!(matches!(
                started(&directory, wall_ms, TraceBounds::SPEC).finish(),
                TraceStop::Drained
            ));
        }

        let names = names_in(&directory.path());
        for (wall_ms, stamp) in STAMPED_STARTS {
            let events = format!("{stamp}-{RUN_HEX}-estafette.events.jsonl");
            assert!(names.contains(&events), "{wall_ms}: {events} in {names:?}");
        }
        assert_eq!(names.len(), STAMPED_STARTS.len() * 2, "{names:?}");
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_5_an_events_line_carries_the_header_then_names_the_node_once_the_endpoint_is_noted()
     {
        let (root, directory) = scratch_trace_directory("events");
        let trace = started(&directory, NOW_MS, TraceBounds::SPEC);
        let id = SecretKey::from_bytes(&[ENDPOINT_SEED; 32]).public();
        let node = data_encoding::HEXLOWER.encode(id.as_bytes());
        let node = node.get(..SPEC_NODE_HEX_CHARS).unwrap();

        trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        trace.note_endpoint(id);
        trace.record(&ClaimFailure::NetworkRefused(NodeError::TopicClosedBeforeHandle).into());
        trace.record(&InboxEvent::AcceptFailed(io::Error::other("a marker")).into());
        assert!(matches!(trace.finish(), TraceStop::Drained));

        let header = |n: u64| {
            format!(
                "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":{n},\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{NOW_MS}"
            )
        };
        assert_eq!(
            fs::read_to_string(&trace.files().events).unwrap(),
            format!(
                "{}\
                 {},\"node\":\"{node}\",\"event\":\"process.claim_network_refused\",\"code\":\"unreachable\"}}\n\
                 {},\"node\":\"{node}\",\"event\":\"inbox.accept_failed\"}}\n",
                clock_line(0, None),
                header(1),
                header(2)
            )
        );
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_13_a_file_rolls_over_once_at_its_size_bound_and_each_rollover_replaces_the_earlier_one()
     {
        let (root, directory) = scratch_trace_directory("rollover");
        let line_bytes = u64::try_from(clock_line(0, None).len()).unwrap();
        let trace = started(
            &directory,
            NOW_MS,
            TraceBounds {
                rollover_bytes: line_bytes * LINES_PER_FILE,
                ..TraceBounds::SPEC
            },
        );

        for _line in 0..LINES_WRITTEN {
            trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        }
        assert!(matches!(trace.finish(), TraceStop::Drained));

        let events = &trace.files().events;
        assert_eq!(fs::read_to_string(events).unwrap(), clock_line(4, None));
        assert_eq!(
            fs::read_to_string(crate::dirs::rolled_over_trace_file(events)).unwrap(),
            format!("{}{}", clock_line(2, None), clock_line(3, None))
        );
        assert_eq!(names_in(&directory.path()).len(), 3);
        drop(fs::remove_dir_all(&root));
    }

    /// The raw line numbered `n` of the test raw event.
    fn raw_line(n: u64) -> String {
        format!(
            "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":{n},\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{NOW_MS},\"event\":\"{RAW_EVENT}\"}}\n"
        )
    }

    /// The numbers of the raw lines each file holds after its first line, in the order the files open, when a line that would take a file past `rollover_bytes` opens the next file.
    fn raw_numbers_by_file(
        lines_written: u64,
        first_line_bytes: u64,
        rollover_bytes: u64,
    ) -> Vec<Vec<u64>> {
        let mut files = vec![Vec::new()];
        let mut file_bytes = first_line_bytes;
        for n in 1..=lines_written {
            let line_bytes = u64::try_from(raw_line(n).len()).unwrap();
            if file_bytes.saturating_add(line_bytes) > rollover_bytes {
                files.push(Vec::new());
                file_bytes = first_line_bytes;
            }
            file_bytes = file_bytes.saturating_add(line_bytes);
            files.last_mut().unwrap().push(n);
        }
        files
    }

    #[test]
    fn observability_decision_7_every_raw_file_a_rollover_opens_begins_with_the_line_saying_it_is_not_safe_to_post()
     {
        const SEEDED_CASES: u64 = 24;
        const MOST_LINES_PER_FILE: u64 = 4;
        const MOST_LINES_WRITTEN: u64 = 12;
        let not_safe_to_post = format!(
            "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":0,\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{NOW_MS}{SPEC_RAW_FIRST_LINE_AFTER_HEADER}"
        );
        let line_bytes = u64::try_from(raw_line(1).len()).unwrap();
        let first_line_bytes = u64::try_from(not_safe_to_post.len()).unwrap();
        for case in 0..SEEDED_CASES {
            let mut draws = Draws::seeded(case);
            let lines_per_file = draws.below(MOST_LINES_PER_FILE).saturating_add(1);
            let slack_bytes = draws.below(line_bytes);
            let lines_written = draws.below(MOST_LINES_WRITTEN).saturating_add(1);
            let rollover_bytes = first_line_bytes
                .saturating_add(line_bytes.saturating_mul(lines_per_file))
                .saturating_add(slack_bytes);
            let (root, directory) = scratch_trace_directory(&format!("raw-rollover-{case}"));
            let trace = started(
                &directory,
                NOW_MS,
                TraceBounds {
                    rollover_bytes,
                    ..TraceBounds::SPEC
                },
            );

            for _line in 0..lines_written {
                trace.raw.write(RAW_EVENT, &[]);
            }
            assert!(matches!(trace.finish(), TraceStop::Drained), "case {case}");

            let raw = &trace.files().raw;
            let rolled_over = crate::dirs::rolled_over_trace_file(raw);
            let numbers_by_file =
                raw_numbers_by_file(lines_written, first_line_bytes, rollover_bytes);
            let (current, earlier) = numbers_by_file.split_last().unwrap();
            assert_eq!(
                rolled_over.exists(),
                !earlier.is_empty(),
                "case {case}: whether a rollover opened a raw file for {lines_written} lines at {lines_per_file} a file"
            );
            let raw_file_holding = |numbers: &[u64]| {
                let lines: String = numbers.iter().map(|n| raw_line(*n)).collect();
                format!("{not_safe_to_post}{lines}")
            };
            assert_eq!(
                fs::read_to_string(raw).unwrap(),
                raw_file_holding(current),
                "case {case}: the raw file lines go to begins with the not-safe line and holds the lines since the last rollover"
            );
            if let Some(before_current) = earlier.last() {
                assert_eq!(
                    fs::read_to_string(&rolled_over).unwrap(),
                    raw_file_holding(before_current),
                    "case {case}: the raw file the last rollover renamed begins with the not-safe line and holds the lines before it"
                );
            }
            drop(fs::remove_dir_all(&root));
        }
    }

    /// The failure a stand-in file operation reports.
    fn stand_in_failure() -> io::Error {
        io::Error::other(STAND_IN_FAILURE)
    }

    /// Why the trace refused to start, failing the test when it started.
    fn refusal_of(
        directory: &TraceDirectory,
        effects: &TraceEffects,
        bounds: TraceBounds,
    ) -> TraceRefused {
        match Trace::start(
            directory,
            &OperatingSystemFilesystem,
            RunId::from_random_bytes(RUN_BYTES),
            effects,
            bounds,
        ) {
            Ok(_trace) => panic!("the trace started"),
            Err(refusal) => refusal,
        }
    }

    #[test]
    fn observability_decision_3_a_trace_file_that_cannot_be_created_refuses_the_start_naming_its_path_and_no_debug()
     {
        let (root, directory) = scratch_trace_directory("refused-file");
        let events = directory.files_of(&run_name(NOW_MS, RUN_HEX)).events;

        let refusal = refusal_of(
            &directory,
            &TraceEffects {
                create_file: Arc::new(|_path: &Path| Err(stand_in_failure())),
                ..TraceEffects::of_the_operating_system(frozen_clocks(NOW_MS))
            },
            TraceBounds::SPEC,
        );

        assert_eq!(refusal.action, TraceAction::CreateFile);
        assert_eq!(refusal.path, events);
        assert_eq!(
            refusal.to_string(),
            format!(
                "cannot create the trace file {}: {STAND_IN_FAILURE}; run with --no-debug to write no trace",
                events.display()
            )
        );
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_3_a_writer_thread_that_cannot_start_refuses_the_start_naming_its_file()
     {
        let (root, directory) = scratch_trace_directory("refused-thread");
        let events = directory.files_of(&run_name(NOW_MS, RUN_HEX)).events;

        let refusal = refusal_of(
            &directory,
            &TraceEffects {
                spawn_thread: Arc::new(|_name: &str, _work| Err(stand_in_failure())),
                ..TraceEffects::of_the_operating_system(frozen_clocks(NOW_MS))
            },
            TraceBounds::SPEC,
        );

        assert_eq!(refusal.action, TraceAction::StartWriter);
        assert_eq!(refusal.path, events);
        assert_eq!(
            refusal.to_string(),
            format!(
                "cannot start the writer of the trace file {}: {STAND_IN_FAILURE}; run with --no-debug to write no trace",
                events.display()
            )
        );
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_13_a_trace_directory_that_cannot_be_listed_refuses_the_start() {
        let (root, directory) = scratch_trace_directory("refused-list");
        let traces = directory.path();

        let refusal = refusal_of(
            &directory,
            &TraceEffects {
                list_files: Arc::new(|_directory: &Path| Err(stand_in_failure())),
                ..TraceEffects::of_the_operating_system(frozen_clocks(NOW_MS))
            },
            TraceBounds::SPEC,
        );

        assert_eq!(refusal.action, TraceAction::ListDirectory);
        assert_eq!(refusal.path, traces);
        assert_eq!(names_in(&traces), Vec::<String>::new());
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_13_an_old_run_that_cannot_be_removed_refuses_the_start_naming_its_file()
     {
        let (root, directory) = scratch_trace_directory("refused-remove");
        let traces = directory.path();

        let refusal = refusal_of(
            &directory,
            &TraceEffects {
                list_files: Arc::new(|_directory: &Path| {
                    Ok(vec![StoredFile {
                        name: OsString::from(STALE_EVENTS_FILE),
                        bytes: SMALL_DIRECTORY_BYTES + 1,
                        written_ms: NOW_MS - SMALL_FRESHNESS_MS - 1,
                    }])
                }),
                remove_file: Arc::new(|_path: &Path| Err(stand_in_failure())),
                ..TraceEffects::of_the_operating_system(frozen_clocks(NOW_MS))
            },
            TraceBounds {
                directory_bytes: SMALL_DIRECTORY_BYTES,
                freshness: Duration::from_millis(SMALL_FRESHNESS_MS),
                ..TraceBounds::SPEC
            },
        );

        assert_eq!(refusal.action, TraceAction::RemoveOldFile);
        assert_eq!(refusal.path, traces.join(STALE_EVENTS_FILE));
        assert_eq!(names_in(&traces), Vec::<String>::new());
        drop(fs::remove_dir_all(&root));
    }

    /// Trace files held in memory, whose events-file writes, creations and renames fail at the attempts a test names, each counted from 0, and whose torn writes take half their bytes before the next attempt fails.
    #[derive(Clone, Default)]
    struct MemoryFiles(Arc<Mutex<MemoryState>>);

    /// What the memory files hold and how many attempts of each kind they saw.
    #[derive(Default)]
    struct MemoryState {
        held: BTreeMap<PathBuf, Vec<u8>>,
        failing_writes: Vec<usize>,
        torn_writes: Vec<usize>,
        events_lengths: Vec<u64>,
        refuses_shrinking: bool,
        failing_creations: Vec<usize>,
        failing_renames: Vec<usize>,
        writes: usize,
        creations: usize,
        renames: usize,
    }

    /// One file of `MemoryFiles`, appending to what it holds at its path.
    struct MemoryFile {
        path: PathBuf,
        files: MemoryFiles,
        position: usize,
    }

    impl Write for MemoryFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let taken = self.files.write_at(&self.path, self.position, bytes)?;
            self.position = self.position.saturating_add(taken);
            Ok(taken)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for MemoryFile {
        fn set_length(&mut self, length: u64) -> io::Result<()> {
            self.files.set_len_of(&self.path, length)
        }
    }

    /// Whether the path names an events file.
    fn is_events_file(path: &Path) -> bool {
        path.to_str()
            .is_some_and(|path| path.ends_with(EVENTS_FILE_SUFFIX))
    }

    /// Counts one more attempt and says whether it is one of the failing ones.
    fn is_failing_attempt(attempts: &mut usize, failing: &[usize]) -> bool {
        let attempt = *attempts;
        *attempts = attempt.saturating_add(1);
        failing.contains(&attempt)
    }

    impl MemoryFiles {
        /// Files whose events-file writes, creations and renames fail at the attempts named.
        fn failing(writes: &[usize], creations: &[usize], renames: &[usize]) -> Self {
            Self(Arc::new(Mutex::new(MemoryState {
                failing_writes: writes.to_vec(),
                failing_creations: creations.to_vec(),
                failing_renames: renames.to_vec(),
                ..MemoryState::default()
            })))
        }

        /// Files whose events-file write at attempt `write` takes half its bytes, so the attempt after it fails.
        fn torn_at(write: usize) -> Self {
            Self(Arc::new(Mutex::new(MemoryState {
                torn_writes: vec![write],
                ..MemoryState::default()
            })))
        }

        /// Files that refuse to set any file shorter than it is.
        fn refusing_to_shrink() -> Self {
            Self(Arc::new(Mutex::new(MemoryState {
                refuses_shrinking: true,
                ..MemoryState::default()
            })))
        }

        /// The files and attempt counts, locked.
        fn state(&self) -> std::sync::MutexGuard<'_, MemoryState> {
            self.0.lock().unwrap()
        }

        /// Trace effects whose files are these, the rest the operating system's, with the wall clock frozen at `wall_ms`.
        fn effects(&self, wall_ms: u64) -> TraceEffects {
            let creating = self.clone();
            let renaming = self.clone();
            TraceEffects {
                create_file: Arc::new(move |path: &Path| creating.create(path)),
                rename_file: Arc::new(move |from: &Path, to: &Path| renaming.rename(from, to)),
                ..TraceEffects::of_the_operating_system(frozen_clocks(wall_ms))
            }
        }

        /// Creates an empty file at the path, unless this is a failing creation of an events file.
        fn create(&self, path: &Path) -> io::Result<Box<dyn TraceFile>> {
            let mut state = self.state();
            if is_events_file(path) {
                let MemoryState {
                    creations,
                    failing_creations,
                    ..
                } = &mut *state;
                if is_failing_attempt(creations, failing_creations) {
                    return Err(stand_in_failure());
                }
            }
            state.held.insert(path.to_path_buf(), Vec::new());
            drop(state);
            Ok(Box::new(MemoryFile {
                path: path.to_path_buf(),
                files: self.clone(),
                position: 0,
            }))
        }

        /// Moves what `from` holds to `to`, unless this is a failing rename.
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            let mut state = self.state();
            let MemoryState {
                renames,
                failing_renames,
                ..
            } = &mut *state;
            if is_failing_attempt(renames, failing_renames) {
                return Err(stand_in_failure());
            }
            let moved = state.held.remove(from).unwrap_or_default();
            state.held.insert(to.to_path_buf(), moved);
            drop(state);
            Ok(())
        }

        /// Writes at `position` what the attempt takes of `bytes` and says how many bytes it took.
        fn write_at(&self, path: &Path, position: usize, bytes: &[u8]) -> io::Result<usize> {
            let mut state = self.state();
            let mut taken = bytes;
            if is_events_file(path) {
                let MemoryState {
                    writes,
                    failing_writes,
                    torn_writes,
                    ..
                } = &mut *state;
                let attempt = *writes;
                if is_failing_attempt(writes, failing_writes) {
                    return Err(stand_in_failure());
                }
                if torn_writes.contains(&attempt) {
                    failing_writes.push(*writes);
                    taken = &bytes[..bytes.len() / 2];
                }
            }
            let held = state.held.entry(path.to_path_buf()).or_default();
            let end = position.saturating_add(taken.len());
            if held.len() < end {
                held.resize(end, 0);
            }
            held[position..end].copy_from_slice(taken);
            drop(state);
            Ok(taken.len())
        }

        /// Sets the length of the file at the path, the part past what it held reading as zeros, noting each length an events file is given, unless these files refuse to shrink it.
        fn set_len_of(&self, path: &Path, length: u64) -> io::Result<()> {
            let mut state = self.state();
            let held_length = usize::try_from(length).map_err(io::Error::other)?;
            let refuses_shrinking = state.refuses_shrinking;
            let held = state.held.entry(path.to_path_buf()).or_default();
            if refuses_shrinking && held_length < held.len() {
                return Err(stand_in_failure());
            }
            held.resize(held_length, 0);
            if is_events_file(path) {
                state.events_lengths.push(length);
            }
            drop(state);
            Ok(())
        }

        /// The text the file holds.
        fn text_of(&self, path: &Path) -> String {
            String::from_utf8(self.state().held.get(path).cloned().unwrap_or_default()).unwrap()
        }

        /// Each line the file holds as its number, its event and the count a `dropped` line carries.
        fn numbered_events_of(&self, path: &Path) -> Vec<(u64, String, Option<u64>)> {
            self.text_of(path).lines().map(numbered_event).collect()
        }
    }

    /// The line's number, its event and the count a `dropped` line carries.
    fn numbered_event(line: &str) -> (u64, String, Option<u64>) {
        let line: Value = serde_json::from_str(line).unwrap();
        (
            line["n"].as_u64().unwrap(),
            line["event"].as_str().unwrap().to_owned(),
            line["count"].as_u64(),
        )
    }

    /// A trace started with the given effects and bounds.
    fn started_with(
        directory: &TraceDirectory,
        effects: &TraceEffects,
        bounds: TraceBounds,
    ) -> Trace {
        Trace::start(
            directory,
            &OperatingSystemFilesystem,
            RunId::from_random_bytes(RUN_BYTES),
            effects,
            bounds,
        )
        .unwrap()
    }

    /// A clock event numbered `n`, as `numbered_event` reads it.
    fn clock_event(n: u64) -> (u64, String, Option<u64>) {
        (n, CLOCK_EVENT.to_owned(), None)
    }

    /// A `dropped` line numbered `n` carrying `count`, as `numbered_event` reads it.
    fn dropped_event(n: u64, count: u64) -> (u64, String, Option<u64>) {
        (n, DROPPED_EVENT.to_owned(), Some(count))
    }

    #[test]
    fn observability_decision_12_a_line_whose_write_fails_is_counted_and_the_next_line_written_is_a_dropped_event_where_n_skips()
     {
        let (root, directory) = scratch_trace_directory("failed-writes");
        let files = MemoryFiles::failing(&[1, 2], &[], &[]);
        let trace = started_with(&directory, &files.effects(NOW_MS), TraceBounds::SPEC);

        for _event in 0..4 {
            trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        }
        assert!(matches!(trace.finish(), TraceStop::Drained));

        assert_eq!(
            files.numbered_events_of(&trace.files().events),
            [clock_event(0), dropped_event(3, 2), clock_event(4)]
        );
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_12_a_line_written_in_part_before_its_write_fails_is_counted_and_the_dropped_line_after_it_stands_whole_on_its_own_line()
     {
        let (root, directory) = scratch_trace_directory("torn-write");
        let files = MemoryFiles::torn_at(1);
        let trace = started_with(&directory, &files.effects(NOW_MS), TraceBounds::SPEC);

        for _event in 0..3 {
            trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        }
        assert!(matches!(trace.finish(), TraceStop::Drained));

        let held = files.text_of(&trace.files().events);
        let lines: Vec<&str> = held.lines().collect();
        let [first, fragment, dropped, last] = lines.as_slice() else {
            panic!("four lines, the fragment on its own: {held:?}");
        };
        let torn_line = clock_line(1, None).into_bytes();
        assert_eq!(fragment.as_bytes(), &torn_line[..torn_line.len() / 2]);
        assert_eq!(
            [first, dropped, last].map(|line| numbered_event(line)),
            [clock_event(0), dropped_event(2, 1), clock_event(3)]
        );
        drop(fs::remove_dir_all(&root));
    }

    /// An events file the test holds back: each write announces itself, waits for the test's release, then hands the line over.
    struct HeldBackEventsFile {
        entered: Sender<()>,
        released: Receiver<()>,
        written: Sender<String>,
    }

    impl TraceFile for HeldBackEventsFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for io::Sink {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    impl Write for HeldBackEventsFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.send(()).map_err(io::Error::other)?;
            self.released.recv().map_err(io::Error::other)?;
            self.written
                .send(String::from_utf8_lossy(bytes).into_owned())
                .map_err(io::Error::other)?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The test's ends of a held-back events file: where a write announces itself, where the test releases it, and where its line arrives.
    struct HeldBackEvents {
        entering: Receiver<()>,
        release: Sender<()>,
        written: Receiver<String>,
    }

    /// A trace whose events file the test holds back and whose raw file is discarded, its queues `SMALL_QUEUE_LINES` long.
    fn held_back_trace(directory: &TraceDirectory) -> (Trace, HeldBackEvents) {
        let (entered, entering) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (written_sender, written) = mpsc::channel();
        let events_file = Mutex::new(Some(HeldBackEventsFile {
            entered,
            released,
            written: written_sender,
        }));
        let effects = TraceEffects {
            create_file: Arc::new(move |path: &Path| -> io::Result<Box<dyn TraceFile>> {
                let held_back = is_events_file(path)
                    .then(|| events_file.lock().unwrap().take())
                    .flatten();
                if let Some(file) = held_back {
                    return Ok(Box::new(file));
                }
                Ok(Box::new(io::sink()))
            }),
            ..TraceEffects::of_the_operating_system(frozen_clocks(NOW_MS))
        };
        let trace = started_with(
            directory,
            &effects,
            TraceBounds {
                queue_lines: SMALL_QUEUE_LINES,
                drain: TEST_DRAIN_BOUND,
                ..TraceBounds::SPEC
            },
        );
        (
            trace,
            HeldBackEvents {
                entering,
                release,
                written,
            },
        )
    }

    /// What the receiver gets within `WRITER_STEP_BOUND`, failing with the condition's name when it gets nothing.
    fn within_bound<T>(receiver: &Receiver<T>, condition: &str) -> T {
        receiver
            .recv_timeout(WRITER_STEP_BOUND)
            .unwrap_or_else(|failure| panic!("{condition} within {WRITER_STEP_BOUND:?}: {failure}"))
    }

    /// The next line the held-back events file writes once the test releases it.
    fn written_line(events: &HeldBackEvents) -> (u64, String, Option<u64>) {
        events.release.send(()).unwrap();
        numbered_event(&within_bound(&events.written, "a released line is written"))
    }

    #[test]
    fn observability_decision_12_a_full_queue_counts_each_lost_event_and_the_next_line_is_a_dropped_event_where_n_skips()
     {
        let (root, directory) = scratch_trace_directory("full-queue");
        let (trace, events) = held_back_trace(&directory);

        trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        within_bound(&events.entering, "the writer holds the first line");
        for _queued_or_lost in 0..4 {
            trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        }
        let held: Vec<_> = (0..3).map(|_line| written_line(&events)).collect();
        trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        let after: Vec<_> = (0..2).map(|_line| written_line(&events)).collect();

        assert_eq!(held, [clock_event(0), clock_event(1), clock_event(2)]);
        assert_eq!(after, [dropped_event(5, 2), clock_event(6)]);
        drop(events.release);
        assert!(matches!(trace.finish(), TraceStop::Drained));
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_12_events_lost_after_the_last_line_queued_are_reported_by_a_dropped_event_at_the_close()
     {
        let (root, directory) = scratch_trace_directory("loss-at-close");
        let (trace, events) = held_back_trace(&directory);
        trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        within_bound(&events.entering, "the writer holds the first line");
        for _queued_or_lost in 0..4 {
            trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        }
        let HeldBackEvents {
            entering,
            release,
            written,
        } = events;
        let releasing = std::thread::spawn(move || {
            release.send(()).unwrap();
            for _line in 0..=SMALL_QUEUE_LINES {
                within_bound(&entering, "the writer holds a later line");
                release.send(()).unwrap();
            }
        });

        let stop = trace.finish();

        assert!(matches!(stop, TraceStop::Drained), "{stop:?}");
        let lines: Vec<_> = written
            .try_iter()
            .map(|line| numbered_event(&line))
            .collect();
        assert_eq!(
            lines,
            [
                clock_event(0),
                clock_event(1),
                clock_event(2),
                dropped_event(5, 2)
            ]
        );
        releasing.join().unwrap();
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_12_finish_returns_once_every_queued_line_is_written() {
        let (root, directory) = scratch_trace_directory("finish-drains");
        let (trace, events) = held_back_trace(&directory);
        trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        within_bound(&events.entering, "the writer holds the first line");
        for _queued in 0..SMALL_QUEUE_LINES {
            trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        }
        let HeldBackEvents {
            entering,
            release,
            written,
        } = events;
        let releasing = std::thread::spawn(move || {
            release.send(()).unwrap();
            for _line in 0..SMALL_QUEUE_LINES {
                within_bound(&entering, "the writer holds a queued line");
                release.send(()).unwrap();
            }
        });

        let stop = trace.finish();

        assert!(matches!(stop, TraceStop::Drained), "{stop:?}");
        assert_eq!(written.try_iter().count(), SMALL_QUEUE_LINES + 1);
        releasing.join().unwrap();
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_6_a_trace_file_is_sized_ahead_of_its_lines_and_cut_back_to_them_at_a_clean_stop()
     {
        let (root, directory) = scratch_trace_directory("sized-ahead");
        let files = MemoryFiles::default();
        let trace = started_with(&directory, &files.effects(NOW_MS), TraceBounds::SPEC);

        trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        assert!(matches!(trace.finish(), TraceStop::Drained));

        let line = clock_line(0, None);
        assert_eq!(
            files.state().events_lengths,
            [
                TRACE_FILE_SIZED_AHEAD_BYTES,
                u64::try_from(line.len()).unwrap()
            ]
        );
        assert_eq!(files.text_of(&trace.files().events), line);
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_12_a_clean_stop_that_cannot_cut_a_file_back_to_its_last_line_says_so()
    {
        let (root, directory) = scratch_trace_directory("not-cut");
        let files = MemoryFiles::refusing_to_shrink();
        let trace = started_with(&directory, &files.effects(NOW_MS), TraceBounds::SPEC);

        trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        let stop = trace.finish();

        assert!(matches!(stop, TraceStop::NotCut(_)), "{stop:?}");
        assert_eq!(
            files.text_of(&trace.files().events).len(),
            usize::try_from(TRACE_FILE_SIZED_AHEAD_BYTES).unwrap()
        );
        drop(fs::remove_dir_all(&root));
    }

    /// The lengths the events file was given while it took one line carrying `text_bytes` bytes of text and stopped, and the bytes it then held.
    fn lengths_given_for_one_line(directory: &TraceDirectory, text_bytes: u64) -> (Vec<u64>, u64) {
        let files = MemoryFiles::default();
        let trace = started_with(directory, &files.effects(NOW_MS), TraceBounds::SPEC);
        trace.events.write(
            CLOCK_EVENT,
            &[(
                "text",
                Value::from("x".repeat(usize::try_from(text_bytes).unwrap())),
            )],
        );
        assert!(matches!(trace.finish(), TraceStop::Drained), "{text_bytes}");
        let held = files.text_of(&trace.files().events);
        assert!(held.ends_with("\"}\n"), "{text_bytes}: {}", held.len());
        let lengths = files.state().events_lengths.clone();
        (lengths, u64::try_from(held.len()).unwrap())
    }

    #[test]
    fn observability_decision_6_a_line_is_sized_ahead_by_the_fewest_whole_steps_that_hold_it_and_a_clean_stop_cuts_back_to_it()
     {
        const STEP_COUNTS: [u64; 3] = [1, 2, 3];
        const SEEDED_CASES: u64 = 20;
        const SEEDED_TEXT_BOUND: u64 = 4 * TRACE_FILE_SIZED_AHEAD_BYTES;
        let (root, directory) = scratch_trace_directory("sized-ahead-lines");
        let (_, line_bytes_without_text) = lengths_given_for_one_line(&directory, 0);
        let at_and_around_each_step = STEP_COUNTS.iter().flat_map(|steps| {
            let boundary = steps.saturating_mul(TRACE_FILE_SIZED_AHEAD_BYTES);
            [
                boundary.saturating_sub(1),
                boundary,
                boundary.saturating_add(1),
            ]
            .map(|line_bytes| line_bytes.saturating_sub(line_bytes_without_text))
        });
        let seeded = (0..SEEDED_CASES).map(|case| Draws::seeded(case).below(SEEDED_TEXT_BOUND));

        for text_bytes in at_and_around_each_step.chain(seeded) {
            let (lengths, line_bytes) = lengths_given_for_one_line(&directory, text_bytes);

            assert_eq!(
                line_bytes,
                line_bytes_without_text.saturating_add(text_bytes),
                "{text_bytes}"
            );
            assert_eq!(
                lengths,
                [
                    line_bytes.next_multiple_of(TRACE_FILE_SIZED_AHEAD_BYTES),
                    line_bytes
                ],
                "{text_bytes}"
            );
        }
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_13_a_rollover_whose_rename_or_new_file_fails_loses_only_counted_lines_and_the_next_line_written_is_a_dropped_event()
     {
        let (root, directory) = scratch_trace_directory("failed-rollover");
        let files = MemoryFiles::failing(&[], &[1], &[0]);
        let line_bytes = u64::try_from(clock_line(0, None).len()).unwrap();
        let trace = started_with(
            &directory,
            &files.effects(NOW_MS),
            TraceBounds {
                rollover_bytes: line_bytes * LINES_PER_FILE,
                ..TraceBounds::SPEC
            },
        );

        for _event in 0..LINES_WRITTEN {
            trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        }
        assert!(matches!(trace.finish(), TraceStop::Drained));

        let events = &trace.files().events;
        assert_eq!(
            files.numbered_events_of(&crate::dirs::rolled_over_trace_file(events)),
            [clock_event(0), clock_event(1)]
        );
        assert_eq!(
            files.numbered_events_of(events),
            [dropped_event(4, 2), clock_event(5)]
        );
        drop(fs::remove_dir_all(&root));
    }

    /// Creates a file of `bytes` bytes in the trace directory, last written at `written_ms`.
    fn stored_run_file(traces: &Path, name: &str, bytes: u64, written_ms: u64) {
        let file = File::create(traces.join(name)).unwrap();
        file.set_len(bytes).unwrap();
        file.set_modified(
            UNIX_EPOCH
                .checked_add(Duration::from_millis(written_ms))
                .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn observability_decision_13_a_start_removes_the_oldest_stale_runs_both_files_together_until_the_directory_is_within_its_bound()
     {
        let (root, directory) = scratch_trace_directory("prune");
        let traces = directory.path();
        fs::create_dir_all(&traces).unwrap();
        let fresh_at_the_bound = NOW_MS - SMALL_FRESHNESS_MS;
        let stale_by_one = NOW_MS - SMALL_FRESHNESS_MS - 1;
        let run_a = "20000101T000000Z-0000000000000001-estafette";
        let run_b = "20000102T000000Z-0000000000000002-estafette";
        let run_c = "20000103T000000Z-0000000000000003-estafette";
        let run_d = "20000104T000000Z-0000000000000004-estafette";
        stored_run_file(
            &traces,
            &format!("{run_a}.events.jsonl"),
            200,
            fresh_at_the_bound,
        );
        stored_run_file(&traces, &format!("{run_b}.events.jsonl"), 150, stale_by_one);
        stored_run_file(
            &traces,
            &format!("{run_b}.events.jsonl.1"),
            50,
            stale_by_one,
        );
        stored_run_file(&traces, &format!("{run_b}.raw.jsonl"), 100, NOW_MS);
        stored_run_file(&traces, &format!("{run_c}.events.jsonl"), 400, stale_by_one);
        stored_run_file(&traces, &format!("{run_c}.raw.jsonl"), 100, stale_by_one);
        stored_run_file(&traces, &format!("{run_d}.events.jsonl"), 300, NOW_MS);

        let trace = started(
            &directory,
            NOW_MS,
            TraceBounds {
                directory_bytes: SMALL_DIRECTORY_BYTES,
                freshness: Duration::from_millis(SMALL_FRESHNESS_MS),
                ..TraceBounds::SPEC
            },
        );
        assert!(matches!(trace.finish(), TraceStop::Drained));

        let kept: Vec<String> = names_in(&traces)
            .into_iter()
            .filter(|name| !name.contains(RUN_HEX))
            .collect();
        assert_eq!(
            kept,
            [
                format!("{run_a}.events.jsonl"),
                format!("{run_c}.events.jsonl"),
                format!("{run_c}.raw.jsonl"),
                format!("{run_d}.events.jsonl"),
            ]
        );
        drop(fs::remove_dir_all(&root));
    }
}
