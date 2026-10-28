use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use estafette_bridge_claude::discover::{
    ProcessTable, ProcessTableError, PsProcessTable, RuntimeDirectoryInputs, follow_binary_sockets,
};
use estafette_bridge_claude::doctor::{
    DoctorInputs, MESSAGING_SOCKET_VARIABLE, MessagingSocket, RUN_SUBCOMMAND, SettingsFile,
    dropped_frames_path, run_doctor, running_bridge_for,
};
use estafette_bridge_claude::inbox_client::{
    InboxConnector, InboxFrame, UnixSocketConnector, follow_inbox,
};
use estafette_bridge_claude::inject::SessionNotifier;

const DOCTOR_SUBCOMMAND: &str = "doctor";
const USAGE: &str = "usage: estafette-bridge-claude run <harness-pid> | doctor";
const EXIT_FAILURE: u8 = 1;
const EXIT_USAGE: u8 = 2;
const EXIT_CHECK_FAILED: u8 = 3;
const MESSAGING_TOKEN_VARIABLE: &str = "CLAUDE_CODE_MESSAGING_TOKEN";
const HOME_VARIABLE: &str = "HOME";
const CLAUDE_CONFIG_DIR_VARIABLE: &str = "CLAUDE_CONFIG_DIR";
const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
const TEMPORARY_DIRECTORY_VARIABLE: &str = "TMPDIR";
const SYSTEM_TEMPORARY_DIRECTORY: &str = "/tmp";
const CLAUDE_DIRECTORY: &str = ".claude";
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
const STAGING_EXTENSION: &str = "staging";

#[derive(Debug, Eq, PartialEq)]
enum Command {
    Run { harness_pid: u32 },
    Doctor,
}

/// Reads the subcommand and its arguments from the command line, refusing any other form.
fn parse_command(arguments: &[String]) -> Option<Command> {
    match arguments {
        [subcommand, harness_pid] if subcommand == RUN_SUBCOMMAND => Some(Command::Run {
            harness_pid: harness_pid.parse().ok()?,
        }),
        [subcommand] if subcommand == DOCTOR_SUBCOMMAND => Some(Command::Doctor),
        _ => None,
    }
}

struct Environment {
    messaging_socket: Option<PathBuf>,
    messaging_token: Option<String>,
    home: Option<PathBuf>,
    claude_config_dir: Option<PathBuf>,
    xdg_runtime_dir: Option<PathBuf>,
    temporary_dir: PathBuf,
    own_pid: u32,
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
        claude_config_dir: non_empty_variable(CLAUDE_CONFIG_DIR_VARIABLE).map(PathBuf::from),
        xdg_runtime_dir: non_empty_variable(RUNTIME_DIRECTORY_VARIABLE).map(PathBuf::from),
        temporary_dir: non_empty_variable(TEMPORARY_DIRECTORY_VARIABLE)
            .map_or_else(|| PathBuf::from(SYSTEM_TEMPORARY_DIRECTORY), PathBuf::from),
        own_pid: std::process::id(),
        working_dir: std::env::current_dir().map_err(CommandError::WorkingDirectoryUnreadable)?,
        started: Instant::now(),
    })
}

#[derive(Debug)]
enum CommandError {
    MissingVariable(&'static str),
    HomeOwnerUnreadable { home: PathBuf, source: io::Error },
    SettingsUnreadable { path: PathBuf, source: io::Error },
    WorkingDirectoryUnreadable(io::Error),
    ReportUnwritable(io::Error),
    ProcessTable(ProcessTableError),
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingVariable(name) => write!(formatter, "{name} is not set"),
            Self::HomeOwnerUnreadable { home, source } => write!(
                formatter,
                "cannot read the owner of {}: {source}",
                home.display()
            ),
            Self::SettingsUnreadable { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::WorkingDirectoryUnreadable(source) => {
                write!(formatter, "cannot read the working directory: {source}")
            }
            Self::ReportUnwritable(source) => {
                write!(formatter, "cannot write the report: {source}")
            }
            Self::ProcessTable(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for CommandError {}

/// The file system reads the commands make, handed in from the entry.
trait Files {
    /// Reads a whole file as text.
    fn read_text(&self, path: &Path) -> io::Result<String>;
    /// Lists the paths inside a directory.
    fn list_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    /// Tells whether anything exists at the path.
    fn exists(&self, path: &Path) -> bool;
    /// Gives the numeric id of the user owning the path.
    fn owner_user_id(&self, path: &Path) -> io::Result<u32>;
    /// Writes a file by staging it beside the final name and renaming it over that name.
    fn write_text_atomically(&self, path: &Path, text: &str) -> io::Result<()>;
}

struct OperatingSystemFiles;

impl Files for OperatingSystemFiles {
    fn read_text(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn list_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn owner_user_id(&self, path: &Path) -> io::Result<u32> {
        Ok(fs::metadata(path)?.uid())
    }

    fn write_text_atomically(&self, path: &Path, text: &str) -> io::Result<()> {
        let staging = path.with_extension(STAGING_EXTENSION);
        fs::write(&staging, text)?;
        fs::rename(staging, path)
    }
}

/// Names the runtime directory inputs, taking the user id from the owner of the home directory.
fn runtime_inputs(
    environment: &Environment,
    files: &impl Files,
) -> Result<RuntimeDirectoryInputs, CommandError> {
    let home = environment
        .home
        .clone()
        .ok_or(CommandError::MissingVariable(HOME_VARIABLE))?;
    let user_id =
        files
            .owner_user_id(&home)
            .map_err(|source| CommandError::HomeOwnerUnreadable {
                home: home.clone(),
                source,
            })?;
    Ok(RuntimeDirectoryInputs {
        xdg_runtime_dir: environment.xdg_runtime_dir.clone(),
        temporary_dir: Some(environment.temporary_dir.clone()),
        user_id,
    })
}

/// Takes a mutex whether or not a panicking holder poisoned it, which the abort-on-panic release profile makes unreachable.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

type ErrorSink = Arc<dyn Fn(&str) + Send + Sync>;
type Elapsed = Arc<dyn Fn() -> Duration + Send + Sync>;
type Task = Box<dyn FnOnce() + Send>;
type Spawner = Arc<dyn Fn(Task) + Send + Sync>;

struct BridgeSession<S: InboxConnector, F: Files> {
    notifier: Mutex<SessionNotifier<S>>,
    followed_sockets: Mutex<BTreeSet<PathBuf>>,
    dropped_frames_file: PathBuf,
    files: F,
    elapsed: Elapsed,
    report_error: ErrorSink,
    spawn: Spawner,
    wait: fn(Duration),
}

impl<S: InboxConnector, F: Files> BridgeSession<S, F> {
    /// Injects one frame as a notice and records the dropped-frame count when it changed.
    fn deliver(&self, frame: &InboxFrame) {
        let mut notifier = lock(&self.notifier);
        let dropped_before = notifier.dropped_frames();
        if let Err(error) = notifier.notify(frame, (self.elapsed)()) {
            (self.report_error)(&error.to_string());
        }
        let dropped_after = notifier.dropped_frames();
        drop(notifier);
        if dropped_after != dropped_before {
            self.record_dropped_frames(dropped_after);
        }
    }

    /// Writes the dropped-frame count where the doctor reads it, reporting a failed write.
    fn record_dropped_frames(&self, count: u64) {
        if let Err(error) = self
            .files
            .write_text_atomically(&self.dropped_frames_file, &count.to_string())
        {
            (self.report_error)(&format!("cannot write the dropped-frame count: {error}"));
        }
    }
}

/// Starts a task that follows one binary inbox socket until its connection fails.
fn follow_binary_socket<S, B, F>(
    session: &Arc<BridgeSession<S, F>>,
    socket_path: PathBuf,
    mut connector: B,
) where
    S: InboxConnector + Send + 'static,
    B: InboxConnector + Send + 'static,
    F: Files + Send + Sync + 'static,
{
    let session = Arc::clone(session);
    let spawn = Arc::clone(&session.spawn);
    spawn(Box::new(move || {
        let wait = session.wait;
        let outcome = follow_inbox(&mut connector, &mut |pause| wait(pause), &mut |frame| {
            session.deliver(frame);
        });
        let Err(error) = outcome;
        (session.report_error)(&format!("{}: {error}", socket_path.display()));
        lock(&session.followed_sockets).remove(&socket_path);
    }));
}

#[derive(Debug, Eq, PartialEq)]
enum BridgeOutcome {
    HarnessExited,
    AlreadyServed { bridge_pid: u32 },
}

struct BridgeEffects<S, C, F> {
    own_pid: u32,
    runtime_inputs: RuntimeDirectoryInputs,
    temporary_dir: PathBuf,
    session_connector: S,
    session_token: String,
    connect_binary_socket: C,
    files: F,
    elapsed: Elapsed,
    report_error: ErrorSink,
    spawn: Spawner,
    wait: fn(Duration),
}

type ProductionBridgeEffects<F> =
    BridgeEffects<UnixSocketConnector, fn(PathBuf) -> UnixSocketConnector, F>;

/// Builds the production sockets, clock, error sink, spawner and pauses the bridge runs on from the environment.
fn bridge_effects<F: Files>(
    environment: &Environment,
    files: F,
) -> Result<ProductionBridgeEffects<F>, CommandError> {
    let socket = environment
        .messaging_socket
        .clone()
        .ok_or(CommandError::MissingVariable(MESSAGING_SOCKET_VARIABLE))?;
    let session_token = environment
        .messaging_token
        .clone()
        .ok_or(CommandError::MissingVariable(MESSAGING_TOKEN_VARIABLE))?;
    let started = environment.started;
    Ok(BridgeEffects {
        own_pid: environment.own_pid,
        runtime_inputs: runtime_inputs(environment, &files)?,
        temporary_dir: environment.temporary_dir.clone(),
        session_connector: UnixSocketConnector::new(socket),
        session_token,
        connect_binary_socket: UnixSocketConnector::new,
        files,
        elapsed: Arc::new(move || started.elapsed()),
        report_error: Arc::new(|message| eprintln!("{message}")),
        spawn: Arc::new(|task| {
            thread::spawn(task);
        }),
        wait: thread::sleep,
    })
}

/// Serves the harness process by following every binary socket under it until the harness exits, unless another bridge already serves it.
fn run_bridge<S, B, F>(
    harness_pid: u32,
    effects: BridgeEffects<S, impl Fn(PathBuf) -> B, F>,
    processes: &impl ProcessTable,
) -> Result<BridgeOutcome, CommandError>
where
    S: InboxConnector + Send + 'static,
    B: InboxConnector + Send + 'static,
    F: Files + Send + Sync + 'static,
{
    let entries = processes.list().map_err(CommandError::ProcessTable)?;
    if let Some(bridge_pid) = running_bridge_for(harness_pid, effects.own_pid, &entries) {
        return Ok(BridgeOutcome::AlreadyServed { bridge_pid });
    }
    let wait = effects.wait;
    let session = Arc::new(BridgeSession {
        notifier: Mutex::new(SessionNotifier::new(
            effects.session_connector,
            effects.session_token,
        )),
        followed_sockets: Mutex::default(),
        dropped_frames_file: dropped_frames_path(&effects.temporary_dir, harness_pid),
        files: effects.files,
        elapsed: effects.elapsed,
        report_error: effects.report_error,
        spawn: effects.spawn,
        wait,
    });
    session.record_dropped_frames(0);
    follow_binary_sockets(
        harness_pid,
        &effects.runtime_inputs,
        processes,
        &mut |pause| wait(pause),
        &mut |sockets| {
            for socket_path in sockets {
                if lock(&session.followed_sockets).insert(socket_path.clone()) {
                    let connector = (effects.connect_binary_socket)(socket_path.clone());
                    follow_binary_socket(&session, socket_path.clone(), connector);
                }
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

/// Lists the user, project and local settings files in the order Claude Code reads them.
fn settings_file_paths(environment: &Environment, project_dir: &Path) -> Vec<PathBuf> {
    let user_config_dir = environment.claude_config_dir.clone().or_else(|| {
        environment
            .home
            .as_ref()
            .map(|home| home.join(CLAUDE_DIRECTORY))
    });
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
        temporary_dir: environment.temporary_dir.clone(),
        runtime_inputs: runtime_inputs(environment, files)?,
    };
    let report = run_doctor(&inputs, processes, &|path: &Path| {
        files.read_text(path).ok()
    })
    .map_err(CommandError::ProcessTable)?;
    writeln!(output, "{report}").map_err(CommandError::ReportUnwritable)?;
    Ok(report.all_passed())
}

/// Maps a command outcome to its exit status, writing an error's message alone to the error output.
fn exit_code_of(outcome: Result<bool, CommandError>, errors: &mut impl Write) -> ExitCode {
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(EXIT_CHECK_FAILED),
        Err(error) => {
            drop(writeln!(errors, "{error}"));
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Runs the subcommand named on the command line and exits with its status.
fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut errors = std::io::stderr();
    let environment = match read_environment() {
        Ok(environment) => environment,
        Err(error) => return exit_code_of(Err(error), &mut errors),
    };
    match parse_command(&arguments) {
        Some(Command::Run { harness_pid }) => {
            let outcome = bridge_effects(&environment, OperatingSystemFiles)
                .and_then(|effects| run_bridge(harness_pid, effects, &PsProcessTable))
                .map(|outcome| {
                    if let BridgeOutcome::AlreadyServed { bridge_pid } = outcome {
                        eprintln!("bridge {bridge_pid} already serves process {harness_pid}");
                    }
                    true
                });
            exit_code_of(outcome, &mut errors)
        }
        Some(Command::Doctor) => {
            let outcome = run_doctor_command(
                &environment,
                &OperatingSystemFiles,
                &PsProcessTable,
                &mut std::io::stdout(),
            );
            exit_code_of(outcome, &mut errors)
        }
        None => {
            eprintln!("{USAGE}");
            ExitCode::from(EXIT_USAGE)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use estafette_bridge_claude::discover::ProcessEntry;
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::os::unix::net::UnixStream;

    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn spec_3_12_the_hook_command_line_starts_the_bridge_for_its_parent_pid() {
        assert_eq!(
            parse_command(&arguments(&["run", "4242"])),
            Some(Command::Run { harness_pid: 4242 })
        );
    }

    #[test]
    fn spec_3_12_doctor_takes_no_arguments() {
        assert_eq!(
            parse_command(&arguments(&["doctor"])),
            Some(Command::Doctor)
        );
    }

    #[test]
    fn spec_3_12_anything_else_is_refused_with_the_usage() {
        for refused in [
            &[][..],
            &["run"],
            &["run", "not-a-pid"],
            &["run", "-5"],
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
            claude_config_dir: None,
            xdg_runtime_dir: None,
            temporary_dir: PathBuf::from("/tmp"),
            own_pid: 900,
            working_dir: PathBuf::from("/work"),
            started: Instant::now(),
        }
    }

    fn bridge_process(pid: u32, harness_pid: &str) -> ProcessEntry {
        ProcessEntry {
            pid,
            parent_pid: 1,
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
                own_pid: 900,
                runtime_inputs: RuntimeDirectoryInputs {
                    xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
                    temporary_dir: None,
                    user_id: 1000,
                },
                temporary_dir: PathBuf::from(TEST_TEMPORARY_DIR),
                session_connector: DownConnector,
                session_token: "token".to_owned(),
                connect_binary_socket: Box::new(move |socket_path| {
                    lock(&recorded_paths).push(socket_path);
                    DownConnector
                }),
                files,
                elapsed: Arc::new(move || Duration::from_secs(*lock(&clock))),
                report_error: Arc::new(move |message| {
                    lock(&recorded_errors).push(message.to_owned());
                }),
                spawn: Arc::new(move |task| {
                    lock(&recorded_tasks).push(task);
                }),
                wait: |_| {},
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

    #[test]
    fn spec_3_12_a_second_start_for_the_same_harness_leaves_the_running_bridge_alone() {
        let running = FixedProcessTable(vec![
            bridge_process(900, "100"),
            bridge_process(700, "100"),
            bridge_process(701, "999"),
        ]);
        let wiring = wiring();
        assert_eq!(
            run_bridge(100, wiring.effects, &running).expect("guarded start"),
            BridgeOutcome::AlreadyServed { bridge_pid: 700 }
        );
        assert_eq!(lock(&wiring.spawned).len(), 0);
    }

    #[test]
    fn spec_3_12_the_bridge_needs_the_session_socket_and_token_before_it_starts() {
        assert!(matches!(
            bridge_effects(&bare_environment(), OperatingSystemFiles),
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
            pid,
            parent_pid: 100,
            executable: Some("/usr/bin/estafette".to_owned()),
            arguments: vec!["mcp".to_owned()],
        }
    }

    #[test]
    fn spec_3_12_the_bridge_follows_each_binary_socket_once_however_many_polls_see_it() {
        let harness = ProcessEntry {
            pid: 100,
            parent_pid: 1,
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
            run_bridge(100, wiring.effects, &polls).expect("bridge runs"),
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
        assert_eq!(written_counts(&wiring.writes), ["0"]);
    }

    #[test]
    fn spec_3_12_a_binary_socket_that_cannot_be_reached_is_reported_and_followed_again_later() {
        let harness = ProcessEntry {
            pid: 100,
            parent_pid: 1,
            executable: Some("/usr/bin/claude".to_owned()),
            arguments: Vec::new(),
        };
        let running = vec![harness, mcp_binary(101)];
        let polls = SnapshotSequence(RefCell::new(vec![running.clone(), running, Vec::new()]));
        let wiring = wiring();
        run_bridge(100, wiring.effects, &polls).expect("bridge runs");
        let task = lock(&wiring.spawned).pop().expect("one follower spawned");
        task();
        assert_eq!(
            *lock(&wiring.errors),
            ["/run/user/1000/estafette-1000/101.sock: cannot connect to the inbox socket: down"]
        );
    }

    fn frame_from(from: &str, body: &str) -> InboxFrame {
        InboxFrame {
            seq: 1,
            id: "id".to_owned(),
            from: from.to_owned(),
            body: body.to_owned(),
            sent_at: 0,
            received_at: 0,
        }
    }

    struct DeliveryRun {
        errors: Vec<String>,
        counts: Vec<String>,
    }

    fn deliver_eleven_frames(seconds_between_frames: u64) -> DeliveryRun {
        let wiring = wiring();
        let session = BridgeSession {
            notifier: Mutex::new(SessionNotifier::new(DownConnector, "token".to_owned())),
            followed_sockets: Mutex::default(),
            dropped_frames_file: PathBuf::from(DROPPED_FRAMES_FILE),
            files: StandInFiles::default(),
            elapsed: Arc::clone(&wiring.effects.elapsed),
            report_error: Arc::clone(&wiring.effects.report_error),
            spawn: Arc::clone(&wiring.effects.spawn),
            wait: |_| {},
        };
        for index in 0..11_u64 {
            *lock(&wiring.clock_seconds) = index.saturating_mul(seconds_between_frames);
            session.deliver(&frame_from("ana", &format!("body {index}")));
        }
        DeliveryRun {
            errors: lock(&wiring.errors).clone(),
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
        assert_eq!(within_one_window.counts, ["1"]);
        let across_windows = deliver_eleven_frames(61);
        assert_eq!(across_windows.errors.len(), 11);
        assert_eq!(across_windows.counts, Vec::<String>::new());
    }

    fn scratch_directory(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "estafette-bridge-claude-managed-{label}-{}",
            std::process::id()
        ));
        if directory.exists() {
            fs::remove_dir_all(&directory).expect("stale scratch");
        }
        fs::create_dir_all(directory.join(MANAGED_DROP_IN_DIRECTORY)).expect("scratch directory");
        directory
    }

    #[test]
    fn spec_3_12_managed_settings_are_the_file_and_the_visible_json_files_of_its_drop_in_directory()
    {
        let directory = scratch_directory("read");
        let drop_in = directory.join(MANAGED_DROP_IN_DIRECTORY);
        fs::write(directory.join(MANAGED_SETTINGS_FILE), "{\"a\":1}").expect("write");
        fs::write(drop_in.join("20-b.json"), "{\"b\":2}").expect("write");
        fs::write(drop_in.join("10-a.json"), "{\"c\":3}").expect("write");
        fs::write(drop_in.join(".hidden.json"), "{\"d\":4}").expect("write");
        fs::write(drop_in.join("notes.txt"), "e").expect("write");
        let managed = read_managed_settings(&OperatingSystemFiles, &directory);
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
        fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[test]
    fn spec_3_12_a_managed_settings_path_that_exists_but_cannot_be_read_is_reported_unreadable() {
        let directory = scratch_directory("unreadable");
        let unreadable = directory
            .join(MANAGED_DROP_IN_DIRECTORY)
            .join("30-bad.json");
        fs::create_dir(&unreadable).expect("a directory where a file is expected");
        let managed = read_managed_settings(&OperatingSystemFiles, &directory);
        assert_eq!(managed.unreadable, [unreadable]);
        assert_eq!(managed.files.len(), 0);
        let absent = read_managed_settings(&OperatingSystemFiles, &directory.join("missing"));
        assert_eq!((absent.files.len(), absent.unreadable.len()), (0, 0));
        fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[test]
    fn spec_3_12_a_failed_check_ends_the_doctor_with_exit_3_and_other_outcomes_with_0_and_1() {
        let mut errors = Vec::new();
        assert_eq!(exit_code_of(Ok(true), &mut errors), ExitCode::SUCCESS);
        assert_eq!(exit_code_of(Ok(false), &mut errors), ExitCode::from(3));
        assert_eq!(errors, b"");
        assert_eq!(
            exit_code_of(
                Err(CommandError::MissingVariable(HOME_VARIABLE)),
                &mut errors
            ),
            ExitCode::from(1)
        );
        assert_eq!(errors, b"HOME is not set\n");
    }

    #[derive(Default)]
    struct StandInFiles {
        texts: BTreeMap<PathBuf, Result<String, io::ErrorKind>>,
        directories: BTreeMap<PathBuf, Result<Vec<PathBuf>, io::ErrorKind>>,
        existing: BTreeSet<PathBuf>,
        reads: Mutex<Vec<PathBuf>>,
        writes: Arc<Mutex<Vec<(PathBuf, String)>>>,
    }

    impl Files for StandInFiles {
        fn read_text(&self, path: &Path) -> io::Result<String> {
            lock(&self.reads).push(path.to_owned());
            match self.texts.get(path) {
                Some(Ok(text)) => Ok(text.clone()),
                Some(Err(kind)) => Err(io::Error::from(*kind)),
                None => Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        }

        fn list_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            match self.directories.get(path) {
                Some(Ok(listing)) => Ok(listing.clone()),
                Some(Err(kind)) => Err(io::Error::from(*kind)),
                None => Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        }

        fn exists(&self, path: &Path) -> bool {
            self.existing.contains(path)
        }

        fn owner_user_id(&self, _path: &Path) -> io::Result<u32> {
            Ok(1000)
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
    fn spec_3_12_the_operating_system_writes_the_count_by_staging_then_renaming() {
        let directory = scratch_directory("atomic");
        let target = directory.join("count");
        OperatingSystemFiles
            .write_text_atomically(&target, "3")
            .expect("first write");
        OperatingSystemFiles
            .write_text_atomically(&target, "12")
            .expect("second write");
        assert_eq!(fs::read_to_string(&target).expect("count"), "12");
        assert!(!target.with_extension(STAGING_EXTENSION).exists());
        fs::remove_dir_all(&directory).expect("cleanup");
    }
}
