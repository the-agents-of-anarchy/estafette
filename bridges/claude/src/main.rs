use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use estafette_bridge_claude::discover::{
    BinarySocket, POLL_INTERVAL, Pid, ProcessTable, ProcessTableError, PsProcessTable,
    RuntimeDirectoryInputs, follow_binary_sockets, runtime_directory,
};
use estafette_bridge_claude::doctor::{
    DoctorInputs, MESSAGING_SOCKET_VARIABLE, MessagingSocket, RUN_SUBCOMMAND, SettingsFile,
    dropped_frames_path, permission_class_path, run_doctor, running_bridge_for,
};
use estafette_bridge_claude::dropped_frames::DroppedFrames;
use estafette_bridge_claude::inbox_client::{
    InboxConnection, InboxConnector, InboxFrame, UnixSocketConnector, follow_inbox,
};
use estafette_bridge_claude::inject::SessionNotifier;
use estafette_bridge_claude::owned_entry::{
    EntryFacts, ProcessUserUnknown, RuntimeDirectoryProblem, check_runtime_directory,
    create_private_directory, process_user_id, read_state_file, write_state_file,
};
use estafette_bridge_claude::permission_class::PermissionClass;
use estafette_bridge_claude::session_files::{
    latest_permission_mode, session_id_of_registry_entry, transcript_file_name,
};

use estafette_trace::{
    BaseDirectoryUnresolvable, Program, ROLLED_OVER_TRACE_SUFFIX, RUN_ID_BYTES,
    TRACE_DIRECTORY_MAX_BYTES, TRACE_FILE_ROLLOVER_BYTES, TRACE_FRESHNESS,
    TRACE_RUN_FILE_MAX_BYTES, TraceBounds, TraceClocks, TraceDirectory, TraceEffects, TraceRefused,
    events_trace_file_name, run_name_pattern,
};

use crate::debug_trace::{DebugTrace, TraceInputs, Tracing};
use crate::event::{BridgeEvent, FrameIds};

mod debug_trace;
mod event;
#[cfg(test)]
#[path = "../../../core/tests/common/fresh_directory.rs"]
mod fresh_directory;
#[cfg(test)]
#[path = "../../../core/tests/common/remove_scratch.rs"]
mod remove_scratch;

const DOCTOR_SUBCOMMAND: &str = "doctor";
const BRIDGE_PROGRAM: &str = "estafette-bridge-claude";
const RUN_FORM: &str = "run <harness-pid> [--debug | --no-debug]";
const DEBUG_FLAG: &str = "--debug";
const NO_DEBUG_FLAG: &str = "--no-debug";
const HELP_FLAG: &str = "--help";
const BYTES_PER_MEBIBYTE: u64 = 1_048_576;
const MEBIBYTES_PER_GIBIBYTE: u64 = 1024;
const SECONDS_PER_MINUTE: u64 = 60;
const SECURE_RANDOM_DEVICE: &str = "/dev/urandom";
const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_CHECK_FAILED: u8 = 3;
const MESSAGING_TOKEN_VARIABLE: &str = "CLAUDE_CODE_MESSAGING_TOKEN";
const HOME_VARIABLE: &str = "HOME";
const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
const CONFIG_HOME_UNDER_HOME: &str = ".config";
const PRODUCT_DIRECTORY: &str = "estafette";
const CLAUDE_CONFIG_DIR_VARIABLE: &str = "CLAUDE_CONFIG_DIR";
const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
const TEMPORARY_DIRECTORY_VARIABLE: &str = "TMPDIR";
const SYSTEM_TEMPORARY_DIRECTORY: &str = "/tmp";
const CLAUDE_DIRECTORY: &str = ".claude";
const SESSIONS_DIRECTORY: &str = "sessions";
const PROJECTS_DIRECTORY: &str = "projects";
const REGISTRY_ENTRY_EXTENSION: &str = "json";
const CLASS_BEFORE_THE_LIVE_MODE_IS_READ: PermissionClass = PermissionClass::Prompting;
const TRANSCRIPT_TAIL_WINDOW_BYTES: u64 = 65_536;
const SETTINGS_FILE: &str = "settings.json";
const LOCAL_SETTINGS_FILE: &str = "settings.local.json";
#[cfg(target_os = "macos")]
const MANAGED_SETTINGS_DIRECTORY: &str = "/Library/Application Support/ClaudeCode";
#[cfg(not(target_os = "macos"))]
const MANAGED_SETTINGS_DIRECTORY: &str = "/etc/claude-code";
const MANAGED_SETTINGS_FILE: &str = "managed-settings.json";
const MANAGED_DROP_IN_DIRECTORY: &str = "managed-settings.d";
const SETTINGS_EXTENSION: &str = "json";
const HIDDEN_FILE_PREFIX: char = '.';
const STAGING_SUFFIX: &str = ".staging";
/// How often a serving bridge writes its alive line, on its own clock: `spec/observability.md` decision 13 has a live process write its events file at least this often.
const ALIVE_LINE_PERIOD: Duration = Duration::from_secs(60);
/// At most one alive line falls due inside one pause between polls, on a clock that never runs back, while that pause is shorter than the period.
const _: () = assert!(POLL_INTERVAL.as_nanos() < ALIVE_LINE_PERIOD.as_nanos());

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Run { harness_pid: Pid, tracing: Tracing },
    RunHelp,
    Doctor,
}

/// The one line naming every form the command line takes.
fn usage() -> String {
    format!(
        "usage: {BRIDGE_PROGRAM} {RUN_FORM} | {RUN_SUBCOMMAND} {HELP_FLAG} | {DOCTOR_SUBCOMMAND}"
    )
}

/// The clause a trace-start refusal ends with, naming the flag that runs without a trace.
fn trace_skip_hint() -> String {
    format!("run with {NO_DEBUG_FLAG} to write no trace")
}

/// The help of `run`: what it does, reads, writes and refuses, and what its debug trace writes, where, that it is safe to post, and its bounds.
fn run_help() -> String {
    let events = events_trace_file_name(&run_name_pattern(Program::Bridge));
    format!(
        "usage: {BRIDGE_PROGRAM} {RUN_FORM}\n\nServes the Claude Code session whose process is <harness-pid> until that process exits: follows the inbox socket of every estafette mcp process under it and writes one notice per frame into the session's messaging socket. Reads {MESSAGING_SOCKET_VARIABLE} and {MESSAGING_TOKEN_VARIABLE} from the environment and refuses to start without them. Exits 0 at once when another bridge already serves the session.\n\n{DEBUG_FLAG}     Write the debug trace of this run, the default until v1.0.0. Writes one file per run, readable by you alone, in {directory}: {events}, the run's typed events, which is safe to post: it holds no message body, token or socket path. The first line on standard error names the file. Bounds: the file rolls over once at {rollover_mib} MiB into a {ROLLED_OVER_TRACE_SUFFIX} file that replaces any earlier one, so a run keeps at most {run_file_mib} MiB; at each start the oldest runs of the bridge and of estafette are removed, every file of a run together, until the directory holds at most {directory_gib} GiB, keeping every run whose events file was written in the last {freshness_minutes} minutes. A trace that cannot start refuses the run.\n{NO_DEBUG_FLAG}  Write no debug trace for this run: create no trace file and start no trace writer.",
        directory = TraceDirectory::described(),
        rollover_mib = TRACE_FILE_ROLLOVER_BYTES / BYTES_PER_MEBIBYTE,
        run_file_mib = TRACE_RUN_FILE_MAX_BYTES / BYTES_PER_MEBIBYTE,
        directory_gib = TRACE_DIRECTORY_MAX_BYTES / BYTES_PER_MEBIBYTE / MEBIBYTES_PER_GIBIBYTE,
        freshness_minutes = TRACE_FRESHNESS.as_secs() / SECONDS_PER_MINUTE,
    )
}

/// Reads the subcommand and its arguments from the command line, refusing any other form.
fn parse_command(arguments: &[String]) -> Option<Command> {
    match arguments {
        [subcommand, flag] if subcommand == RUN_SUBCOMMAND && flag == HELP_FLAG => {
            Some(Command::RunHelp)
        }
        [subcommand, harness_pid, tracing_flags @ ..] if subcommand == RUN_SUBCOMMAND => {
            Some(Command::Run {
                harness_pid: harness_pid.parse().ok()?,
                tracing: tracing_of_flags(tracing_flags)?,
            })
        }
        [subcommand] if subcommand == DOCTOR_SUBCOMMAND => Some(Command::Doctor),
        _ => None,
    }
}

/// Reads the tracing `run` asks for from the words after the harness pid: none or one of the flag pair, `--debug` when none.
fn tracing_of_flags(flags: &[String]) -> Option<Tracing> {
    match flags {
        [] => Some(Tracing::Debug),
        [flag] if flag == DEBUG_FLAG => Some(Tracing::Debug),
        [flag] if flag == NO_DEBUG_FLAG => Some(Tracing::NoDebug),
        _ => None,
    }
}

struct Environment {
    messaging_socket: Option<PathBuf>,
    messaging_token: Option<String>,
    home: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    claude_config_dir: Option<PathBuf>,
    xdg_runtime_dir: Option<PathBuf>,
    temporary_dir: PathBuf,
    own_pid: Pid,
    working_dir: PathBuf,
    started: Instant,
}

/// Reads an environment variable, treating an empty value as unset.
fn non_empty_variable(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

/// Captures the process environment, the own pid, the working directory and the start time once at the entry.
fn read_environment() -> Result<Environment, CommandError> {
    Ok(Environment {
        messaging_socket: non_empty_variable(MESSAGING_SOCKET_VARIABLE).map(PathBuf::from),
        messaging_token: non_empty_variable(MESSAGING_TOKEN_VARIABLE)
            .and_then(|token| token.into_string().ok()),
        home: non_empty_variable(HOME_VARIABLE).map(PathBuf::from),
        xdg_config_home: non_empty_variable(CONFIG_HOME_VARIABLE).map(PathBuf::from),
        claude_config_dir: non_empty_variable(CLAUDE_CONFIG_DIR_VARIABLE).map(PathBuf::from),
        xdg_runtime_dir: non_empty_variable(RUNTIME_DIRECTORY_VARIABLE).map(PathBuf::from),
        temporary_dir: non_empty_variable(TEMPORARY_DIRECTORY_VARIABLE)
            .map_or_else(|| PathBuf::from(SYSTEM_TEMPORARY_DIRECTORY), PathBuf::from),
        own_pid: Pid(std::process::id()),
        working_dir: std::env::current_dir().map_err(CommandError::WorkingDirectoryUnreadable)?,
        started: Instant::now(),
    })
}

#[derive(Debug)]
enum CommandError {
    MissingVariable(&'static str),
    ConfigBaseUnresolvable,
    ConfigDirectoryCreationFailed { path: PathBuf, source: io::Error },
    ProcessUserUnknown(ProcessUserUnknown),
    SettingsUnreadable { path: PathBuf, source: io::Error },
    WorkingDirectoryUnreadable(io::Error),
    ReportUnwritable(io::Error),
    HelpUnwritable(io::Error),
    ProcessTable(ProcessTableError),
    DataDirectoryUnresolvable(BaseDirectoryUnresolvable),
    RunIdUnreadable(io::Error),
    TraceRefused(TraceRefused),
    RuntimeDirectoryRefused(RuntimeDirectoryProblem),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVariable(name) => write!(formatter, "{name} is not set"),
            Self::ConfigBaseUnresolvable => write!(
                formatter,
                "{CONFIG_HOME_VARIABLE} and {HOME_VARIABLE} are both unset or empty: set one of them"
            ),
            Self::ConfigDirectoryCreationFailed { path, source } => {
                write!(formatter, "cannot create {}: {source}", path.display())
            }
            Self::ProcessUserUnknown(source) => source.fmt(formatter),
            Self::SettingsUnreadable { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::WorkingDirectoryUnreadable(source) => {
                write!(formatter, "cannot read the working directory: {source}")
            }
            Self::ReportUnwritable(source) => {
                write!(formatter, "cannot write the report: {source}")
            }
            Self::HelpUnwritable(source) => {
                write!(formatter, "cannot write the help: {source}")
            }
            Self::ProcessTable(source) => source.fmt(formatter),
            Self::DataDirectoryUnresolvable(source) => write!(
                formatter,
                "cannot place the debug trace: {source}; {}",
                trace_skip_hint()
            ),
            Self::RunIdUnreadable(source) => write!(
                formatter,
                "cannot read the secure random source for the run id of the debug trace: {source}; {}",
                trace_skip_hint()
            ),
            Self::TraceRefused(source) => source.fmt(formatter),
            Self::RuntimeDirectoryRefused(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for CommandError {}

/// The file system reads and writes the commands make, handed in from the entry.
trait Files {
    /// Reads a whole file as text.
    fn read_text(&self, path: &Path) -> io::Result<String>;
    /// Reads the last bytes of a file, at most the window, as text.
    fn read_tail_text(&self, path: &Path, window_bytes: u64) -> io::Result<String>;
    /// Lists the paths inside a directory.
    fn list_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    /// Tells whether anything exists at the path.
    fn exists(&self, path: &Path) -> bool;
    /// Creates the directory with the private mode and its missing parents, refusing an existing directory that is not a private directory of the user.
    fn create_private_directory(&self, path: &Path) -> io::Result<()>;
    /// Gives the numeric id of the user this process runs as, learned once at the entry.
    fn user_id(&self) -> u32;
    /// Reads the facts of the entry at the path without following a symbolic link there.
    fn entry_facts(&self, path: &Path) -> io::Result<EntryFacts>;
    /// Reads a state file of the bridge as text, refusing anything but a regular file of the user.
    fn read_state_text(&self, path: &Path) -> io::Result<String>;
    /// Writes a state file by staging it beside the final name and renaming it over that name, refusing a final name that is a symbolic link or another user's file.
    fn write_text_atomically(&self, path: &Path, text: &str) -> io::Result<()>;
}

/// The operating system's files, read and written as the user this process runs as.
#[derive(Clone, Copy)]
struct OperatingSystemFiles {
    user_id: u32,
}

impl OperatingSystemFiles {
    /// The files as the user of this process uses them.
    fn of_this_process() -> Result<Self, CommandError> {
        process_user_id()
            .map(|user_id| Self { user_id })
            .map_err(CommandError::ProcessUserUnknown)
    }
}

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Names a temporary file beside the path that no other staged write of this process uses.
fn unique_staging_path(path: &Path) -> PathBuf {
    let sequence_number = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut staging_name = path.as_os_str().to_owned();
    staging_name.push(format!(".{sequence_number}{STAGING_SUFFIX}"));
    PathBuf::from(staging_name)
}

impl Files for OperatingSystemFiles {
    fn read_text(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn read_tail_text(&self, path: &Path, window_bytes: u64) -> io::Result<String> {
        let mut file = fs::File::open(path)?;
        let length = file.metadata()?.len();
        file.seek(SeekFrom::Start(length.saturating_sub(window_bytes)))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn list_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn create_private_directory(&self, path: &Path) -> io::Result<()> {
        create_private_directory(self.user_id, path)
    }

    fn user_id(&self) -> u32 {
        self.user_id
    }

    fn entry_facts(&self, path: &Path) -> io::Result<EntryFacts> {
        EntryFacts::at(path)
    }

    fn read_state_text(&self, path: &Path) -> io::Result<String> {
        read_state_file(path, self.user_id)
    }

    fn write_text_atomically(&self, path: &Path, text: &str) -> io::Result<()> {
        write_state_file(self.user_id, path, &unique_staging_path(path), text)
    }
}

/// Names the config directory the way core resolves it, from the config base variable or else the home directory.
fn core_config_directory(environment: &Environment) -> Result<PathBuf, CommandError> {
    environment
        .xdg_config_home
        .clone()
        .or_else(|| {
            environment
                .home
                .as_ref()
                .map(|home| home.join(CONFIG_HOME_UNDER_HOME))
        })
        .map(|base| base.join(PRODUCT_DIRECTORY))
        .ok_or(CommandError::ConfigBaseUnresolvable)
}

/// Creates the config directory as core does, so a bridge started before core's first run finds it.
fn create_config_directory(
    environment: &Environment,
    files: &impl Files,
) -> Result<(), CommandError> {
    let config_directory = core_config_directory(environment)?;
    files
        .create_private_directory(&config_directory)
        .map_err(|source| CommandError::ConfigDirectoryCreationFailed {
            path: config_directory,
            source,
        })
}

/// Names the runtime directory inputs, taking the user id from the user the process runs as.
fn runtime_inputs(environment: &Environment, files: &impl Files) -> RuntimeDirectoryInputs {
    RuntimeDirectoryInputs {
        xdg_runtime_dir: environment.xdg_runtime_dir.clone(),
        temporary_dir: environment.temporary_dir.clone(),
        user_id: files.user_id(),
    }
}

/// Refuses the runtime directory when it exists and is not a directory of the user that grants nothing to group or other; a missing one holds no socket to connect to.
fn require_usable_runtime_directory(
    files: &impl Files,
    inputs: &RuntimeDirectoryInputs,
) -> Result<(), CommandError> {
    check_runtime_directory(inputs.user_id, &runtime_directory(inputs), |directory| {
        files.entry_facts(directory)
    })
    .map_err(CommandError::RuntimeDirectoryRefused)
}

/// Takes a mutex whether or not a panicking holder poisoned it, which the abort-on-panic release profile makes unreachable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where every event goes, built at the composition root: the run record on standard error and, under `--debug`, the events file; it must not block.
type RunRecord = Arc<dyn Fn(BridgeEvent) + Send + Sync>;
type Elapsed = Arc<dyn Fn() -> Duration + Send + Sync>;
type Task = Box<dyn FnOnce() + Send>;
type Spawner = Arc<dyn Fn(Task) + Send + Sync>;
type Pause = Arc<dyn Fn(Duration) + Send + Sync>;

/// Where the bridge read the session's permission class from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClassSource {
    /// The latest permission-mode entry in the end of the transcript.
    TranscriptTail,
    /// The latest permission-mode entry in the whole transcript, read while no class had been read.
    WholeTranscript,
    /// The class read before, kept because the end of the transcript holds no permission-mode entry.
    KeptFromBefore,
}

#[derive(Debug)]
enum LiveClassError {
    RegistryUnreadable { path: PathBuf, source: io::Error },
    NoSessionId(PathBuf),
    ProjectsUnreadable { path: PathBuf, source: io::Error },
    TranscriptMissing(String),
    TranscriptUnreadable { path: PathBuf, source: io::Error },
    NoModeEntry(PathBuf),
}

impl fmt::Display for LiveClassError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RegistryUnreadable { path, source }
            | Self::TranscriptUnreadable { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::NoSessionId(path) => write!(formatter, "{} names no session", path.display()),
            Self::ProjectsUnreadable { path, source } => {
                write!(formatter, "cannot list {}: {source}", path.display())
            }
            Self::TranscriptMissing(file_name) => {
                write!(formatter, "no project directory holds {file_name}")
            }
            Self::NoModeEntry(path) => {
                write!(formatter, "{} has no permission-mode entry", path.display())
            }
        }
    }
}

struct BridgeSession<S: InboxConnector, F: Files> {
    notifier: Mutex<SessionNotifier<S>>,
    followed_sockets: Mutex<BTreeSet<PathBuf>>,
    handled_seqs: Mutex<BTreeMap<PathBuf, u64>>,
    registry_file: PathBuf,
    projects_directory: PathBuf,
    transcript_file: Mutex<Option<PathBuf>>,
    permission_class_file: PathBuf,
    attested_class: Mutex<Option<PermissionClass>>,
    dropped_frames_file: PathBuf,
    files: F,
    elapsed: Elapsed,
    record: RunRecord,
    spawn: Spawner,
    wait: Pause,
}

impl<S: InboxConnector, F: Files> BridgeSession<S, F> {
    /// Finds the transcript file of the served session through its registry entry.
    fn resolve_transcript_file(&self) -> Result<PathBuf, LiveClassError> {
        let registry_text = self
            .files
            .read_text(&self.registry_file)
            .map_err(|source| LiveClassError::RegistryUnreadable {
                path: self.registry_file.clone(),
                source,
            })?;
        let session_id = session_id_of_registry_entry(&registry_text)
            .ok_or_else(|| LiveClassError::NoSessionId(self.registry_file.clone()))?;
        let file_name = transcript_file_name(&session_id);
        self.files
            .list_directory(&self.projects_directory)
            .map_err(|source| LiveClassError::ProjectsUnreadable {
                path: self.projects_directory.clone(),
                source,
            })?
            .into_iter()
            .map(|project| project.join(&file_name))
            .find(|candidate| self.files.exists(candidate))
            .ok_or(LiveClassError::TranscriptMissing(file_name))
    }

    /// Reads the latest permission mode from the end of the transcript, keeping the class read before when that end holds none, and reads the whole file only while no class has been read, saying where the class came from.
    fn class_from_transcript(
        &self,
        transcript_file: &Path,
    ) -> Result<(PermissionClass, ClassSource), LiveClassError> {
        let unreadable = |source| LiveClassError::TranscriptUnreadable {
            path: transcript_file.to_owned(),
            source,
        };
        let tail = self
            .files
            .read_tail_text(transcript_file, TRANSCRIPT_TAIL_WINDOW_BYTES)
            .map_err(unreadable)?;
        if let Some(mode) = latest_permission_mode(&tail) {
            return Ok((
                PermissionClass::from_permission_mode(&mode),
                ClassSource::TranscriptTail,
            ));
        }
        let class_read_before = *lock(&self.attested_class);
        if let Some(class_read_before) = class_read_before {
            return Ok((class_read_before, ClassSource::KeptFromBefore));
        }
        latest_permission_mode(&self.files.read_text(transcript_file).map_err(unreadable)?)
            .map(|mode| {
                (
                    PermissionClass::from_permission_mode(&mode),
                    ClassSource::WholeTranscript,
                )
            })
            .ok_or_else(|| LiveClassError::NoModeEntry(transcript_file.to_owned()))
    }

    /// Reads the permission class of the served session and where it came from, resolving its transcript once and again only when the known one cannot be opened.
    fn live_permission_class(&self) -> Result<(PermissionClass, ClassSource), LiveClassError> {
        let known = lock(&self.transcript_file).clone();
        if let Some(transcript_file) = known {
            let class = self.class_from_transcript(&transcript_file);
            if !matches!(class, Err(LiveClassError::TranscriptUnreadable { .. })) {
                return class;
            }
        }
        let transcript_file = self.resolve_transcript_file()?;
        *lock(&self.transcript_file) = Some(transcript_file.clone());
        self.class_from_transcript(&transcript_file)
    }

    /// Reads, records and reports the live permission class without holding the notifier, reporting why it cannot be read.
    fn read_live_permission_class(&self) -> Option<PermissionClass> {
        match self.live_permission_class() {
            Ok((class, source)) => {
                (self.record)(BridgeEvent::PermissionClassRead { class, source });
                self.record_permission_class(class);
                Some(class)
            }
            Err(error) => {
                (self.record)(BridgeEvent::PermissionClassUnreadable(error));
                None
            }
        }
    }

    /// Reports and records the attested class where the doctor reads it when it differs from the one attested before.
    fn record_permission_class(&self, class: PermissionClass) {
        let mut attested = lock(&self.attested_class);
        if *attested == Some(class) {
            return;
        }
        *attested = Some(class);
        drop(attested);
        (self.record)(BridgeEvent::PermissionClassAttested(class));
        if let Err(error) = self
            .files
            .write_text_atomically(&self.permission_class_file, &class.to_string())
        {
            (self.record)(BridgeEvent::PermissionClassNotWritten(error));
        }
    }

    /// Injects one frame as a notice and records the dropped-frame count when it changed.
    fn deliver(&self, frame: &InboxFrame) {
        let live_class = self.read_live_permission_class();
        let mut notifier = lock(&self.notifier);
        if let Some(class) = live_class {
            notifier.attest(class);
        }
        let dropped_before = notifier.dropped_frames();
        let notice = notifier.notify(frame, (self.elapsed)());
        let dropped_after = notifier.dropped_frames();
        drop(notifier);
        (self.record)(BridgeEvent::of_notice(FrameIds::of(frame), notice));
        if dropped_after != dropped_before {
            self.record_dropped_frames(dropped_after);
        }
    }

    /// Gives the seq of the last frame handled from a socket, 0 when none was.
    fn last_handled_seq(&self, socket_path: &Path) -> u64 {
        lock(&self.handled_seqs)
            .get(socket_path)
            .copied()
            .unwrap_or_default()
    }

    /// Records a frame's arrival from a socket, delivers it, and remembers its seq as the point to follow that socket from again.
    fn deliver_from_socket(&self, socket: &BinarySocket, frame: &InboxFrame) {
        (self.record)(BridgeEvent::FrameReceived {
            binary_pid: socket.pid(),
            frame: FrameIds::of(frame),
        });
        self.deliver(frame);
        lock(&self.handled_seqs).insert(socket.path().to_owned(), frame.seq);
    }

    /// Forgets the handled seqs of every socket whose process no longer runs, so a socket path that returns is followed from the start.
    fn forget_handled_seqs_except(&self, running_sockets: &[BinarySocket]) {
        lock(&self.handled_seqs).retain(|socket_path, _| {
            running_sockets
                .iter()
                .any(|running| running.path() == socket_path)
        });
    }

    /// Writes the dropped-frame count where the doctor reads it, reporting a failed write.
    fn record_dropped_frames(&self, dropped: DroppedFrames) {
        if let Err(error) = self
            .files
            .write_text_atomically(&self.dropped_frames_file, &dropped.as_state_file_text())
        {
            (self.record)(BridgeEvent::DroppedFramesNotWritten(error));
        }
    }

    /// Waits out the pause before the next poll, writing the alive line when it falls due inside the pause, and gives the reading the next one falls due at.
    fn wait_between_polls(&self, pause: Duration, alive_line_due: Duration) -> Duration {
        let pause_ends = (self.elapsed)().saturating_add(pause);
        let mut next_alive_line_due = alive_line_due;
        if next_alive_line_due <= pause_ends {
            (self.wait)(next_alive_line_due.saturating_sub((self.elapsed)()));
            (self.record)(BridgeEvent::Alive);
            next_alive_line_due = alive_line_due_after((self.elapsed)());
        }
        (self.wait)(pause_ends.saturating_sub((self.elapsed)()));
        next_alive_line_due
    }
}

/// Gives the reading of the bridge's clock at which the alive line falls due: one period after the reading handed in.
const fn alive_line_due_after(reading: Duration) -> Duration {
    reading.saturating_add(ALIVE_LINE_PERIOD)
}

/// Starts a task that follows one binary inbox socket until its connection fails.
fn follow_binary_socket<S, B, F>(
    session: &Arc<BridgeSession<S, F>>,
    socket: BinarySocket,
    mut connector: B,
) where
    S: InboxConnector + Send + 'static,
    B: InboxConnector + Send + 'static,
    F: Files + Send + Sync + 'static,
{
    let session = Arc::clone(session);
    let spawn = Arc::clone(&session.spawn);
    spawn(Box::new(move || {
        let wait = Arc::clone(&session.wait);
        let since = session.last_handled_seq(socket.path());
        let binary_pid = socket.pid();
        let outcome = follow_inbox(
            &mut connector,
            since,
            &mut |pause| wait(pause),
            &mut |connection| {
                (session.record)(match connection {
                    InboxConnection::Opened => BridgeEvent::InboxConnected { binary_pid },
                    InboxConnection::Closed => BridgeEvent::InboxClosed { binary_pid },
                });
            },
            &mut |frame| {
                session.deliver_from_socket(&socket, frame);
            },
        );
        let Err(error) = outcome;
        (session.record)(BridgeEvent::InboxLost {
            socket: socket.clone(),
            error,
        });
        lock(&session.followed_sockets).remove(socket.path());
    }));
}

#[derive(Debug, Eq, PartialEq)]
enum BridgeOutcome {
    HarnessExited,
    AlreadyServed { bridge_pid: Pid },
}

struct BridgeEffects<S, C, F> {
    own_pid: Pid,
    runtime_inputs: RuntimeDirectoryInputs,
    session_connector: S,
    session_token: String,
    claude_config_directory: PathBuf,
    connect_binary_socket: C,
    files: F,
    elapsed: Elapsed,
    record: RunRecord,
    spawn: Spawner,
    wait: Pause,
}

type ProductionBridgeEffects<F> =
    BridgeEffects<UnixSocketConnector, fn(PathBuf) -> UnixSocketConnector, F>;

/// Builds the production sockets, clock, spawner and pauses the bridge runs on from the environment, beside the run record handed in.
fn bridge_effects<F: Files>(
    environment: &Environment,
    files: F,
    record: RunRecord,
) -> Result<ProductionBridgeEffects<F>, CommandError> {
    let socket = environment
        .messaging_socket
        .clone()
        .ok_or(CommandError::MissingVariable(MESSAGING_SOCKET_VARIABLE))?;
    let session_token = environment
        .messaging_token
        .clone()
        .ok_or(CommandError::MissingVariable(MESSAGING_TOKEN_VARIABLE))?;
    create_config_directory(environment, &files)?;
    let started = environment.started;
    Ok(BridgeEffects {
        own_pid: environment.own_pid,
        runtime_inputs: runtime_inputs(environment, &files),
        session_connector: UnixSocketConnector::new(socket),
        session_token,
        claude_config_directory: claude_config_directory(environment)
            .ok_or(CommandError::MissingVariable(HOME_VARIABLE))?,
        connect_binary_socket: UnixSocketConnector::new,
        files,
        elapsed: Arc::new(move || started.elapsed()),
        record,
        spawn: Arc::new(|task| {
            thread::spawn(task);
        }),
        wait: Arc::new(thread::sleep),
    })
}

/// Serves the harness process by following every binary socket under it until the harness exits, unless another bridge already serves it.
fn run_bridge<S, B, F>(
    harness_pid: Pid,
    effects: BridgeEffects<S, impl Fn(PathBuf) -> B, F>,
    processes: &impl ProcessTable,
) -> Result<BridgeOutcome, CommandError>
where
    S: InboxConnector + Send + 'static,
    B: InboxConnector + Send + 'static,
    F: Files + Send + Sync + 'static,
{
    let mut alive_line_due = alive_line_due_after((effects.elapsed)());
    let entries = processes.list().map_err(CommandError::ProcessTable)?;
    if let Some(bridge_pid) = running_bridge_for(harness_pid, effects.own_pid, &entries) {
        return Ok(BridgeOutcome::AlreadyServed { bridge_pid });
    }
    require_usable_runtime_directory(&effects.files, &effects.runtime_inputs)?;
    let session = Arc::new(BridgeSession {
        notifier: Mutex::new(SessionNotifier::new(
            effects.session_connector,
            effects.session_token,
            CLASS_BEFORE_THE_LIVE_MODE_IS_READ,
        )),
        followed_sockets: Mutex::default(),
        handled_seqs: Mutex::default(),
        registry_file: effects
            .claude_config_directory
            .join(SESSIONS_DIRECTORY)
            .join(format!("{harness_pid}.{REGISTRY_ENTRY_EXTENSION}")),
        projects_directory: effects.claude_config_directory.join(PROJECTS_DIRECTORY),
        transcript_file: Mutex::default(),
        permission_class_file: permission_class_path(
            &effects.runtime_inputs.temporary_dir,
            harness_pid,
        ),
        attested_class: Mutex::default(),
        dropped_frames_file: dropped_frames_path(
            &effects.runtime_inputs.temporary_dir,
            harness_pid,
        ),
        files: effects.files,
        elapsed: effects.elapsed,
        record: effects.record,
        spawn: effects.spawn,
        wait: effects.wait,
    });
    if let Some(class) = session.read_live_permission_class() {
        lock(&session.notifier).attest(class);
    }
    session.record_dropped_frames(DroppedFrames::default());
    follow_binary_sockets(
        harness_pid,
        &effects.runtime_inputs,
        processes,
        &mut |pause| alive_line_due = session.wait_between_polls(pause, alive_line_due),
        &mut |sockets| {
            session.forget_handled_seqs_except(sockets);
            for socket in sockets {
                if lock(&session.followed_sockets).contains(socket.path()) {
                    continue;
                }
                if let Err(refusal) =
                    require_usable_runtime_directory(&session.files, &effects.runtime_inputs)
                {
                    (session.record)(BridgeEvent::CommandFailed(refusal));
                    break;
                }
                lock(&session.followed_sockets).insert(socket.path().to_owned());
                let connector = (effects.connect_binary_socket)(socket.path().to_owned());
                follow_binary_socket(&session, socket.clone(), connector);
            }
        },
    )
    .map_err(CommandError::ProcessTable)?;
    Ok(BridgeOutcome::HarnessExited)
}

/// Reads one settings file, treating a missing file as absent.
fn read_settings_file(
    files: &impl Files,
    path: PathBuf,
) -> Result<Option<SettingsFile>, CommandError> {
    match files.read_text(&path) {
        Ok(text) => Ok(Some(SettingsFile { path, text })),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CommandError::SettingsUnreadable { path, source }),
    }
}

/// Names the directory Claude Code keeps its settings, session registry and transcripts in.
fn claude_config_directory(environment: &Environment) -> Option<PathBuf> {
    environment.claude_config_dir.clone().or_else(|| {
        environment
            .home
            .as_ref()
            .map(|home| home.join(CLAUDE_DIRECTORY))
    })
}

/// Lists the user, project and local settings files in the order Claude Code reads them.
fn settings_file_paths(environment: &Environment, project_dir: &Path) -> Vec<PathBuf> {
    let user_config_dir = claude_config_directory(environment);
    let project_claude_dir = project_dir.join(CLAUDE_DIRECTORY);
    user_config_dir
        .into_iter()
        .map(|directory| directory.join(SETTINGS_FILE))
        .chain([
            project_claude_dir.join(SETTINGS_FILE),
            project_claude_dir.join(LOCAL_SETTINGS_FILE),
        ])
        .collect()
}

#[derive(Default)]
struct ManagedSettings {
    files: Vec<SettingsFile>,
    unreadable: Vec<PathBuf>,
}

/// Tells whether a path names a non-hidden JSON file.
fn is_visible_settings_file(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some(SETTINGS_EXTENSION)
        && !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| name.starts_with(HIDDEN_FILE_PREFIX))
}

/// Lists the managed settings file and the visible JSON files of its drop-in directory in name order, noting an unreadable drop-in directory.
fn managed_settings_paths(
    files: &impl Files,
    directory: &Path,
    unreadable: &mut Vec<PathBuf>,
) -> Vec<PathBuf> {
    let drop_in_directory = directory.join(MANAGED_DROP_IN_DIRECTORY);
    let mut paths = vec![directory.join(MANAGED_SETTINGS_FILE)];
    match files.list_directory(&drop_in_directory) {
        Ok(listing) => {
            paths.extend(
                listing
                    .into_iter()
                    .filter(|path| is_visible_settings_file(path)),
            );
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(_) => unreadable.push(drop_in_directory),
    }
    paths.sort();
    paths
}

/// Reads every managed settings file, separating the unreadable ones.
fn read_managed_settings(files: &impl Files, directory: &Path) -> ManagedSettings {
    let mut managed = ManagedSettings::default();
    for path in managed_settings_paths(files, directory, &mut managed.unreadable) {
        match files.read_text(&path) {
            Ok(text) => managed.files.push(SettingsFile { path, text }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(_) => managed.unreadable.push(path),
        }
    }
    managed
}

/// Tells whether the session messaging socket is unset, missing or present.
fn messaging_socket_state(environment: &Environment, files: &impl Files) -> MessagingSocket {
    match &environment.messaging_socket {
        None => MessagingSocket::Unset,
        Some(path) if files.exists(path) => MessagingSocket::Present(path.clone()),
        Some(path) => MessagingSocket::Missing(path.clone()),
    }
}

/// Gathers the doctor inputs, writes the report to the output and tells whether every check passed.
fn run_doctor_command(
    environment: &Environment,
    files: &impl Files,
    processes: &impl ProcessTable,
    output: &mut impl Write,
) -> Result<bool, CommandError> {
    let mut settings_files: Vec<SettingsFile> =
        settings_file_paths(environment, &environment.working_dir)
            .into_iter()
            .map(|path| read_settings_file(files, path))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
    let managed = read_managed_settings(files, Path::new(MANAGED_SETTINGS_DIRECTORY));
    settings_files.extend(managed.files);
    let inputs = DoctorInputs {
        own_pid: environment.own_pid,
        messaging_socket: messaging_socket_state(environment, files),
        settings_files,
        unreadable_settings: managed.unreadable,
        runtime_inputs: runtime_inputs(environment, files),
    };
    let report = run_doctor(
        &inputs,
        processes,
        &|path: &Path| files.read_text(path).ok(),
        &|path: &Path| files.read_state_text(path).ok(),
        &|path: &Path| files.entry_facts(path),
    )
    .map_err(CommandError::ProcessTable)?;
    writeln!(output, "{report}").map_err(CommandError::ReportUnwritable)?;
    Ok(report.all_passed())
}

/// Maps a command outcome to its exit status, recording a failure's message alone.
fn exit_code_of(outcome: Result<bool, CommandError>, record: &RunRecord) -> ExitCode {
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(EXIT_CHECK_FAILED),
        Err(error) => {
            record(BridgeEvent::CommandFailed(error));
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// The run record written to the error output and, when there is one, into the debug trace, a failed write to the error output dropped because nothing remains to report it to.
fn run_record_on<Errors: Write + Send + 'static>(
    errors: Arc<Mutex<Errors>>,
    trace: Option<Arc<DebugTrace>>,
) -> RunRecord {
    Arc::new(move |event| {
        if let Some(line) = event.run_record_line() {
            drop(writeln!(lock(&errors), "{line}"));
        }
        if let Some(trace) = &trace {
            trace.record(&event);
        }
    })
}

/// Serves the harness process under the tracing asked for, recording every event on the error output and, under `--debug`, in the events file it names first, and gives the exit status.
fn run_command<S, B, C, F>(
    harness_pid: Pid,
    tracing: Tracing,
    trace_inputs: &TraceInputs<'_>,
    errors: Arc<Mutex<impl Write + Send + 'static>>,
    bridge_effects: impl FnOnce(RunRecord) -> Result<BridgeEffects<S, C, F>, CommandError>,
    processes: &impl ProcessTable,
) -> ExitCode
where
    S: InboxConnector + Send + 'static,
    B: InboxConnector + Send + 'static,
    C: Fn(PathBuf) -> B,
    F: Files + Send + Sync + 'static,
{
    let trace = match DebugTrace::start(tracing, trace_inputs) {
        Ok(trace) => trace.map(Arc::new),
        Err(error) => return exit_code_of(Err(error), &run_record_on(errors, None)),
    };
    if let Some(trace) = &trace {
        drop(writeln!(lock(&errors), "{}", trace.announcement()));
    }
    let record = run_record_on(Arc::clone(&errors), trace.clone());
    record(BridgeEvent::Started { harness_pid });
    let outcome = bridge_effects(Arc::clone(&record))
        .and_then(|effects| run_bridge(harness_pid, effects, processes))
        .map(|outcome| {
            if let BridgeOutcome::AlreadyServed { bridge_pid } = outcome {
                record(BridgeEvent::AlreadyServed {
                    bridge_pid,
                    harness_pid,
                });
            }
            true
        });
    let status = exit_code_of(outcome, &record);
    if let Some(unfinished) = trace.and_then(|trace| trace.finish()) {
        run_record_on(errors, None)(unfinished);
    }
    status
}

/// Draws a run id's bytes from the operating system's secure random source.
fn secure_random_run_id() -> io::Result<[u8; RUN_ID_BYTES]> {
    let mut run_id = [0; RUN_ID_BYTES];
    fs::File::open(SECURE_RANDOM_DEVICE)?.read_exact(&mut run_id)?;
    Ok(run_id)
}

/// The trace's clocks, both read on each line: milliseconds since `started`, and milliseconds since the Unix epoch, a reading before it taken as the epoch.
fn trace_clocks_since(started: Instant) -> TraceClocks {
    TraceClocks {
        monotonic_ms: Arc::new(move || {
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
        }),
        wall_ms: Arc::new(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                })
        }),
    }
}

/// Runs the subcommand named on the command line and exits with its status.
fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let errors = Arc::new(Mutex::new(std::io::stderr()));
    let record = run_record_on(Arc::clone(&errors), None);
    let environment = match read_environment() {
        Ok(environment) => environment,
        Err(error) => return exit_code_of(Err(error), &record),
    };
    match parse_command(&arguments) {
        Some(Command::Run {
            harness_pid,
            tracing,
        }) => match OperatingSystemFiles::of_this_process() {
            Err(error) => exit_code_of(Err(error), &record),
            Ok(files) => run_command(
                harness_pid,
                tracing,
                &TraceInputs {
                    environment: &non_empty_variable,
                    draw_run_id: &secure_random_run_id,
                    create_private_directory: &|path| files.create_private_directory(path),
                    effects: TraceEffects::of_the_operating_system(trace_clocks_since(
                        environment.started,
                    )),
                    bounds: TraceBounds::SPEC,
                },
                errors,
                |record| bridge_effects(&environment, files, record),
                &PsProcessTable,
            ),
        },
        Some(Command::RunHelp) => {
            let outcome = writeln!(std::io::stdout(), "{}", run_help())
                .map(|()| true)
                .map_err(CommandError::HelpUnwritable);
            exit_code_of(outcome, &record)
        }
        Some(Command::Doctor) => {
            let outcome = OperatingSystemFiles::of_this_process().and_then(|files| {
                run_doctor_command(
                    &environment,
                    &files,
                    &PsProcessTable,
                    &mut std::io::stdout(),
                )
            });
            exit_code_of(outcome, &record)
        }
        None => {
            record(BridgeEvent::UsageRefused);
            ExitCode::from(EXIT_USAGE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use estafette_bridge_claude::discover::{ProcessEntry, binary_sockets_under};
    use estafette_bridge_claude::owned_entry::EntryKind;
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixStream;

    fn silent_record() -> RunRecord {
        Arc::new(|_event| {})
    }

    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn spec_3_12_the_hook_command_line_starts_the_bridge_for_its_parent_pid() {
        assert_eq!(
            parse_command(&arguments(&["run", "4242"])),
            Some(Command::Run {
                harness_pid: Pid(4242),
                tracing: Tracing::Debug,
            })
        );
    }

    #[test]
    fn observability_decision_1_run_takes_the_debug_flag_pair_after_the_pid_and_writes_the_trace_unless_told_not_to()
     {
        for (words, tracing) in [
            (&["run", "4242"][..], Tracing::Debug),
            (&["run", "4242", "--debug"], Tracing::Debug),
            (&["run", "4242", "--no-debug"], Tracing::NoDebug),
        ] {
            assert_eq!(
                parse_command(&arguments(words)),
                Some(Command::Run {
                    harness_pid: Pid(4242),
                    tracing,
                }),
                "{words:?}"
            );
        }
    }

    #[test]
    fn spec_3_12_doctor_takes_no_arguments() {
        assert_eq!(
            parse_command(&arguments(&["doctor"])),
            Some(Command::Doctor)
        );
    }

    #[test]
    fn observability_decision_3_the_help_of_run_states_what_the_trace_writes_where_that_it_is_safe_to_post_and_its_bounds()
     {
        assert_eq!(
            parse_command(&arguments(&["run", "--help"])),
            Some(Command::RunHelp)
        );
        let help = run_help();
        for stated in [
            "usage: estafette-bridge-claude run <harness-pid> [--debug | --no-debug]",
            "--debug ",
            "--no-debug ",
            &estafette_trace::TraceDirectory::described(),
            "<start>-<run>-bridge.events.jsonl",
            "safe to post",
            "64 MiB",
            ".1 file",
            "128 MiB",
            "1 GiB",
            "10 minutes",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
        ] {
            assert!(help.contains(stated), "{stated:?} missing from:\n{help}");
        }
    }

    #[test]
    fn spec_3_12_anything_else_is_refused_with_the_usage() {
        for refused in [
            &[][..],
            &["run"],
            &["run", "not-a-pid"],
            &["run", "-5"],
            &["run", "--no-debug", "4242"],
            &["run", "4242", "--debug", "--no-debug"],
            &["run", "4242", "--no-debug", "--no-debug"],
            &["run", "4242", "--verbose"],
            &["doctor", "--no-debug"],
            &["doctor", "--project-dir", "/work"],
            &["doctor", "extra"],
            &["serve"],
        ] {
            assert_eq!(parse_command(&arguments(refused)), None, "{refused:?}");
        }
    }

    struct FixedProcessTable(Vec<ProcessEntry>);

    impl ProcessTable for FixedProcessTable {
        fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
            Ok(self.0.clone())
        }
    }

    fn bare_environment() -> Environment {
        Environment {
            messaging_socket: None,
            messaging_token: None,
            home: None,
            xdg_config_home: None,
            claude_config_dir: None,
            xdg_runtime_dir: None,
            temporary_dir: PathBuf::from("/tmp"),
            own_pid: Pid(900),
            working_dir: PathBuf::from("/work"),
            started: Instant::now(),
        }
    }

    fn bridge_process(pid: u32, harness_pid: &str) -> ProcessEntry {
        ProcessEntry {
            pid: Pid(pid),
            parent_pid: Pid(1),
            executable: Some("/opt/bin/estafette-bridge-claude".to_owned()),
            arguments: vec!["run".to_owned(), harness_pid.to_owned()],
        }
    }

    struct DownConnector;

    impl InboxConnector for DownConnector {
        type Stream = UnixStream;

        fn connect(&mut self) -> io::Result<UnixStream> {
            Err(io::Error::other("down"))
        }
    }

    type SpawnedTasks = Arc<Mutex<Vec<Task>>>;
    type ErrorMessages = Arc<Mutex<Vec<String>>>;

    struct Wiring {
        effects: BridgeEffects<DownConnector, Box<dyn Fn(PathBuf) -> DownConnector>, StandInFiles>,
        factory_paths: Arc<Mutex<Vec<PathBuf>>>,
        spawned: SpawnedTasks,
        errors: ErrorMessages,
        clock_seconds: Arc<Mutex<u64>>,
        writes: Arc<Mutex<Vec<(PathBuf, String)>>>,
    }

    const TEST_TEMPORARY_DIR: &str = "/var/tmp";

    fn wiring() -> Wiring {
        let factory_paths = Arc::new(Mutex::new(Vec::new()));
        let spawned: SpawnedTasks = Arc::default();
        let errors: ErrorMessages = Arc::default();
        let clock_seconds = Arc::new(Mutex::new(0));
        let recorded_paths = Arc::clone(&factory_paths);
        let recorded_tasks = Arc::clone(&spawned);
        let recorded_errors = Arc::clone(&errors);
        let clock = Arc::clone(&clock_seconds);
        let files = StandInFiles::default();
        let writes = Arc::clone(&files.writes);
        Wiring {
            effects: BridgeEffects {
                own_pid: Pid(900),
                runtime_inputs: RuntimeDirectoryInputs {
                    xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
                    temporary_dir: PathBuf::from(TEST_TEMPORARY_DIR),
                    user_id: 1000,
                },
                session_connector: DownConnector,
                session_token: "token".to_owned(),
                claude_config_directory: PathBuf::from(CLAUDE_CONFIG_DIRECTORY),
                connect_binary_socket: Box::new(move |socket_path| {
                    lock(&recorded_paths).push(socket_path);
                    DownConnector
                }),
                files,
                elapsed: Arc::new(move || Duration::from_secs(*lock(&clock))),
                record: Arc::new(move |event| {
                    lock(&recorded_errors).extend(event.run_record_line());
                }),
                spawn: Arc::new(move |task| {
                    lock(&recorded_tasks).push(task);
                }),
                wait: Arc::new(|_| {}),
            },
            factory_paths,
            spawned,
            errors,
            clock_seconds,
            writes,
        }
    }

    const DROPPED_FRAMES_FILE: &str = "/var/tmp/estafette-bridge-claude-100.dropped-frames";

    fn written_counts(writes: &Mutex<Vec<(PathBuf, String)>>) -> Vec<String> {
        lock(writes)
            .iter()
            .filter(|(path, _)| path == Path::new(DROPPED_FRAMES_FILE))
            .map(|(_, text)| text.clone())
            .collect()
    }

    fn rate_limit_drops_counted(writes: &Mutex<Vec<(PathBuf, String)>>) -> u64 {
        written_counts(writes)
            .last()
            .and_then(|text| DroppedFrames::from_state_file_text(text))
            .map_or(0, |dropped| dropped.rate_limit)
    }

    #[test]
    fn spec_3_12_a_second_start_for_the_same_harness_leaves_the_running_bridge_alone() {
        let running = FixedProcessTable(vec![
            bridge_process(900, &HARNESS_PID.to_string()),
            bridge_process(700, &HARNESS_PID.to_string()),
            bridge_process(701, "999"),
        ]);
        let wiring = wiring();
        assert_eq!(
            run_bridge(HARNESS_PID, wiring.effects, &running).expect("guarded start"),
            BridgeOutcome::AlreadyServed {
                bridge_pid: Pid(700)
            }
        );
        assert_eq!(lock(&wiring.spawned).len(), 0);
    }

    #[test]
    fn observability_decision_1_a_bridge_started_with_either_debug_flag_still_guards_its_harness_against_a_second_start()
     {
        for flag in ["--debug", "--no-debug"] {
            let mut flagged_bridge = bridge_process(700, &HARNESS_PID.to_string());
            flagged_bridge.arguments.push(flag.to_owned());
            let running = FixedProcessTable(vec![bridge_process(900, "100"), flagged_bridge]);
            let wiring = wiring();
            assert_eq!(
                run_bridge(HARNESS_PID, wiring.effects, &running).expect("guarded start"),
                BridgeOutcome::AlreadyServed {
                    bridge_pid: Pid(700)
                },
                "{flag}"
            );
        }
    }

    #[test]
    fn observability_decision_5_each_run_id_is_drawn_afresh_from_the_secure_random_source() {
        let first = secure_random_run_id().expect("first draw");
        let second = secure_random_run_id().expect("second draw");
        assert_ne!(first, second);
        assert_ne!(first, [0; RUN_ID_BYTES]);
    }

    const USER_THE_PROCESS_RUNS_AS: u32 = 4242;

    fn runtime_user_id_for(environment: Environment) -> Result<u32, String> {
        let environment = Environment {
            messaging_socket: Some(PathBuf::from("/tmp/session.sock")),
            messaging_token: Some("token".to_owned()),
            claude_config_dir: Some(PathBuf::from(CLAUDE_CONFIG_DIRECTORY)),
            ..environment
        };
        let files = StandInFiles {
            user: Some(USER_THE_PROCESS_RUNS_AS),
            ..StandInFiles::default()
        };
        bridge_effects(&environment, files, silent_record())
            .map(|effects| effects.runtime_inputs.user_id)
            .map_err(|error| error.to_string())
    }

    #[test]
    fn spec_3_3_the_runtime_directory_user_id_is_the_user_the_process_runs_as_whatever_directory_core_resolves()
     {
        let with_config_base = Environment {
            xdg_config_home: Some(PathBuf::from("/cfg")),
            home: Some(PathBuf::from("/home/ana")),
            ..bare_environment()
        };
        let under_home = Environment {
            home: Some(PathBuf::from("/home/ana")),
            ..bare_environment()
        };
        for environment in [with_config_base, under_home] {
            assert_eq!(
                runtime_user_id_for(environment),
                Ok(USER_THE_PROCESS_RUNS_AS)
            );
        }
    }

    #[test]
    fn spec_3_3_without_xdg_config_home_and_home_the_bridge_refuses_with_the_message_core_gives() {
        assert_eq!(
            runtime_user_id_for(bare_environment()),
            Err("XDG_CONFIG_HOME and HOME are both unset or empty: set one of them".to_owned())
        );
    }

    const CORE_PRIVATE_DIRECTORY_MODE: u32 = 0o700;
    const HARNESS_PID: Pid = Pid(100);
    const MOST_FRAMES_BEFORE_A_DROP: usize = 100;
    const STAGED_WRITES: usize = 50;

    #[test]
    fn spec_3_3_a_bridge_starting_before_core_creates_the_missing_config_directory_as_core_does() {
        let scratch = scratch_directory("config-creation");
        let config_directory = scratch.join("missing-base").join(PRODUCT_DIRECTORY);
        let environment = Environment {
            messaging_socket: Some(PathBuf::from("/tmp/session.sock")),
            messaging_token: Some("token".to_owned()),
            claude_config_dir: Some(PathBuf::from(CLAUDE_CONFIG_DIRECTORY)),
            xdg_config_home: Some(scratch.join("missing-base")),
            ..bare_environment()
        };
        let effects = bridge_effects(
            &environment,
            OperatingSystemFiles::of_this_process().unwrap(),
            silent_record(),
        )
        .unwrap_or_else(|error| panic!("the bridge refused to start: {error}"));
        let metadata = fs::metadata(&config_directory).expect("config directory created");
        assert_eq!(
            metadata.permissions().mode() & 0o777,
            CORE_PRIVATE_DIRECTORY_MODE
        );
        assert_eq!(effects.runtime_inputs.user_id, metadata.uid());
        crate::remove_scratch::remove_scratch(&scratch).expect("cleanup");
    }

    #[test]
    fn spec_3_3_a_config_directory_the_bridge_cannot_create_is_refused_with_the_message_core_gives()
    {
        let environment = Environment {
            messaging_socket: Some(PathBuf::from("/tmp/session.sock")),
            messaging_token: Some("token".to_owned()),
            claude_config_dir: Some(PathBuf::from(CLAUDE_CONFIG_DIRECTORY)),
            xdg_config_home: Some(PathBuf::from("/cfg")),
            ..bare_environment()
        };
        let mut files = StandInFiles::default();
        files.creation_failures.insert(
            PathBuf::from("/cfg/estafette"),
            io::ErrorKind::PermissionDenied,
        );
        assert_eq!(
            bridge_effects(&environment, files, silent_record())
                .err()
                .map(|error| error.to_string()),
            Some("cannot create /cfg/estafette: permission denied".to_owned())
        );
    }

    #[test]
    fn spec_3_12_a_bridge_with_the_socket_but_no_token_names_the_token_variable() {
        let environment = Environment {
            messaging_socket: Some(PathBuf::from("/tmp/session.sock")),
            ..bare_environment()
        };
        assert!(matches!(
            bridge_effects(&environment, StandInFiles::default(), silent_record()),
            Err(CommandError::MissingVariable(MESSAGING_TOKEN_VARIABLE))
        ));
    }

    #[test]
    fn spec_3_12_the_bridge_needs_the_session_socket_and_token_before_it_starts() {
        assert!(matches!(
            bridge_effects(
                &bare_environment(),
                OperatingSystemFiles::of_this_process().unwrap(),
                silent_record()
            ),
            Err(CommandError::MissingVariable(MESSAGING_SOCKET_VARIABLE))
        ));
    }

    struct SnapshotSequence(RefCell<Vec<Vec<ProcessEntry>>>);

    impl ProcessTable for SnapshotSequence {
        fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
            let mut remaining = self.0.borrow_mut();
            Ok(if remaining.len() > 1 {
                remaining.remove(0)
            } else {
                remaining.first().cloned().unwrap_or_default()
            })
        }
    }

    fn mcp_binary(pid: u32) -> ProcessEntry {
        ProcessEntry {
            pid: Pid(pid),
            parent_pid: HARNESS_PID,
            executable: Some("/usr/bin/estafette".to_owned()),
            arguments: vec!["mcp".to_owned()],
        }
    }

    #[test]
    fn spec_3_12_the_bridge_follows_each_binary_socket_once_however_many_polls_see_it() {
        let harness = ProcessEntry {
            pid: HARNESS_PID,
            parent_pid: Pid(1),
            executable: Some("/usr/bin/claude".to_owned()),
            arguments: Vec::new(),
        };
        let with_two_binaries = vec![harness, mcp_binary(101), mcp_binary(102)];
        let polls = SnapshotSequence(RefCell::new(vec![
            with_two_binaries.clone(),
            with_two_binaries.clone(),
            with_two_binaries,
            Vec::new(),
        ]));
        let wiring = wiring();
        assert_eq!(
            run_bridge(HARNESS_PID, wiring.effects, &polls).expect("bridge runs"),
            BridgeOutcome::HarnessExited
        );
        assert_eq!(
            *lock(&wiring.factory_paths),
            [
                PathBuf::from("/run/user/1000/estafette-1000/101.sock"),
                PathBuf::from("/run/user/1000/estafette-1000/102.sock"),
            ]
        );
        assert_eq!(lock(&wiring.spawned).len(), 2);
        assert_eq!(written_counts(&wiring.writes), ["0 0 0"]);
    }

    #[test]
    fn spec_3_12_a_binary_socket_that_cannot_be_reached_is_reported_and_followed_again_later() {
        let harness = ProcessEntry {
            pid: HARNESS_PID,
            parent_pid: Pid(1),
            executable: Some("/usr/bin/claude".to_owned()),
            arguments: Vec::new(),
        };
        let running = vec![harness, mcp_binary(101)];
        let polls = SnapshotSequence(RefCell::new(vec![running.clone(), running, Vec::new()]));
        let wiring = wiring();
        run_bridge(HARNESS_PID, wiring.effects, &polls).expect("bridge runs");
        let task = lock(&wiring.spawned).pop().expect("one follower spawned");
        task();
        assert_eq!(
            lock(&wiring.errors).last().map(String::as_str),
            Some(
                "/run/user/1000/estafette-1000/101.sock: cannot connect to the inbox socket: down"
            )
        );
    }

    type SinceLines = Arc<Mutex<Vec<String>>>;

    const RUNTIME_DIRECTORY_OF_WIRING: &str = "/run/user/1000/estafette-1000";
    const GROUP_OTHER_READABLE_DIRECTORY_MODE: u32 = 0o755;
    const PRIVATE_RUNTIME_DIRECTORY_MODE: u32 = 0o700;
    const USER_OF_WIRING: u32 = 1000;

    fn directory_facts(permission_bits: u32) -> EntryFacts {
        EntryFacts {
            kind: EntryKind::Directory,
            owner_user_id: USER_OF_WIRING,
            permission_bits,
        }
    }

    fn harness_entry() -> ProcessEntry {
        ProcessEntry {
            pid: HARNESS_PID,
            parent_pid: Pid(1),
            executable: Some("/usr/bin/claude".to_owned()),
            arguments: Vec::new(),
        }
    }

    const REFUSED_RUNTIME_DIRECTORY_MESSAGE: &str = "/run/user/1000/estafette-1000 already exists and is mode 0755, which grants access to group or other; it must be a directory of yours with no access for group or other";

    #[test]
    fn spec_3_3_the_bridge_refuses_at_its_start_a_runtime_directory_granting_access_to_group_and_other_runtime_directory_is_private_or_refused()
     {
        let wiring = wiring();
        lock(&wiring.effects.files.entries).insert(
            PathBuf::from(RUNTIME_DIRECTORY_OF_WIRING),
            directory_facts(GROUP_OTHER_READABLE_DIRECTORY_MODE),
        );
        let polls = SnapshotSequence(RefCell::new(vec![
            vec![harness_entry(), mcp_binary(101)],
            Vec::new(),
        ]));

        let outcome = run_bridge(HARNESS_PID, wiring.effects, &polls);

        assert_eq!(
            outcome.err().map(|error| error.to_string()),
            Some(REFUSED_RUNTIME_DIRECTORY_MESSAGE.to_owned())
        );
        assert_eq!(*lock(&wiring.factory_paths), Vec::<PathBuf>::new());
        assert_eq!(lock(&wiring.spawned).len(), 0);
    }

    const LISTING_OF_THE_SECOND_POLL: usize = 3;

    #[test]
    fn spec_3_3_the_bridge_refuses_at_its_start_a_runtime_directory_it_cannot_inspect_runtime_directory_is_private_or_refused()
     {
        let wiring = wiring();
        lock(&wiring.effects.files.entry_failures).insert(
            PathBuf::from(RUNTIME_DIRECTORY_OF_WIRING),
            io::ErrorKind::PermissionDenied,
        );
        let polls = SnapshotSequence(RefCell::new(vec![
            vec![harness_entry(), mcp_binary(101)],
            Vec::new(),
        ]));

        let outcome = run_bridge(HARNESS_PID, wiring.effects, &polls);

        assert_eq!(
            outcome.err().map(|error| error.to_string()),
            Some(format!(
                "cannot inspect {RUNTIME_DIRECTORY_OF_WIRING}: permission denied"
            ))
        );
        assert_eq!(lock(&wiring.spawned).len(), 0);
    }

    struct DirectoryLoosenedAtItsSecondPoll {
        entries: Arc<Mutex<BTreeMap<PathBuf, EntryFacts>>>,
        polls: SnapshotSequence,
        listings: RefCell<usize>,
    }

    impl ProcessTable for DirectoryLoosenedAtItsSecondPoll {
        fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
            let listing_number = {
                let mut listings = self.listings.borrow_mut();
                *listings = listings.saturating_add(1);
                *listings
            };
            if listing_number == LISTING_OF_THE_SECOND_POLL {
                lock(&self.entries).insert(
                    PathBuf::from(RUNTIME_DIRECTORY_OF_WIRING),
                    directory_facts(GROUP_OTHER_READABLE_DIRECTORY_MODE),
                );
            }
            self.polls.list()
        }
    }

    #[test]
    fn spec_3_3_the_bridge_connects_to_no_socket_once_its_runtime_directory_stops_being_private_runtime_directory_is_private_or_refused()
     {
        let wiring = wiring();
        lock(&wiring.effects.files.entries).insert(
            PathBuf::from(RUNTIME_DIRECTORY_OF_WIRING),
            directory_facts(PRIVATE_RUNTIME_DIRECTORY_MODE),
        );
        let polls = DirectoryLoosenedAtItsSecondPoll {
            entries: Arc::clone(&wiring.effects.files.entries),
            polls: SnapshotSequence(RefCell::new(vec![
                vec![harness_entry()],
                vec![harness_entry()],
                vec![harness_entry(), mcp_binary(101)],
                Vec::new(),
            ])),
            listings: RefCell::new(0),
        };

        run_bridge(HARNESS_PID, wiring.effects, &polls).expect("bridge runs");

        assert_eq!(*lock(&wiring.factory_paths), Vec::<PathBuf>::new());
        assert_eq!(lock(&wiring.spawned).len(), 0);
        assert_eq!(
            lock(&wiring.errors).last().map(String::as_str),
            Some(REFUSED_RUNTIME_DIRECTORY_MESSAGE)
        );
    }

    struct ScriptedInboxStream {
        replies: io::Cursor<Vec<u8>>,
        fails_after_replies: bool,
        since_lines: SinceLines,
        written: Vec<u8>,
    }

    impl Read for ScriptedInboxStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match self.replies.read(buffer)? {
                0 if self.fails_after_replies => {
                    Err(io::Error::from(io::ErrorKind::ConnectionReset))
                }
                read_bytes => Ok(read_bytes),
            }
        }
    }

    impl Write for ScriptedInboxStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            lock(&self.since_lines).push(String::from_utf8_lossy(&self.written).into_owned());
            self.written.clear();
            Ok(())
        }
    }

    struct ScriptedInboxConnection {
        replies: String,
        fails_after_replies: bool,
    }

    struct ScriptedInboxSocket {
        connections: Arc<Mutex<VecDeque<ScriptedInboxConnection>>>,
        since_lines: SinceLines,
    }

    impl InboxConnector for ScriptedInboxSocket {
        type Stream = ScriptedInboxStream;

        fn connect(&mut self) -> io::Result<ScriptedInboxStream> {
            let connection = lock(&self.connections)
                .pop_front()
                .ok_or_else(|| io::Error::from(io::ErrorKind::ConnectionRefused))?;
            Ok(ScriptedInboxStream {
                replies: io::Cursor::new(connection.replies.into_bytes()),
                fails_after_replies: connection.fails_after_replies,
                since_lines: Arc::clone(&self.since_lines),
                written: Vec::new(),
            })
        }
    }

    fn inbox_frame_line(seq: u64) -> String {
        format!("{{\"seq\":{seq},\"id\":\"id-{seq}\",\"from\":\"ana\",\"body\":\"body {seq}\"}}\n")
    }

    struct CountingDownConnector(Arc<Mutex<usize>>);

    impl InboxConnector for CountingDownConnector {
        type Stream = UnixStream;

        fn connect(&mut self) -> io::Result<UnixStream> {
            {
                let mut attempts = lock(&self.0);
                *attempts = attempts.saturating_add(1);
            }
            Err(io::Error::other("down"))
        }
    }

    struct ScriptedRun {
        since_lines: Vec<String>,
        injection_attempts: usize,
    }

    fn run_bridge_over_scripted_sockets(
        connections: Vec<ScriptedInboxConnection>,
        polls: Vec<Vec<ProcessEntry>>,
    ) -> ScriptedRun {
        let wiring = wiring();
        let injection_attempts = Arc::new(Mutex::new(0));
        let connections = Arc::new(Mutex::new(VecDeque::from(connections)));
        let since_lines = SinceLines::default();
        let socket_connections = Arc::clone(&connections);
        let socket_since_lines = Arc::clone(&since_lines);
        let effects = BridgeEffects {
            own_pid: wiring.effects.own_pid,
            runtime_inputs: wiring.effects.runtime_inputs,
            session_connector: CountingDownConnector(Arc::clone(&injection_attempts)),
            session_token: wiring.effects.session_token,
            claude_config_directory: wiring.effects.claude_config_directory,
            connect_binary_socket: move |_socket_path| ScriptedInboxSocket {
                connections: Arc::clone(&socket_connections),
                since_lines: Arc::clone(&socket_since_lines),
            },
            files: wiring.effects.files,
            elapsed: wiring.effects.elapsed,
            record: wiring.effects.record,
            spawn: Arc::new(|task| task()),
            wait: Arc::new(|_| {}),
        };
        run_bridge(HARNESS_PID, effects, &SnapshotSequence(RefCell::new(polls)))
            .expect("bridge runs");
        ScriptedRun {
            since_lines: lock(&since_lines).clone(),
            injection_attempts: *lock(&injection_attempts),
        }
    }

    fn harness_with_one_binary() -> Vec<ProcessEntry> {
        vec![
            ProcessEntry {
                pid: HARNESS_PID,
                parent_pid: Pid(1),
                executable: Some("/usr/bin/claude".to_owned()),
                arguments: Vec::new(),
            },
            mcp_binary(101),
        ]
    }

    #[test]
    fn spec_3_12_step_5_a_socket_that_fails_mid_stream_is_followed_again_from_the_last_handled_seq()
    {
        let run = run_bridge_over_scripted_sockets(
            vec![
                ScriptedInboxConnection {
                    replies: format!("{}{}", inbox_frame_line(3), inbox_frame_line(4)),
                    fails_after_replies: true,
                },
                ScriptedInboxConnection {
                    replies: inbox_frame_line(5),
                    fails_after_replies: false,
                },
            ],
            vec![
                harness_with_one_binary(),
                harness_with_one_binary(),
                harness_with_one_binary(),
                Vec::new(),
            ],
        );
        assert_eq!(run.since_lines, ["{\"since\":0}\n", "{\"since\":4}\n"]);
        assert_eq!(run.injection_attempts, 3);
    }

    fn harness_without_binaries() -> Vec<ProcessEntry> {
        harness_with_one_binary().into_iter().take(1).collect()
    }

    #[test]
    fn spec_3_12_step_5_a_socket_of_a_process_that_ended_is_followed_from_zero_if_its_path_returns()
    {
        let run = run_bridge_over_scripted_sockets(
            vec![
                ScriptedInboxConnection {
                    replies: inbox_frame_line(3),
                    fails_after_replies: true,
                },
                ScriptedInboxConnection {
                    replies: inbox_frame_line(1),
                    fails_after_replies: false,
                },
            ],
            vec![
                harness_with_one_binary(),
                harness_with_one_binary(),
                harness_without_binaries(),
                harness_with_one_binary(),
                Vec::new(),
            ],
        );
        assert_eq!(run.since_lines, ["{\"since\":0}\n", "{\"since\":0}\n"]);
        assert_eq!(run.injection_attempts, 2);
    }

    fn frame_from(from: &str, body: &str) -> InboxFrame {
        InboxFrame {
            seq: 1,
            id: "id".to_owned(),
            from: from.to_owned(),
            body: body.to_owned(),
        }
    }

    struct DeliveryRun {
        errors: Vec<String>,
        counts: Vec<String>,
    }

    const CLAUDE_CONFIG_DIRECTORY: &str = "/home/ana/.claude";
    const REGISTRY_FILE: &str = "/home/ana/.claude/sessions/100.json";
    const PROJECTS_DIRECTORY_PATH: &str = "/home/ana/.claude/projects";
    const TRANSCRIPT_FILE: &str = "/home/ana/.claude/projects/-work/abc-1.jsonl";
    const PERMISSION_CLASS_FILE: &str = "/var/tmp/estafette-bridge-claude-100.permission-class";

    fn files_of_live_session(mode: &str) -> StandInFiles {
        let mut files = StandInFiles::default();
        files.texts.insert(
            PathBuf::from(REGISTRY_FILE),
            Ok("{\"pid\":100,\"sessionId\":\"abc-1\"}".to_owned()),
        );
        files.directories.insert(
            PathBuf::from(PROJECTS_DIRECTORY_PATH),
            Ok(vec![
                PathBuf::from("/home/ana/.claude/projects/-other"),
                PathBuf::from("/home/ana/.claude/projects/-work"),
            ]),
        );
        files.existing.insert(PathBuf::from(TRANSCRIPT_FILE));
        files.texts.insert(
            PathBuf::from(TRANSCRIPT_FILE),
            Ok(format!(
                "{{\"type\":\"permission-mode\",\"permissionMode\":\"{mode}\"}}\n"
            )),
        );
        files
    }

    fn bridge_session_over<S: InboxConnector, F: Files>(
        wiring: &Wiring,
        files: F,
        connector: S,
    ) -> BridgeSession<S, F> {
        BridgeSession {
            notifier: Mutex::new(SessionNotifier::new(
                connector,
                "token".to_owned(),
                CLASS_BEFORE_THE_LIVE_MODE_IS_READ,
            )),
            followed_sockets: Mutex::default(),
            handled_seqs: Mutex::default(),
            registry_file: PathBuf::from(REGISTRY_FILE),
            projects_directory: PathBuf::from(PROJECTS_DIRECTORY_PATH),
            transcript_file: Mutex::default(),
            permission_class_file: PathBuf::from(PERMISSION_CLASS_FILE),
            attested_class: Mutex::default(),
            dropped_frames_file: PathBuf::from(DROPPED_FRAMES_FILE),
            files,
            elapsed: Arc::clone(&wiring.effects.elapsed),
            record: Arc::clone(&wiring.effects.record),
            spawn: Arc::clone(&wiring.effects.spawn),
            wait: Arc::new(|_| {}),
        }
    }

    fn bridge_session(wiring: &Wiring) -> BridgeSession<DownConnector, StandInFiles> {
        bridge_session_over(
            wiring,
            files_of_live_session("bypassPermissions"),
            DownConnector,
        )
    }

    fn connect_errors(wiring: &Wiring) -> Vec<String> {
        lock(&wiring.errors)
            .iter()
            .filter(|message| message.starts_with("cannot connect"))
            .cloned()
            .collect()
    }

    fn deliver_eleven_frames(seconds_between_frames: u64) -> DeliveryRun {
        let wiring = wiring();
        let session = bridge_session(&wiring);
        for index in 0..11_u64 {
            *lock(&wiring.clock_seconds) = index.saturating_mul(seconds_between_frames);
            session.deliver(&frame_from("ana", &format!("body {index}")));
        }
        DeliveryRun {
            errors: connect_errors(&wiring),
            counts: written_counts(&session.files.writes),
        }
    }

    #[test]
    fn spec_3_12_the_clock_handed_in_decides_the_rate_limit_and_each_drop_is_recorded() {
        let within_one_window = deliver_eleven_frames(1);
        assert_eq!(within_one_window.errors.len(), 10);
        assert_eq!(
            within_one_window.errors.first().map(String::as_str),
            Some("cannot connect to the messaging socket: down")
        );
        assert_eq!(
            within_one_window.counts.last().map(String::as_str),
            Some("1 0 10")
        );
        let across_windows = deliver_eleven_frames(61);
        assert_eq!(across_windows.errors.len(), 11);
        assert_eq!(
            across_windows.counts.last().map(String::as_str),
            Some("0 0 11")
        );
    }

    #[test]
    fn spec_3_12_frames_of_two_followed_sockets_share_one_rate_limit_per_sender() {
        let wiring = wiring();
        let session = bridge_session(&wiring);
        let [first_socket, second_socket] = <[BinarySocket; 2]>::try_from(binary_sockets_under(
            HARNESS_PID,
            &wiring.effects.runtime_inputs,
            &[mcp_binary(101), mcp_binary(102)],
        ))
        .expect("two binaries under the harness");
        let mut delivered_from_first_socket = 0_usize;
        while rate_limit_drops_counted(&session.files.writes) == 0
            && delivered_from_first_socket < MOST_FRAMES_BEFORE_A_DROP
        {
            session.deliver_from_socket(
                &first_socket,
                &frame_from("ana", &format!("body {delivered_from_first_socket}")),
            );
            delivered_from_first_socket = delivered_from_first_socket.saturating_add(1);
        }
        assert_eq!(rate_limit_drops_counted(&session.files.writes), 1);
        let injected_before_second_socket = connect_errors(&wiring).len();
        session.deliver_from_socket(
            &second_socket,
            &frame_from("ana", "body from the second socket"),
        );
        assert_eq!(connect_errors(&wiring).len(), injected_before_second_socket);
        assert_eq!(rate_limit_drops_counted(&session.files.writes), 2);
    }

    fn class_files_written(session: &BridgeSession<DownConnector, StandInFiles>) -> Vec<String> {
        lock(&session.files.writes)
            .iter()
            .filter(|(path, _)| path == Path::new(PERMISSION_CLASS_FILE))
            .map(|(_, text)| text.clone())
            .collect()
    }

    #[test]
    fn spec_3_12_harness_2_1_319_a_session_started_by_hand_is_attested_from_its_live_transcript_on_every_frame()
     {
        for (mode, class) in [("bypassPermissions", "bypass"), ("auto", "prompting")] {
            let wiring = wiring();
            let session = bridge_session_over(&wiring, files_of_live_session(mode), DownConnector);
            session.deliver(&frame_from("ana", "one"));
            session.deliver(&frame_from("ana", "two"));
            assert_eq!(class_files_written(&session), [class], "{mode}");
            assert_eq!(
                lock(&wiring.errors)
                    .iter()
                    .filter(|message| message.starts_with("attested permission class"))
                    .count(),
                1,
                "{mode}"
            );
            let transcript_tail_reads = lock(&session.files.tail_reads)
                .iter()
                .filter(|path| path.as_path() == Path::new(TRANSCRIPT_FILE))
                .count();
            assert_eq!(transcript_tail_reads, 2, "{mode}");
        }
    }

    fn reads_of(session: &BridgeSession<DownConnector, StandInFiles>, file: &str) -> usize {
        lock(&session.files.reads)
            .iter()
            .filter(|path| path.as_path() == Path::new(file))
            .count()
    }

    #[test]
    fn spec_3_12_harness_2_1_319_the_transcript_path_is_resolved_once_and_only_its_tail_is_read_per_frame()
     {
        let wiring = wiring();
        let session = bridge_session(&wiring);
        for body in ["one", "two", "three"] {
            session.deliver(&frame_from("ana", body));
        }
        assert_eq!(reads_of(&session, REGISTRY_FILE), 1);
        assert_eq!(*lock(&session.files.listings), 1);
        assert_eq!(reads_of(&session, TRANSCRIPT_FILE), 0);
        assert_eq!(lock(&session.files.tail_reads).len(), 3);
    }

    #[test]
    fn spec_3_12_harness_2_1_319_a_tail_without_a_mode_entry_falls_back_to_one_whole_read() {
        let wiring = wiring();
        let mut files = files_of_live_session("bypassPermissions");
        files.tails.insert(
            PathBuf::from(TRANSCRIPT_FILE),
            "{\"type\":\"user\"}\n".to_owned(),
        );
        let session = bridge_session_over(&wiring, files, DownConnector);
        session.deliver(&frame_from("ana", "one"));
        assert_eq!(reads_of(&session, TRANSCRIPT_FILE), 1);
        assert_eq!(class_files_written(&session), ["bypass"]);
    }

    #[test]
    fn spec_3_12_harness_2_1_319_a_mode_entry_older_than_the_tail_window_is_read_whole_once_not_per_frame()
     {
        let wiring = wiring();
        let mut files = files_of_live_session("bypassPermissions");
        let filler_line = "{\"type\":\"user\"}\n";
        let window = usize::try_from(TRANSCRIPT_TAIL_WINDOW_BYTES).expect("window fits");
        files.tails.insert(
            PathBuf::from(TRANSCRIPT_FILE),
            filler_line.repeat(window / filler_line.len() + 1),
        );
        let session = bridge_session_over(&wiring, files, DownConnector);
        for body in ["one", "two", "three"] {
            session.deliver(&frame_from("ana", body));
        }
        assert_eq!(reads_of(&session, TRANSCRIPT_FILE), 1);
        assert_eq!(lock(&session.files.tail_reads).len(), 3);
        assert_eq!(class_files_written(&session), ["bypass"]);
    }

    #[test]
    fn spec_3_12_harness_2_1_319_a_cached_transcript_that_cannot_be_opened_is_resolved_again() {
        const OTHER_TRANSCRIPT: &str = "/home/ana/.claude/projects/-other/abc-1.jsonl";
        let wiring = wiring();
        let mut files = files_of_live_session("bypassPermissions");
        files.existing.insert(PathBuf::from(OTHER_TRANSCRIPT));
        files.texts.insert(
            PathBuf::from(OTHER_TRANSCRIPT),
            Ok("{\"type\":\"permission-mode\",\"permissionMode\":\"auto\"}\n".to_owned()),
        );
        files.directories.insert(
            PathBuf::from(PROJECTS_DIRECTORY_PATH),
            Ok(vec![
                PathBuf::from("/home/ana/.claude/projects/-work"),
                PathBuf::from("/home/ana/.claude/projects/-other"),
            ]),
        );
        let session = bridge_session_over(&wiring, files, DownConnector);
        session.deliver(&frame_from("ana", "one"));
        lock(&session.files.hidden).insert(PathBuf::from(TRANSCRIPT_FILE));
        session.deliver(&frame_from("ana", "two"));
        assert_eq!(class_files_written(&session), ["bypass", "prompting"]);
        assert_eq!(*lock(&session.files.listings), 2);
    }

    #[test]
    fn spec_3_12_harness_2_1_319_the_operating_system_reads_only_the_window_at_the_end_of_a_file() {
        let directory = scratch_directory("tail");
        let file = directory.join("transcript.jsonl");
        fs::write(&file, "first line\nsecond line\nthird line\n").expect("write");
        let window = OperatingSystemFiles::of_this_process()
            .unwrap()
            .read_tail_text(&file, 12)
            .expect("read tail");
        assert_eq!(window, "\nthird line\n");
        crate::remove_scratch::remove_scratch(&directory).expect("cleanup");
    }

    #[test]
    fn spec_3_12_harness_2_1_319_the_bridge_reads_and_logs_the_live_class_when_it_starts() {
        let polls = SnapshotSequence(RefCell::new(vec![Vec::new()]));
        for (mode, class) in [("bypassPermissions", "bypass"), ("auto", "prompting")] {
            let mut wiring = wiring();
            wiring.effects.files = files_of_live_session(mode);
            let writes = Arc::clone(&wiring.effects.files.writes);
            run_bridge(HARNESS_PID, wiring.effects, &polls).expect("bridge runs");
            assert_eq!(
                *lock(&wiring.errors),
                [format!("attested permission class: {class}")],
                "{mode}"
            );
            let class_files: Vec<String> = lock(&writes)
                .iter()
                .filter(|(path, _)| path == Path::new(PERMISSION_CLASS_FILE))
                .map(|(_, text)| text.clone())
                .collect();
            assert_eq!(class_files, [class], "{mode}");
        }
    }

    #[test]
    fn spec_3_12_harness_2_1_319_the_bridge_logs_that_no_live_class_exists_yet_when_it_starts() {
        let polls = SnapshotSequence(RefCell::new(vec![Vec::new()]));
        let wiring = wiring();
        run_bridge(HARNESS_PID, wiring.effects, &polls).expect("bridge runs");
        assert_eq!(
            *lock(&wiring.errors),
            [
                "cannot read the session's permission mode, keeping the class attested before: cannot read /home/ana/.claude/sessions/100.json: entity not found"
            ]
        );
    }

    #[test]
    fn spec_3_12_harness_2_1_319_the_class_read_live_is_the_class_written_to_the_session_socket() {
        const SOCKET_FILE: &str = "session.sock";
        let socket_directory = crate::fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estf-u43-{}", std::process::id()),
        )
        .expect("scratch directory");
        let socket_path = socket_directory.join(SOCKET_FILE);
        let listener = std::os::unix::net::UnixListener::bind(&socket_path).expect("bind");
        let server = thread::spawn(move || {
            let (mut connection, _) = listener.accept().expect("accept");
            let mut received = String::new();
            io::Read::read_to_string(&mut connection, &mut received).expect("read to close");
            received
        });
        let wiring = wiring();
        let session = bridge_session_over(
            &wiring,
            files_of_live_session("bypassPermissions"),
            UnixSocketConnector::new(socket_path.clone()),
        );
        session.deliver(&frame_from("ana", "x"));
        let received = server.join().expect("server thread");
        fs::remove_file(&socket_path).expect("unlink socket");
        crate::remove_scratch::remove_scratch(&socket_directory)
            .expect("remove the socket directory");
        assert!(received.contains("from-mode=\\\"bypass\\\""), "{received}");
    }

    #[test]
    fn spec_3_12_harness_2_1_319_a_session_whose_mode_cannot_be_read_is_reported_and_attests_nothing()
     {
        let wiring = wiring();
        let session = bridge_session_over(&wiring, StandInFiles::default(), DownConnector);
        session.deliver(&frame_from("ana", "one"));
        assert_eq!(class_files_written(&session), Vec::<String>::new());
        assert!(lock(&wiring.errors).iter().any(|message| message.starts_with(
            "cannot read the session's permission mode, keeping the class attested before: cannot read /home/ana/.claude/sessions/100.json"
        )));
    }

    #[test]
    fn spec_3_12_harness_2_1_319_a_frame_whose_sender_name_is_refused_is_reported_and_recorded_as_dropped()
     {
        let wiring = wiring();
        let session = bridge_session(&wiring);
        session.deliver(&frame_from("an\"a", "x"));
        assert!(
            lock(&wiring.errors)
                .iter()
                .any(|message| message.starts_with("frame from \"an\\\"a\" dropped"))
        );
        assert_eq!(written_counts(&session.files.writes), ["0 1 0"]);
    }

    fn scratch_directory(label: &str) -> PathBuf {
        let directory = crate::fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!(
                "estafette-bridge-claude-managed-{label}-{}",
                std::process::id()
            ),
        )
        .expect("scratch directory");
        fs::create_dir_all(directory.join(MANAGED_DROP_IN_DIRECTORY)).expect("scratch directory");
        directory
    }

    const MANAGED_DIRECTORY: &str = "/managed";

    #[test]
    fn spec_3_12_managed_settings_are_the_file_and_the_visible_json_files_of_its_drop_in_directory()
    {
        let directory = Path::new(MANAGED_DIRECTORY);
        let drop_in = directory.join(MANAGED_DROP_IN_DIRECTORY);
        let mut files = StandInFiles::default();
        for (path, text) in [
            (directory.join(MANAGED_SETTINGS_FILE), "{\"a\":1}"),
            (drop_in.join("20-b.json"), "{\"b\":2}"),
            (drop_in.join("10-a.json"), "{\"c\":3}"),
            (drop_in.join(".hidden.json"), "{\"d\":4}"),
            (drop_in.join("notes.txt"), "e"),
        ] {
            files.texts.insert(path, Ok(text.to_owned()));
        }
        files.directories.insert(
            drop_in.clone(),
            Ok(vec![
                drop_in.join("20-b.json"),
                drop_in.join("10-a.json"),
                drop_in.join(".hidden.json"),
                drop_in.join("notes.txt"),
            ]),
        );
        let managed = read_managed_settings(&files, directory);
        let read: Vec<(String, String)> = managed
            .files
            .iter()
            .map(|file| {
                (
                    file.path
                        .file_name()
                        .expect("name")
                        .to_string_lossy()
                        .into_owned(),
                    file.text.clone(),
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("10-a.json".to_owned(), "{\"c\":3}".to_owned()),
                ("20-b.json".to_owned(), "{\"b\":2}".to_owned()),
                ("managed-settings.json".to_owned(), "{\"a\":1}".to_owned()),
            ]
        );
        assert_eq!(managed.unreadable, Vec::<PathBuf>::new());
        assert!(!lock(&files.reads).contains(&drop_in.join("notes.txt")));
    }

    #[test]
    fn spec_3_12_a_managed_settings_path_that_exists_but_cannot_be_read_is_reported_unreadable() {
        let directory = Path::new(MANAGED_DIRECTORY);
        let drop_in = directory.join(MANAGED_DROP_IN_DIRECTORY);
        let unreadable = drop_in.join("30-bad.json");
        let mut files = StandInFiles::default();
        files
            .texts
            .insert(unreadable.clone(), Err(io::ErrorKind::PermissionDenied));
        files
            .directories
            .insert(drop_in, Ok(vec![unreadable.clone()]));
        let managed = read_managed_settings(&files, directory);
        assert_eq!(managed.unreadable, [unreadable]);
        assert_eq!(managed.files.len(), 0);
        let absent = read_managed_settings(&StandInFiles::default(), Path::new("/missing"));
        assert_eq!((absent.files.len(), absent.unreadable.len()), (0, 0));
    }

    #[test]
    fn spec_3_12_a_failed_check_ends_the_doctor_with_exit_3_and_other_outcomes_with_0_and_1() {
        let errors: Arc<Mutex<Vec<u8>>> = Arc::default();
        let record = run_record_on(Arc::clone(&errors), None);
        assert_eq!(exit_code_of(Ok(true), &record), ExitCode::SUCCESS);
        assert_eq!(exit_code_of(Ok(false), &record), ExitCode::from(3));
        assert_eq!(*lock(&errors), b"");
        assert_eq!(
            exit_code_of(Err(CommandError::MissingVariable(HOME_VARIABLE)), &record),
            ExitCode::from(1)
        );
        assert_eq!(*lock(&errors), b"HOME is not set\n");
    }

    #[derive(Default)]
    struct StandInFiles {
        texts: BTreeMap<PathBuf, Result<String, io::ErrorKind>>,
        directories: BTreeMap<PathBuf, Result<Vec<PathBuf>, io::ErrorKind>>,
        existing: BTreeSet<PathBuf>,
        tails: BTreeMap<PathBuf, String>,
        hidden: Mutex<BTreeSet<PathBuf>>,
        reads: Mutex<Vec<PathBuf>>,
        tail_reads: Mutex<Vec<PathBuf>>,
        listings: Mutex<usize>,
        writes: Arc<Mutex<Vec<(PathBuf, String)>>>,
        user: Option<u32>,
        entries: Arc<Mutex<BTreeMap<PathBuf, EntryFacts>>>,
        entry_failures: Mutex<BTreeMap<PathBuf, io::ErrorKind>>,
        creation_failures: BTreeMap<PathBuf, io::ErrorKind>,
        created: Mutex<Vec<PathBuf>>,
    }

    impl StandInFiles {
        fn text_of(&self, path: &Path) -> io::Result<String> {
            if lock(&self.hidden).contains(path) {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            match self.texts.get(path) {
                Some(Ok(text)) => Ok(text.clone()),
                Some(Err(kind)) => Err(io::Error::from(*kind)),
                None => Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        }
    }

    impl Files for StandInFiles {
        fn read_text(&self, path: &Path) -> io::Result<String> {
            lock(&self.reads).push(path.to_owned());
            self.text_of(path)
        }

        fn read_tail_text(&self, path: &Path, _window_bytes: u64) -> io::Result<String> {
            lock(&self.tail_reads).push(path.to_owned());
            self.tails
                .get(path)
                .map_or_else(|| self.text_of(path), |tail| Ok(tail.clone()))
        }

        fn list_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            {
                let mut listings = lock(&self.listings);
                *listings = listings.saturating_add(1);
            }
            match self.directories.get(path) {
                Some(Ok(listing)) => Ok(listing.clone()),
                Some(Err(kind)) => Err(io::Error::from(*kind)),
                None => Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        }

        fn exists(&self, path: &Path) -> bool {
            self.existing.contains(path) && !lock(&self.hidden).contains(path)
        }

        fn create_private_directory(&self, path: &Path) -> io::Result<()> {
            if let Some(kind) = self.creation_failures.get(path) {
                return Err(io::Error::from(*kind));
            }
            lock(&self.created).push(path.to_owned());
            Ok(())
        }

        fn user_id(&self) -> u32 {
            self.user.unwrap_or(USER_OF_WIRING)
        }

        fn entry_facts(&self, path: &Path) -> io::Result<EntryFacts> {
            if let Some(kind) = lock(&self.entry_failures).get(path) {
                return Err(io::Error::from(*kind));
            }
            lock(&self.entries)
                .get(path)
                .copied()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }

        fn read_state_text(&self, path: &Path) -> io::Result<String> {
            self.text_of(path)
        }

        fn write_text_atomically(&self, path: &Path, text: &str) -> io::Result<()> {
            lock(&self.writes).push((path.to_owned(), text.to_owned()));
            Ok(())
        }
    }

    const USER_SETTINGS: &str = "/home/ana/.claude/settings.json";
    const PROJECT_SETTINGS: &str = "/work/.claude/settings.json";
    const LOCAL_PROJECT_SETTINGS: &str = "/work/.claude/settings.local.json";

    fn doctor_environment() -> Environment {
        Environment {
            home: Some(PathBuf::from("/home/ana")),
            messaging_socket: Some(PathBuf::from("/tmp/session.sock")),
            ..bare_environment()
        }
    }

    fn doctor_output_with(files: &StandInFiles) -> (Result<bool, CommandError>, String) {
        let mut output = Vec::new();
        let outcome = run_doctor_command(
            &doctor_environment(),
            files,
            &FixedProcessTable(Vec::new()),
            &mut output,
        );
        (outcome, String::from_utf8(output).expect("utf8 report"))
    }

    #[test]
    fn spec_3_12_doctor_reads_user_project_local_then_managed_settings_and_skips_missing_ones() {
        let mut files = StandInFiles::default();
        files
            .texts
            .insert(PathBuf::from(PROJECT_SETTINGS), Ok("{}".to_owned()));
        files.existing.insert(PathBuf::from("/tmp/session.sock"));
        let (outcome, report) = doctor_output_with(&files);
        assert!(!outcome.expect("doctor ran"));
        let managed_directory = Path::new(MANAGED_SETTINGS_DIRECTORY);
        assert_eq!(
            *lock(&files.reads),
            [
                PathBuf::from(USER_SETTINGS),
                PathBuf::from(PROJECT_SETTINGS),
                PathBuf::from(LOCAL_PROJECT_SETTINGS),
                managed_directory.join(MANAGED_SETTINGS_FILE),
            ]
        );
        assert!(report.contains("ok    socket path exists\n"), "{report}");
    }

    struct UnwritableOutput;

    impl Write for UnwritableOutput {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn spec_3_12_a_doctor_report_that_cannot_be_written_ends_the_doctor_with_the_write_error() {
        let outcome = run_doctor_command(
            &doctor_environment(),
            &StandInFiles::default(),
            &FixedProcessTable(Vec::new()),
            &mut UnwritableOutput,
        );
        assert!(matches!(outcome, Err(CommandError::ReportUnwritable(_))));
    }

    #[test]
    fn spec_3_12_the_doctor_creates_no_directory() {
        let files = StandInFiles::default();
        let (outcome, _report) = doctor_output_with(&files);
        outcome.expect("doctor ran");
        assert_eq!(*lock(&files.created), Vec::<PathBuf>::new());
    }

    #[test]
    fn spec_3_12_doctor_stops_on_a_settings_file_that_exists_but_cannot_be_read() {
        let mut files = StandInFiles::default();
        files.texts.insert(
            PathBuf::from(USER_SETTINGS),
            Err(io::ErrorKind::PermissionDenied),
        );
        let (outcome, report) = doctor_output_with(&files);
        assert!(matches!(
            outcome,
            Err(CommandError::SettingsUnreadable { ref path, .. }) if path == Path::new(USER_SETTINGS)
        ));
        assert_eq!(report, "");
    }

    #[test]
    fn spec_3_12_doctor_fails_the_refuse_check_for_a_refuse_in_the_project_settings() {
        let mut files = StandInFiles::default();
        files.texts.insert(
            PathBuf::from(PROJECT_SETTINGS),
            Ok(r#"{"crossSessionInbound":"refuse"}"#.to_owned()),
        );
        let (outcome, report) = doctor_output_with(&files);
        assert!(!outcome.expect("doctor ran"));
        assert!(
            report.contains(
                "FAIL  no refuse applies: /work/.claude/settings.json sets crossSessionInbound to refuse"
            ),
            "{report}"
        );
        assert!(report.contains("FAIL  socket path exists: CLAUDE_CODE_MESSAGING_SOCKET names /tmp/session.sock, which does not exist"), "{report}");
    }

    #[test]
    fn spec_3_12_doctor_hands_an_unreadable_managed_file_to_the_refuse_check_as_unverified() {
        let mut files = StandInFiles::default();
        let managed_file = Path::new(MANAGED_SETTINGS_DIRECTORY).join(MANAGED_SETTINGS_FILE);
        files
            .texts
            .insert(managed_file.clone(), Err(io::ErrorKind::PermissionDenied));
        let (_, report) = doctor_output_with(&files);
        let refuse_line = report
            .lines()
            .find(|line| line.starts_with("UNVERIFIED no refuse applies:"))
            .expect("refuse check is unverified");
        assert!(
            refuse_line.ends_with(&format!(
                "{} (the file cannot be read)",
                managed_file.display()
            )),
            "{report}"
        );
    }

    #[test]
    fn spec_3_12_every_staged_write_of_a_state_file_gets_a_staging_path_of_its_own() {
        let target = Path::new("/state/count");
        let staging_paths: BTreeSet<PathBuf> = (0..STAGED_WRITES)
            .map(|_| unique_staging_path(target))
            .collect();
        assert_eq!(staging_paths.len(), STAGED_WRITES);
        assert!(
            staging_paths
                .iter()
                .all(|path| path.parent() == target.parent()
                    && path.to_string_lossy().ends_with(STAGING_SUFFIX))
        );
    }

    #[test]
    fn spec_3_12_two_threads_writing_one_state_file_each_land_a_whole_write() {
        const WRITES_PER_THREAD: usize = 300;
        const WRITE_LENGTH: usize = 65_536;
        let directory = scratch_directory("same-file");
        let target = directory.join("count");
        let texts = ["a".repeat(WRITE_LENGTH), "b".repeat(WRITE_LENGTH)];
        let write_repeatedly = |text: &String| -> Vec<io::Error> {
            (0..WRITES_PER_THREAD)
                .filter_map(|_| {
                    OperatingSystemFiles::of_this_process()
                        .unwrap()
                        .write_text_atomically(&target, text)
                        .err()
                })
                .collect()
        };
        let failures: Vec<io::Error> = thread::scope(|writers| {
            let first_writer = writers.spawn(|| write_repeatedly(&texts[0]));
            let second_writer = writers.spawn(|| write_repeatedly(&texts[1]));
            [first_writer, second_writer]
                .into_iter()
                .flat_map(|writer| writer.join().expect("writer"))
                .collect()
        });
        assert!(failures.is_empty(), "{failures:?}");
        assert!(texts.contains(&fs::read_to_string(&target).expect("final file")));
        crate::remove_scratch::remove_scratch(&directory).expect("cleanup");
    }

    #[test]
    fn spec_3_12_the_operating_system_writes_the_count_by_staging_then_renaming() {
        let directory = scratch_directory("atomic");
        let target = directory.join("count");
        OperatingSystemFiles::of_this_process()
            .unwrap()
            .write_text_atomically(&target, "3")
            .expect("first write");
        OperatingSystemFiles::of_this_process()
            .unwrap()
            .write_text_atomically(&target, "12")
            .expect("second write");
        assert_eq!(fs::read_to_string(&target).expect("count"), "12");
        assert!(
            fs::read_dir(&directory)
                .expect("list")
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(STAGING_SUFFIX))
        );
        crate::remove_scratch::remove_scratch(&directory).expect("cleanup");
    }

    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const DATA_HOME_UNDER_ROOT: &str = "data";
    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
    const UNREADABLE_CLASS_LINE: &str = "cannot read the session's permission mode, keeping the class attested before: cannot read /home/ana/.claude/sessions/100.json: entity not found\n";

    struct TraceProbe {
        root: PathBuf,
        data_home: PathBuf,
        data_home_variable: &'static str,
        run_id_source_fails: bool,
        directory_creation_fails: bool,
        drawn_run_ids: Arc<Mutex<usize>>,
        created_directories: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl TraceProbe {
        fn new(label: &str) -> Self {
            let root = crate::fresh_directory::fresh_directory_in(
                &std::env::temp_dir(),
                &format!(
                    "estafette-bridge-claude-trace-{label}-{}",
                    std::process::id()
                ),
            )
            .expect("scratch directory");
            Self {
                data_home: root.join(DATA_HOME_UNDER_ROOT),
                root,
                data_home_variable: DATA_HOME_VARIABLE,
                run_id_source_fails: false,
                directory_creation_fails: false,
                drawn_run_ids: Arc::default(),
                created_directories: Arc::default(),
            }
        }

        fn run(
            &self,
            tracing: Tracing,
            effects: TraceEffects,
            bounds: TraceBounds,
        ) -> (ExitCode, String) {
            self.run_over(
                tracing,
                effects,
                bounds,
                wiring().effects,
                &FixedProcessTable(Vec::new()),
            )
        }

        fn run_over<S, B, C>(
            &self,
            tracing: Tracing,
            effects: TraceEffects,
            bounds: TraceBounds,
            bridge_effects: BridgeEffects<S, C, StandInFiles>,
            processes: &impl ProcessTable,
        ) -> (ExitCode, String)
        where
            S: InboxConnector + Send + 'static,
            B: InboxConnector + Send + 'static,
            C: Fn(PathBuf) -> B,
        {
            let data_home = self.data_home.clone();
            let data_home_variable = self.data_home_variable;
            let environment =
                move |name: &str| (name == data_home_variable).then(|| OsString::from(&data_home));
            let drawn_run_ids = Arc::clone(&self.drawn_run_ids);
            let run_id_source_fails = self.run_id_source_fails;
            let draw_run_id = move || {
                {
                    let mut drawn = lock(&drawn_run_ids);
                    *drawn = drawn.saturating_add(1);
                }
                if run_id_source_fails {
                    return Err(io::Error::other(STAND_IN_FAILURE));
                }
                Ok(RUN_BYTES)
            };
            let created_directories = Arc::clone(&self.created_directories);
            let directory_creation_fails = self.directory_creation_fails;
            let create_private_directory = move |path: &Path| {
                lock(&created_directories).push(path.to_owned());
                if directory_creation_fails {
                    return Err(io::Error::from(io::ErrorKind::PermissionDenied));
                }
                fs::create_dir_all(path)
            };
            let errors: Arc<Mutex<Vec<u8>>> = Arc::default();
            let status = run_command(
                HARNESS_PID,
                tracing,
                &TraceInputs {
                    environment: &environment,
                    draw_run_id: &draw_run_id,
                    create_private_directory: &create_private_directory,
                    effects,
                    bounds,
                },
                Arc::clone(&errors),
                move |record| {
                    Ok(BridgeEffects {
                        record,
                        ..bridge_effects
                    })
                },
                processes,
            );
            let written = String::from_utf8(lock(&errors).clone()).expect("utf8 standard error");
            (status, written)
        }
    }

    impl Drop for TraceProbe {
        fn drop(&mut self) {
            if let Err(failure) = crate::remove_scratch::remove_scratch(&self.root) {
                assert!(thread::panicking(), "{failure}");
                eprintln!("{failure}");
            }
        }
    }

    #[test]
    fn observability_decision_3_a_run_with_no_debug_writes_standard_error_as_without_a_trace_and_touches_nothing_of_it()
     {
        let probe = TraceProbe::new("no-debug");

        let (status, written) = probe.run(
            Tracing::NoDebug,
            TraceEffects::of_the_operating_system(frozen_trace_clocks()),
            TraceBounds::SPEC,
        );

        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(written, UNREADABLE_CLASS_LINE);
        assert_eq!(*lock(&probe.drawn_run_ids), 0);
        assert_eq!(*lock(&probe.created_directories), Vec::<PathBuf>::new());
        assert!(!probe.data_home.exists());
    }

    const FROZEN_MONOTONIC_MS: u64 = 7;
    const START_WALL_MS: u64 = 951_868_799_123;
    const EVENTS_FILE_NAME: &str = "20000229T235959Z-a1b2c3d4e5f60718-bridge.events.jsonl";
    fn trace_line(n: u64, rest: &str) -> String {
        format!(
            "{{\"v\":2,\"run\":\"a1b2c3d4e5f60718\",\"n\":{n},\"mono_ms\":7,\"wall_ms\":951868799123,{rest}}}\n"
        )
    }

    impl TraceProbe {
        fn traces(&self) -> PathBuf {
            self.data_home.join("estafette").join("traces")
        }

        fn events_file(&self) -> PathBuf {
            self.traces().join(EVENTS_FILE_NAME)
        }
    }

    #[test]
    fn observability_decision_4_a_run_with_debug_names_its_events_file_first_on_standard_error_and_writes_its_events_there()
     {
        let probe = TraceProbe::new("debug");

        let (status, written) = probe.run(
            Tracing::Debug,
            TraceEffects::of_the_operating_system(frozen_trace_clocks()),
            TraceBounds::SPEC,
        );

        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(
            written,
            format!(
                "writing the debug trace: {} (safe to post)\n{UNREADABLE_CLASS_LINE}",
                probe.events_file().display()
            )
        );
        assert_eq!(*lock(&probe.drawn_run_ids), 1);
        assert_eq!(
            *lock(&probe.created_directories),
            [probe.data_home.join("estafette"), probe.traces()]
        );
        assert_eq!(
            fs::read_to_string(probe.events_file()).expect("events file"),
            [
                trace_line(
                    0,
                    &format!(
                        "\"event\":\"process.started\",\"version\":\"{}\",\"harness_pid\":100",
                        env!("CARGO_PKG_VERSION")
                    )
                ),
                trace_line(
                    1,
                    "\"event\":\"permission_class.unreadable\",\"reason\":\"registry_unreadable\""
                ),
            ]
            .concat()
        );
        assert_eq!(
            fs::read_dir(probe.traces()).expect("traces").count(),
            1,
            "the bridge writes the events file alone"
        );
    }

    fn frozen_trace_clocks() -> TraceClocks {
        TraceClocks {
            monotonic_ms: Arc::new(|| FROZEN_MONOTONIC_MS),
            wall_ms: Arc::new(|| START_WALL_MS),
        }
    }

    const SHORT_DRAIN: Duration = Duration::from_millis(20);

    struct StuckFile {
        until_unstuck: std::sync::mpsc::Receiver<()>,
    }

    impl Write for StuckFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.until_unstuck.recv().map_err(io::Error::other)?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl estafette_trace::TraceFile for StuckFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn observability_decision_12_a_stop_whose_writer_still_holds_lines_at_the_drain_bound_is_recorded_on_standard_error()
     {
        let probe = TraceProbe::new("not-drained");
        let (to_the_test, held_unsticks) = std::sync::mpsc::channel();
        let effects = TraceEffects {
            create_file: Arc::new(move |_path: &Path| {
                let (unstick, until_unstuck) = std::sync::mpsc::channel();
                to_the_test.send(unstick).map_err(io::Error::other)?;
                let stuck: Box<dyn estafette_trace::TraceFile> =
                    Box::new(StuckFile { until_unstuck });
                Ok(stuck)
            }),
            ..TraceEffects::of_the_operating_system(frozen_trace_clocks())
        };

        let (status, written) = probe.run(
            Tracing::Debug,
            effects,
            TraceBounds {
                drain: SHORT_DRAIN,
                ..TraceBounds::SPEC
            },
        );

        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(
            written.lines().last(),
            Some(
                "the debug trace did not finish writing its events file within 20 ms of the stop: the file's last lines may be missing"
            ),
            "{written}"
        );
        drop(held_unsticks);
    }

    const STAND_IN_FAILURE: &str = "a stand-in failure";

    #[test]
    fn observability_decision_4_a_debug_trace_that_cannot_start_refuses_the_run_before_it_serves_naming_why_and_the_flag_that_skips_it()
     {
        let mut without_a_data_directory = TraceProbe::new("refused-data-directory");
        without_a_data_directory.data_home_variable = "VARIABLE_UNSET_IN_THIS_TEST";
        let mut without_a_run_id = TraceProbe::new("refused-run-id");
        without_a_run_id.run_id_source_fails = true;
        let mut without_a_trace_directory = TraceProbe::new("refused-directory");
        without_a_trace_directory.directory_creation_fails = true;
        let refused_trace_directory = format!(
            "cannot create the trace directory {}: permission denied; run with --no-debug to write no trace\n",
            without_a_trace_directory
                .data_home
                .join("estafette")
                .display()
        );

        for (probe, refusal) in [
            (
                &without_a_data_directory,
                "cannot place the debug trace: XDG_DATA_HOME and HOME are both unset or empty: set one of them; run with --no-debug to write no trace\n",
            ),
            (
                &without_a_run_id,
                "cannot read the secure random source for the run id of the debug trace: a stand-in failure; run with --no-debug to write no trace\n",
            ),
            (&without_a_trace_directory, refused_trace_directory.as_str()),
        ] {
            let (status, written) = probe.run(
                Tracing::Debug,
                TraceEffects::of_the_operating_system(frozen_trace_clocks()),
                TraceBounds::SPEC,
            );

            assert_eq!(status, ExitCode::from(EXIT_FAILURE), "{written}");
            assert_eq!(written, refusal);
        }
    }

    struct UncuttableFile {
        sized_bytes: u64,
    }

    impl Write for UncuttableFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl estafette_trace::TraceFile for UncuttableFile {
        fn set_length(&mut self, length: u64) -> io::Result<()> {
            if length < self.sized_bytes {
                return Err(io::Error::other(STAND_IN_FAILURE));
            }
            self.sized_bytes = length;
            Ok(())
        }
    }

    #[test]
    fn observability_decision_12_a_stop_that_cannot_cut_the_events_file_back_is_recorded_on_standard_error()
     {
        let probe = TraceProbe::new("not-cut");
        let effects = TraceEffects {
            create_file: Arc::new(|_path: &Path| {
                let uncuttable: Box<dyn estafette_trace::TraceFile> =
                    Box::new(UncuttableFile { sized_bytes: 0 });
                Ok(uncuttable)
            }),
            ..TraceEffects::of_the_operating_system(frozen_trace_clocks())
        };

        let (status, written) = probe.run(Tracing::Debug, effects, TraceBounds::SPEC);

        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(
            written.lines().last(),
            Some(
                "the debug trace could not cut its events file back to its last line at the stop, so the file ends in zero bytes: a stand-in failure"
            ),
            "{written}"
        );
    }

    const HARNESS_EXITS_AT: Duration = Duration::from_secs(125);
    const SPEC_LIVE_PROCESS_WRITES_AT_LEAST_EVERY_MS: u64 = 60_000;
    const SPEC_ALIVE_EVENT: &str = "process.alive";

    struct HarnessRunsUntil {
        clock: Arc<Mutex<Duration>>,
        harness_exits_at: Duration,
    }

    impl ProcessTable for HarnessRunsUntil {
        fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
            Ok(if *lock(&self.clock) < self.harness_exits_at {
                harness_without_binaries()
            } else {
                Vec::new()
            })
        }
    }

    fn milliseconds_on(clock: &Mutex<Duration>) -> u64 {
        u64::try_from(lock(clock).as_millis()).unwrap_or(u64::MAX)
    }

    fn run_serving_on_a_clock_only_its_waits_move(
        probe: &TraceProbe,
        tracing: Tracing,
    ) -> (ExitCode, String) {
        let clock: Arc<Mutex<Duration>> = Arc::default();
        let harness = HarnessRunsUntil {
            clock: Arc::clone(&clock),
            harness_exits_at: HARNESS_EXITS_AT,
        };
        run_over_a_clock_only_its_waits_move(
            probe,
            tracing,
            &clock,
            |elapsed, wait| BridgeEffects {
                elapsed,
                wait,
                ..wiring().effects
            },
            &harness,
        )
    }

    fn run_over_a_clock_only_its_waits_move<S, B, C>(
        probe: &TraceProbe,
        tracing: Tracing,
        clock: &Arc<Mutex<Duration>>,
        serving: impl FnOnce(Elapsed, Pause) -> BridgeEffects<S, C, StandInFiles>,
        processes: &impl ProcessTable,
    ) -> (ExitCode, String)
    where
        S: InboxConnector + Send + 'static,
        B: InboxConnector + Send + 'static,
        C: Fn(PathBuf) -> B,
    {
        let monotonic_clock = Arc::clone(clock);
        let wall_clock = Arc::clone(clock);
        let bridge_clock = Arc::clone(clock);
        let moved_clock = Arc::clone(clock);
        probe.run_over(
            tracing,
            TraceEffects::of_the_operating_system(TraceClocks {
                monotonic_ms: Arc::new(move || milliseconds_on(&monotonic_clock)),
                wall_ms: Arc::new(move || {
                    START_WALL_MS.saturating_add(milliseconds_on(&wall_clock))
                }),
            }),
            TraceBounds::SPEC,
            serving(
                Arc::new(move || *lock(&bridge_clock)),
                Arc::new(move |pause| {
                    let mut now = lock(&moved_clock);
                    *now = now.saturating_add(pause);
                }),
            ),
            processes,
        )
    }

    #[test]
    fn observability_decision_13_a_bridge_serving_with_no_frame_writes_a_line_to_its_events_file_every_60_s_of_its_clock_and_nothing_more()
     {
        let probe = TraceProbe::new("every-60-s");

        let (status, written) = run_serving_on_a_clock_only_its_waits_move(&probe, Tracing::Debug);

        let events: Vec<serde_json::Value> = fs::read_to_string(probe.events_file())
            .expect("events file")
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object a line"))
            .collect();
        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(
            written,
            format!(
                "writing the debug trace: {} (safe to post)\n{UNREADABLE_CLASS_LINE}",
                probe.events_file().display()
            )
        );
        assert_eq!(
            events
                .iter()
                .map(|line| (line["event"].clone(), line["mono_ms"].clone()))
                .collect::<Vec<_>>(),
            [
                ("process.started".into(), 0.into()),
                ("permission_class.unreadable".into(), 0.into()),
                (
                    SPEC_ALIVE_EVENT.into(),
                    SPEC_LIVE_PROCESS_WRITES_AT_LEAST_EVERY_MS.into()
                ),
                (
                    SPEC_ALIVE_EVENT.into(),
                    SPEC_LIVE_PROCESS_WRITES_AT_LEAST_EVERY_MS
                        .saturating_mul(2)
                        .into()
                ),
            ]
        );
        for alive in events
            .iter()
            .filter(|line| line["event"] == SPEC_ALIVE_EVENT)
        {
            assert_eq!(
                alive
                    .as_object()
                    .expect("an object")
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<&str>>(),
                BTreeSet::from(["v", "run", "n", "mono_ms", "wall_ms", "event"]),
                "{alive}"
            );
        }
    }

    #[test]
    fn observability_decision_3_a_bridge_serving_past_60_s_with_no_debug_writes_standard_error_as_before_and_no_trace()
     {
        let probe = TraceProbe::new("every-60-s-no-debug");

        let (status, written) =
            run_serving_on_a_clock_only_its_waits_move(&probe, Tracing::NoDebug);

        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(written, UNREADABLE_CLASS_LINE);
        assert!(!probe.data_home.exists());
    }

    const SPEC_NOTICE_WRITTEN_EVENT: &str = "notice.written";
    const FRAME_DELIVERED_AT_MS: u64 = 90_000;
    const HARNESS_EXITS_AFTER_THE_FRAME_AT: Duration = Duration::from_secs(155);

    struct HarnessListsItsBinaryAtOnePollFrom {
        harness: HarnessRunsUntil,
        binary_listed_from: Duration,
        binary_listed: Cell<bool>,
    }

    impl ProcessTable for HarnessListsItsBinaryAtOnePollFrom {
        fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
            let entries = self.harness.list()?;
            let binary_appears = !entries.is_empty()
                && *lock(&self.harness.clock) >= self.binary_listed_from
                && !self.binary_listed.replace(true);
            Ok(if binary_appears {
                harness_with_one_binary()
            } else {
                entries
            })
        }
    }

    #[test]
    fn observability_decision_13_a_frame_delivered_between_two_alive_lines_leaves_the_second_one_period_after_the_first()
     {
        let probe = TraceProbe::new("frame-between-alive-lines");
        let clock: Arc<Mutex<Duration>> = Arc::default();
        let wiring = wiring();
        let connections = Arc::new(Mutex::new(VecDeque::from([ScriptedInboxConnection {
            replies: scripted_frame_line(1, "ana", "one"),
            fails_after_replies: false,
        }])));

        let (status, _) = run_over_a_clock_only_its_waits_move(
            &probe,
            Tracing::Debug,
            &clock,
            |elapsed, wait| BridgeEffects {
                own_pid: wiring.effects.own_pid,
                runtime_inputs: wiring.effects.runtime_inputs,
                session_connector: WritableSessionSocket,
                session_token: wiring.effects.session_token,
                claude_config_directory: wiring.effects.claude_config_directory,
                connect_binary_socket: move |_socket_path| ScriptedInboxSocket {
                    connections: Arc::clone(&connections),
                    since_lines: SinceLines::default(),
                },
                files: files_of_live_session("bypassPermissions"),
                elapsed,
                record: wiring.effects.record,
                spawn: Arc::new(|task| task()),
                wait,
            },
            &HarnessListsItsBinaryAtOnePollFrom {
                harness: HarnessRunsUntil {
                    clock: Arc::clone(&clock),
                    harness_exits_at: HARNESS_EXITS_AFTER_THE_FRAME_AT,
                },
                binary_listed_from: Duration::from_millis(FRAME_DELIVERED_AT_MS),
                binary_listed: Cell::new(false),
            },
        );

        let deliveries_and_alive_lines: Vec<(serde_json::Value, serde_json::Value)> =
            fs::read_to_string(probe.events_file())
                .expect("events file")
                .lines()
                .map(|line| {
                    serde_json::from_str::<serde_json::Value>(line).expect("one JSON object a line")
                })
                .filter(|line| {
                    line["event"] == SPEC_ALIVE_EVENT || line["event"] == SPEC_NOTICE_WRITTEN_EVENT
                })
                .map(|line| (line["event"].clone(), line["mono_ms"].clone()))
                .collect();
        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(
            deliveries_and_alive_lines,
            [
                (
                    SPEC_ALIVE_EVENT.into(),
                    SPEC_LIVE_PROCESS_WRITES_AT_LEAST_EVERY_MS.into()
                ),
                (
                    SPEC_NOTICE_WRITTEN_EVENT.into(),
                    FRAME_DELIVERED_AT_MS.into()
                ),
                (
                    SPEC_ALIVE_EVENT.into(),
                    SPEC_LIVE_PROCESS_WRITES_AT_LEAST_EVERY_MS
                        .saturating_mul(2)
                        .into()
                ),
            ]
        );
    }

    struct WritableSessionSocket;

    impl InboxConnector for WritableSessionSocket {
        type Stream = io::Cursor<Vec<u8>>;

        fn connect(&mut self) -> io::Result<io::Cursor<Vec<u8>>> {
            Ok(io::Cursor::new(Vec::new()))
        }
    }

    fn scripted_frame_line(seq: u64, from: &str, body: &str) -> String {
        format!(
            "{}\n",
            serde_json::json!({"seq": seq, "id": format!("id-{seq}"), "from": from, "body": body})
        )
    }

    struct ScriptedSession<S> {
        session_connector: S,
        session_token: String,
        runtime_directory: PathBuf,
        session_files: StandInFiles,
        inbox_connections: Vec<ScriptedInboxConnection>,
        since_lines: SinceLines,
    }

    fn one_inbox_connection_replying(frames: String) -> Vec<ScriptedInboxConnection> {
        vec![ScriptedInboxConnection {
            replies: frames,
            fails_after_replies: false,
        }]
    }

    fn run_scripted_session<S: InboxConnector + Send + 'static>(
        probe: &TraceProbe,
        session: ScriptedSession<S>,
    ) -> (ExitCode, String, String) {
        let data_home = probe.data_home.clone();
        let environment =
            move |name: &str| (name == DATA_HOME_VARIABLE).then(|| OsString::from(&data_home));
        let errors: Arc<Mutex<Vec<u8>>> = Arc::default();
        let wiring = wiring();
        let polls_that_see_the_binary = session.inbox_connections.len().saturating_add(1);
        let connections = Arc::new(Mutex::new(VecDeque::from(session.inbox_connections)));
        let status = run_command(
            HARNESS_PID,
            Tracing::Debug,
            &TraceInputs {
                environment: &environment,
                draw_run_id: &|| Ok(RUN_BYTES),
                create_private_directory: &|path: &Path| fs::create_dir_all(path),
                effects: TraceEffects::of_the_operating_system(frozen_trace_clocks()),
                bounds: TraceBounds::SPEC,
            },
            Arc::clone(&errors),
            move |record| {
                Ok(BridgeEffects {
                    own_pid: wiring.effects.own_pid,
                    runtime_inputs: RuntimeDirectoryInputs {
                        xdg_runtime_dir: Some(session.runtime_directory),
                        ..wiring.effects.runtime_inputs
                    },
                    session_connector: session.session_connector,
                    session_token: session.session_token,
                    claude_config_directory: wiring.effects.claude_config_directory,
                    connect_binary_socket: move |_socket_path| ScriptedInboxSocket {
                        connections: Arc::clone(&connections),
                        since_lines: Arc::clone(&session.since_lines),
                    },
                    files: session.session_files,
                    elapsed: wiring.effects.elapsed,
                    record,
                    spawn: Arc::new(|task| task()),
                    wait: Arc::new(|_| {}),
                })
            },
            &SnapshotSequence(RefCell::new(
                std::iter::repeat_n(harness_with_one_binary(), polls_that_see_the_binary)
                    .chain([Vec::new()])
                    .collect(),
            )),
        );
        let written = String::from_utf8(lock(&errors).clone()).expect("utf8 standard error");
        let events = fs::read_to_string(probe.events_file()).expect("events file");
        (status, written, events)
    }

    #[test]
    fn observability_decision_10_the_bridge_records_its_start_each_inbox_connection_and_loss_each_frame_the_class_it_read_and_each_notice_or_drop()
     {
        let probe = TraceProbe::new("decision-10");
        let frames = [
            scripted_frame_line(1, "ana", "one"),
            scripted_frame_line(2, "ana", "one"),
            scripted_frame_line(3, "an\"a", "three"),
        ]
        .concat();

        let (status, written, events) = run_scripted_session(
            &probe,
            ScriptedSession {
                session_connector: WritableSessionSocket,
                session_token: "token".to_owned(),
                runtime_directory: PathBuf::from("/run/user/1000"),
                session_files: files_of_live_session("bypassPermissions"),
                inbox_connections: one_inbox_connection_replying(frames),
                since_lines: SinceLines::default(),
            },
        );

        let version = env!("CARGO_PKG_VERSION");
        let class_read = "\"event\":\"permission_class.read\",\"class\":\"bypass\",\"source\":\"transcript_tail\"";
        let expected_events = [
            format!("\"event\":\"process.started\",\"version\":\"{version}\",\"harness_pid\":100"),
            class_read.to_owned(),
            "\"event\":\"permission_class.attested\",\"class\":\"bypass\"".to_owned(),
            "\"event\":\"inbox.connected\",\"binary_pid\":101".to_owned(),
            "\"event\":\"frame.received\",\"binary_pid\":101,\"message\":\"id-1\",\"frame\":1"
                .to_owned(),
            class_read.to_owned(),
            "\"event\":\"notice.written\",\"message\":\"id-1\",\"frame\":1,\"class\":\"bypass\""
                .to_owned(),
            "\"event\":\"frame.received\",\"binary_pid\":101,\"message\":\"id-2\",\"frame\":2"
                .to_owned(),
            class_read.to_owned(),
            "\"event\":\"frame.dropped_by_rate_limit\",\"message\":\"id-2\",\"frame\":2".to_owned(),
            "\"event\":\"frame.received\",\"binary_pid\":101,\"message\":\"id-3\",\"frame\":3"
                .to_owned(),
            class_read.to_owned(),
            "\"event\":\"frame.dropped_sender_name_refused\",\"message\":\"id-3\",\"frame\":3"
                .to_owned(),
            "\"event\":\"inbox.closed\",\"binary_pid\":101".to_owned(),
            "\"event\":\"inbox.lost\",\"binary_pid\":101,\"reason\":\"connect_failed\"".to_owned(),
        ];
        assert_eq!(status, ExitCode::SUCCESS);
        assert_eq!(
            events,
            expected_events
                .iter()
                .zip(0_u64..)
                .map(|(rest, n)| trace_line(n, rest))
                .collect::<String>()
        );
        assert_eq!(
            written.lines().skip(1).collect::<Vec<&str>>(),
            [
                "attested permission class: bypass",
                "frame from \"an\\\"a\" dropped: the harness's envelope rebuild alters a sender name that is empty or holds anything but visible ASCII without \" < >",
                "/run/user/1000/estafette-1000/101.sock: cannot connect to the inbox socket: connection refused",
            ]
        );
    }

    #[derive(Clone, Copy)]
    enum SessionSocketFailure {
        RefusesTheConnection,
        ClosesAfterTheAuthLine,
    }

    struct ClosedAfterTheAuthLine {
        flushed_lines: usize,
    }

    impl Read for ClosedAfterTheAuthLine {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for ClosedAfterTheAuthLine {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.flushed_lines == 0 {
                Ok(bytes.len())
            } else {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed_lines = self.flushed_lines.saturating_add(1);
            Ok(())
        }
    }

    struct FailingSessionSocket {
        failure: SessionSocketFailure,
        connection_attempts: Arc<Mutex<usize>>,
    }

    impl InboxConnector for FailingSessionSocket {
        type Stream = ClosedAfterTheAuthLine;

        fn connect(&mut self) -> io::Result<ClosedAfterTheAuthLine> {
            {
                let mut attempts = lock(&self.connection_attempts);
                *attempts = attempts.saturating_add(1);
            }
            match self.failure {
                SessionSocketFailure::RefusesTheConnection => {
                    Err(io::Error::from(io::ErrorKind::ConnectionRefused))
                }
                SessionSocketFailure::ClosesAfterTheAuthLine => {
                    Ok(ClosedAfterTheAuthLine { flushed_lines: 0 })
                }
            }
        }
    }

    struct RunOverAFailingSessionSocket {
        events: String,
        count_file_texts: Vec<String>,
        since_lines: Vec<String>,
        connection_attempts: usize,
    }

    fn run_two_frames_over_a_session_socket_that(
        failure: SessionSocketFailure,
        label: &str,
    ) -> RunOverAFailingSessionSocket {
        let probe = TraceProbe::new(label);
        let session_files = files_of_live_session("bypassPermissions");
        let writes = Arc::clone(&session_files.writes);
        let connection_attempts = Arc::new(Mutex::new(0));
        let since_lines = SinceLines::default();
        let frames = [
            scripted_frame_line(1, "ana", "one"),
            scripted_frame_line(2, "bo", "two"),
        ]
        .concat();
        let (status, _written, events) = run_scripted_session(
            &probe,
            ScriptedSession {
                session_connector: FailingSessionSocket {
                    failure,
                    connection_attempts: Arc::clone(&connection_attempts),
                },
                session_token: "token".to_owned(),
                runtime_directory: PathBuf::from("/run/user/1000"),
                session_files,
                inbox_connections: vec![
                    ScriptedInboxConnection {
                        replies: frames,
                        fails_after_replies: true,
                    },
                    ScriptedInboxConnection {
                        replies: String::new(),
                        fails_after_replies: false,
                    },
                ],
                since_lines: Arc::clone(&since_lines),
            },
        );
        assert_eq!(status, ExitCode::SUCCESS);
        let count_file_texts = written_counts(&writes);
        let since_lines = lock(&since_lines).clone();
        let connection_attempts = *lock(&connection_attempts);
        RunOverAFailingSessionSocket {
            events,
            count_file_texts,
            since_lines,
            connection_attempts,
        }
    }

    fn doctor_dropped_frames_line_for(count_file_text: &str) -> String {
        let mut files = StandInFiles::default();
        files.texts.insert(
            dropped_frames_path(&PathBuf::from(TEST_TEMPORARY_DIR), HARNESS_PID),
            Ok(count_file_text.to_owned()),
        );
        let environment = Environment {
            temporary_dir: PathBuf::from(TEST_TEMPORARY_DIR),
            ..doctor_environment()
        };
        let mut output = Vec::new();
        run_doctor_command(
            &environment,
            &files,
            &FixedProcessTable(vec![
                ProcessEntry {
                    pid: environment.own_pid,
                    parent_pid: HARNESS_PID,
                    executable: Some("/opt/bin/estafette-bridge-claude".to_owned()),
                    arguments: vec!["doctor".to_owned()],
                },
                bridge_process(700, &HARNESS_PID.to_string()),
            ]),
            &mut output,
        )
        .expect("the doctor runs");
        String::from_utf8(output)
            .expect("utf8 report")
            .lines()
            .last()
            .expect("a report line")
            .to_owned()
    }

    fn assert_two_frames_counted_as_unwritten_notices_and_never_injected_again(
        run: &RunOverAFailingSessionSocket,
        unwritten_notice_event: &str,
    ) {
        let notice_events: Vec<&str> = run
            .events
            .lines()
            .filter(|line| line.contains("\"event\":\"notice."))
            .collect();
        assert_eq!(notice_events.len(), 2, "{}", run.events);
        for (line, seq) in notice_events.iter().zip(1_u64..) {
            assert!(
                line.contains(&format!(
                    "\"event\":\"{unwritten_notice_event}\",\"message\":\"id-{seq}\",\"frame\":{seq}"
                )),
                "{line}"
            );
        }
        assert!(!run.events.contains("frame.dropped_by_rate_limit"));
        assert_eq!(run.count_file_texts, ["0 0 0", "0 0 1", "0 0 2"]);
        assert_eq!(
            doctor_dropped_frames_line_for(
                run.count_file_texts.last().expect("a count was written")
            ),
            "dropped frames: 2 (rate limit 0, sender name refused 0, notice not written 2)"
        );
        assert_eq!(run.since_lines, ["{\"since\":0}\n", "{\"since\":2}\n"]);
        assert_eq!(run.connection_attempts, 2);
    }

    #[test]
    fn spec_3_12_step_4_frames_whose_notice_cannot_connect_to_the_session_count_as_dropped_with_a_reason_of_their_own_and_are_not_injected_again()
     {
        let run = run_two_frames_over_a_session_socket_that(
            SessionSocketFailure::RefusesTheConnection,
            "refused-session-socket",
        );

        assert_two_frames_counted_as_unwritten_notices_and_never_injected_again(
            &run,
            "notice.connect_failed",
        );
    }

    #[test]
    fn spec_3_12_step_4_frames_whose_notice_write_fails_count_as_dropped_with_a_reason_of_their_own_and_are_not_injected_again()
     {
        let run = run_two_frames_over_a_session_socket_that(
            SessionSocketFailure::ClosesAfterTheAuthLine,
            "closing-session-socket",
        );

        assert_two_frames_counted_as_unwritten_notices_and_never_injected_again(
            &run,
            "notice.write_failed",
        );
    }

    const MARKER: &str = "PLANTED-MARKER-7f3a";

    struct CapturedSessionStream(Arc<Mutex<Vec<u8>>>);

    impl Read for CapturedSessionStream {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for CapturedSessionStream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            lock(&self.0).extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct CapturingSessionSocket(Arc<Mutex<Vec<u8>>>);

    impl InboxConnector for CapturingSessionSocket {
        type Stream = CapturedSessionStream;

        fn connect(&mut self) -> io::Result<CapturedSessionStream> {
            Ok(CapturedSessionStream(Arc::clone(&self.0)))
        }
    }

    #[test]
    fn observability_decision_6_a_marker_planted_in_a_frame_body_the_session_token_and_the_inbox_socket_path_never_reaches_the_events_file()
     {
        let probe = TraceProbe::new("markers");
        let session_lines: Arc<Mutex<Vec<u8>>> = Arc::default();

        let (status, written, events) = run_scripted_session(
            &probe,
            ScriptedSession {
                session_connector: CapturingSessionSocket(Arc::clone(&session_lines)),
                session_token: format!("{MARKER}-token"),
                runtime_directory: PathBuf::from(format!("/run/{MARKER}-runtime")),
                session_files: files_of_live_session("bypassPermissions"),
                inbox_connections: one_inbox_connection_replying(scripted_frame_line(
                    1,
                    "ana",
                    &format!("{MARKER}-body"),
                )),
                since_lines: SinceLines::default(),
            },
        );

        let session_text = String::from_utf8(lock(&session_lines).clone()).expect("utf8 lines");
        assert_eq!(status, ExitCode::SUCCESS);
        assert!(
            session_text.contains(&format!("{MARKER}-token")),
            "{session_text}"
        );
        assert!(
            session_text.contains(&format!("{MARKER}-body")),
            "{session_text}"
        );
        assert!(written.contains(&format!("{MARKER}-runtime")), "{written}");
        assert!(events.contains("\"event\":\"notice.written\""), "{events}");
        assert!(events.contains("\"event\":\"inbox.lost\""), "{events}");
        assert!(!events.contains(MARKER), "{events}");
    }

    #[test]
    fn observability_decision_10_each_class_read_names_whether_it_came_from_the_transcript_end_the_whole_transcript_or_the_class_kept_from_before()
     {
        let probe = TraceProbe::new("class-source");
        let mut session_files = files_of_live_session("bypassPermissions");
        session_files.tails.insert(
            PathBuf::from(TRANSCRIPT_FILE),
            "{\"type\":\"user\"}\n".to_owned(),
        );

        let (_status, _written, events) = run_scripted_session(
            &probe,
            ScriptedSession {
                session_connector: WritableSessionSocket,
                session_token: "token".to_owned(),
                runtime_directory: PathBuf::from("/run/user/1000"),
                session_files,
                inbox_connections: one_inbox_connection_replying(
                    [
                        scripted_frame_line(1, "ana", "one"),
                        scripted_frame_line(2, "ana", "two"),
                    ]
                    .concat(),
                ),
                since_lines: SinceLines::default(),
            },
        );

        let sources: Vec<&str> = events
            .lines()
            .filter(|line| line.contains("\"event\":\"permission_class.read\""))
            .filter_map(|line| line.split("\"source\":\"").nth(1))
            .filter_map(|rest| rest.split('"').next())
            .collect();
        assert_eq!(
            sources,
            ["whole_transcript", "kept_from_before", "kept_from_before"]
        );
    }
}
