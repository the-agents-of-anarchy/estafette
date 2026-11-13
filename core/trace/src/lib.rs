use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use data_encoding::HEXLOWER;

mod directory;
mod files;
mod line;
mod operating_system;
mod run;
mod writer;

pub use directory::{
    BaseDirectoryUnresolvable, ROLLED_OVER_TRACE_SUFFIX, TraceDirectory, TraceFileKind, TraceFiles,
    data_base_directory, events_trace_file_name, non_empty_variable, raw_trace_file_name,
    rolled_over_trace_file, trace_file_of, xdg_base_directory,
};
pub use files::{PRODUCT_NAME, Program, run_name_pattern};
pub use run::{BridgeRun, EstafetteRun};
pub use writer::{DROPPED_LINE_NAME, FileTrace};

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
pub const TRACE_FILE_SIZED_AHEAD_BYTES: u64 = 65_536;

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

/// How a trace's stop left one of its files.
#[must_use]
#[derive(Debug)]
pub enum TraceStop {
    /// The writer took every queued line within the drain bound and the file ends at its last line, a line whose write failed missing from it and counted in the next `dropped` line that could be written.
    Drained,
    /// The writer of the file named did not report within the drain bound that it took every queued line.
    NotDrained { file: TraceFileKind },
    /// The writer of the file named took every queued line, but the file was sized past its last line and could not be cut back to it, so zero bytes follow that line.
    NotCut {
        file: TraceFileKind,
        source: io::Error,
    },
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
pub enum TraceAction {
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

    /// What the trace could not do.
    #[must_use]
    pub const fn action(&self) -> TraceAction {
        self.action
    }

    /// The path the trace could not do it to.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
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
