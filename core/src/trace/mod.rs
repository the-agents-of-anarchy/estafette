use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use data_encoding::HEXLOWER;
use iroh::EndpointId;

use serde_json::Value;

use crate::dirs::{Directories, DirectoryFilesystem, TraceFiles};
use crate::event::{Event, EventName, TraceLineName};

mod files;
mod line;
mod writer;

use files::{create_private_file, remove_oldest_runs, run_name};
use line::HeaderSource;
use writer::{RollingFile, TraceWriter};

/// The version of the trace line format, raised when an event name is renamed or removed or a field changes meaning.
pub const FORMAT_VERSION: u64 = 1;
/// The size at which a trace file rolls over into its `.1` file: 64 MiB.
pub const TRACE_FILE_ROLLOVER_BYTES: u64 = 67_108_864;
/// The most one run keeps of one trace file, the file and its `.1` file together: 128 MiB.
pub const TRACE_RUN_FILE_MAX_BYTES: u64 = 134_217_728;
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
pub struct TraceClocks {
    /// Milliseconds on a clock that never goes back, from the process start.
    pub monotonic_ms: Clock,
    /// Milliseconds since the Unix epoch by this machine's clock.
    pub wall_ms: Clock,
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
        directories: &Directories,
        filesystem: &impl DirectoryFilesystem,
        run: RunId,
        clocks: TraceClocks,
        bounds: TraceBounds,
    ) -> Result<Self, TraceRefused> {
        let traces = directories.traces_directory();
        filesystem
            .create_private_directory(&traces)
            .map_err(|source| TraceRefused::new(TraceAction::CreateDirectory, &traces, source))?;
        let header = Arc::new(HeaderSource::new(run, clocks));
        let started_ms = header.wall_ms();
        remove_oldest_runs(&traces, started_ms, &bounds)?;
        let files = directories.trace_files(&run_name(started_ms, &run.hex()));
        let events = start_writer(&files.events, &header, &bounds)?;
        let raw = start_writer(&files.raw, &header, &bounds)?;
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
        self.header.note_node(id.as_bytes());
    }

    /// Closes both queues and waits, within the drain bound for each, until their lines are on disk, saying whether every line got there.
    pub fn finish(&self) -> bool {
        let events_drained = self.events.finish(self.drain);
        let raw_drained = self.raw.finish(self.drain);
        events_drained && raw_drained
    }
}

/// Creates the trace file and starts its writer.
fn start_writer(
    path: &Path,
    header: &Arc<HeaderSource>,
    bounds: &TraceBounds,
) -> Result<TraceWriter, TraceRefused> {
    let file = create_private_file(path)
        .map_err(|source| TraceRefused::new(TraceAction::CreateFile, path, source))?;
    TraceWriter::start(
        RollingFile::new(path.to_path_buf(), file, bounds.rollover_bytes),
        Arc::clone(header),
        bounds.queue_lines,
    )
    .map_err(|source| TraceRefused::new(TraceAction::StartWriter, path, source))
}

#[cfg(test)]
pub(super) mod tests {
    use std::ffi::OsString;
    use std::fs::{self, File};
    use std::path::Path;
    use std::time::{Duration, UNIX_EPOCH};

    use iroh::SecretKey;

    use super::*;
    use crate::dirs::OperatingSystemFilesystem;
    use crate::event::{InboxEvent, ProcessEvent};
    use crate::node::NodeError;

    const SPEC_RAW_FIRST_LINE_AFTER_HEADER: &str =
        ",\"event\":\"raw_file_opened\",\"safe_to_post\":false}\n";
    const LEAP_DAY_LAST_SECOND_MS: u64 = 951_868_799_123;
    const FROZEN_MONOTONIC_MS: u64 = 7;
    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
    const RUN_HEX: &str = "a1b2c3d4e5f60718";
    const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
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

    pub(super) fn scratch_directories(test_name: &str) -> (PathBuf, Directories) {
        let root = std::env::temp_dir().join(format!(
            "estafette-trace-{test_name}-{}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        let base = root.clone();
        let directories = Directories::establish(
            move |name: &str| match name {
                CONFIG_HOME_VARIABLE => Some(OsString::from(base.join("config"))),
                DATA_HOME_VARIABLE => Some(OsString::from(base.join("data"))),
                RUNTIME_DIRECTORY_VARIABLE => Some(OsString::from(base.join("runtime"))),
                _ => None,
            },
            &OperatingSystemFilesystem,
            None,
        )
        .unwrap();
        (root, directories)
    }

    pub(super) fn frozen_clocks(wall_ms: u64) -> TraceClocks {
        TraceClocks {
            monotonic_ms: Arc::new(|| FROZEN_MONOTONIC_MS),
            wall_ms: Arc::new(move || wall_ms),
        }
    }

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
        let (root, directories) = scratch_directories("start");

        let trace = Trace::start(
            &directories,
            &OperatingSystemFilesystem,
            RunId::from_random_bytes(RUN_BYTES),
            frozen_clocks(LEAP_DAY_LAST_SECOND_MS),
            TraceBounds::SPEC,
        )
        .unwrap();
        trace.finish();

        let run_name = format!("20000229T235959Z-{RUN_HEX}-estafette");
        let traces = directories.traces_directory();
        assert_eq!(
            names_in(&traces),
            [
                format!("{run_name}.events.jsonl"),
                format!("{run_name}.raw.jsonl")
            ]
        );
        assert_eq!(trace.files(), &directories.trace_files(&run_name));
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

    fn started(directories: &Directories, wall_ms: u64, bounds: TraceBounds) -> Trace {
        Trace::start(
            directories,
            &OperatingSystemFilesystem,
            RunId::from_random_bytes(RUN_BYTES),
            frozen_clocks(wall_ms),
            bounds,
        )
        .unwrap()
    }

    fn clock_line(n: u64, node: Option<&str>) -> String {
        let node = node.map_or_else(String::new, |node| format!(",\"node\":\"{node}\""));
        format!(
            "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":{n},\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{NOW_MS}{node},\"event\":\"process.clock_before_unix_epoch\"}}\n"
        )
    }

    #[test]
    fn observability_decision_4_the_start_in_a_name_is_its_utc_second_across_month_year_and_leap_edges()
     {
        let (root, directories) = scratch_directories("stamps");

        for (wall_ms, _stamp) in STAMPED_STARTS {
            assert!(started(&directories, wall_ms, TraceBounds::SPEC).finish());
        }

        let names = names_in(&directories.traces_directory());
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
        let (root, directories) = scratch_directories("events");
        let trace = started(&directories, NOW_MS, TraceBounds::SPEC);
        let id = SecretKey::from_bytes(&[ENDPOINT_SEED; 32]).public();
        let node = data_encoding::HEXLOWER.encode(id.as_bytes());
        let node = node.get(..SPEC_NODE_HEX_CHARS).unwrap();

        trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        trace.note_endpoint(id);
        trace.record(&ProcessEvent::ClaimNetworkRefused(NodeError::TopicClosedBeforeHandle).into());
        trace.record(&InboxEvent::AcceptFailed(io::Error::other("a marker")).into());
        assert!(trace.finish());

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
        let (root, directories) = scratch_directories("rollover");
        let line_bytes = u64::try_from(clock_line(0, None).len()).unwrap();
        let trace = started(
            &directories,
            NOW_MS,
            TraceBounds {
                rollover_bytes: line_bytes * LINES_PER_FILE,
                ..TraceBounds::SPEC
            },
        );

        for _line in 0..LINES_WRITTEN {
            trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        }
        assert!(trace.finish());

        let events = &trace.files().events;
        assert_eq!(fs::read_to_string(events).unwrap(), clock_line(4, None));
        assert_eq!(
            fs::read_to_string(crate::dirs::rolled_over_trace_file(events)).unwrap(),
            format!("{}{}", clock_line(2, None), clock_line(3, None))
        );
        assert_eq!(names_in(&directories.traces_directory()).len(), 3);
        drop(fs::remove_dir_all(&root));
    }

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
        let (root, directories) = scratch_directories("prune");
        let traces = directories.traces_directory();
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
            &directories,
            NOW_MS,
            TraceBounds {
                directory_bytes: SMALL_DIRECTORY_BYTES,
                freshness: Duration::from_millis(SMALL_FRESHNESS_MS),
                ..TraceBounds::SPEC
            },
        );
        assert!(trace.finish());

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
