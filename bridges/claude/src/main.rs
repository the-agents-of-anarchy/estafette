use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
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

fn non_empty_variable(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

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
            Self::ProcessTable(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for CommandError {}

fn runtime_inputs(environment: &Environment) -> Result<RuntimeDirectoryInputs, CommandError> {
    let home = environment
        .home
        .clone()
        .ok_or(CommandError::MissingVariable(HOME_VARIABLE))?;
    let user_id = fs::metadata(&home)
        .map_err(|source| CommandError::HomeOwnerUnreadable {
            home: home.clone(),
            source,
        })?
        .uid();
    Ok(RuntimeDirectoryInputs {
        xdg_runtime_dir: environment.xdg_runtime_dir.clone(),
        temporary_dir: Some(environment.temporary_dir.clone()),
        user_id,
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn write_dropped_frames(path: &Path, count: u64) -> io::Result<()> {
    let staging = path.with_extension(STAGING_EXTENSION);
    fs::write(&staging, count.to_string())?;
    fs::rename(staging, path)
}

struct BridgeSession<S: InboxConnector> {
    notifier: Mutex<SessionNotifier<S>>,
    followed_sockets: Mutex<BTreeSet<PathBuf>>,
    dropped_frames_file: PathBuf,
    started: Instant,
    wait: fn(Duration),
}

impl<S: InboxConnector> BridgeSession<S> {
    fn deliver(&self, frame: &InboxFrame) {
        let mut notifier = lock(&self.notifier);
        let dropped_before = notifier.dropped_frames();
        if let Err(error) = notifier.notify(frame, self.started.elapsed()) {
            eprintln!("{error}");
        }
        let dropped_after = notifier.dropped_frames();
        drop(notifier);
        if dropped_after != dropped_before {
            report_write_failure(write_dropped_frames(
                &self.dropped_frames_file,
                dropped_after,
            ));
        }
    }
}

fn report_write_failure(outcome: io::Result<()>) {
    if let Err(error) = outcome {
        eprintln!("cannot write the dropped-frame count: {error}");
    }
}

fn follow_binary_socket<S, B>(
    session: &Arc<BridgeSession<S>>,
    socket_path: PathBuf,
    mut connector: B,
) where
    S: InboxConnector + Send + 'static,
    B: InboxConnector + Send + 'static,
{
    let session = Arc::clone(session);
    thread::spawn(move || {
        let wait = session.wait;
        let outcome = follow_inbox(&mut connector, &mut |pause| wait(pause), &mut |frame| {
            session.deliver(frame);
        });
        let Err(error) = outcome;
        eprintln!("{}: {error}", socket_path.display());
        lock(&session.followed_sockets).remove(&socket_path);
    });
}

#[derive(Debug, Eq, PartialEq)]
enum BridgeOutcome {
    HarnessExited,
    AlreadyServed { bridge_pid: u32 },
}

struct BridgeEffects<S, C> {
    own_pid: u32,
    started: Instant,
    runtime_inputs: RuntimeDirectoryInputs,
    temporary_dir: PathBuf,
    session_connector: S,
    session_token: String,
    connect_binary_socket: C,
    wait: fn(Duration),
}

type ProductionBridgeEffects =
    BridgeEffects<UnixSocketConnector, fn(PathBuf) -> UnixSocketConnector>;

fn bridge_effects(environment: &Environment) -> Result<ProductionBridgeEffects, CommandError> {
    let socket = environment
        .messaging_socket
        .clone()
        .ok_or(CommandError::MissingVariable(MESSAGING_SOCKET_VARIABLE))?;
    let session_token = environment
        .messaging_token
        .clone()
        .ok_or(CommandError::MissingVariable(MESSAGING_TOKEN_VARIABLE))?;
    Ok(BridgeEffects {
        own_pid: environment.own_pid,
        started: environment.started,
        runtime_inputs: runtime_inputs(environment)?,
        temporary_dir: environment.temporary_dir.clone(),
        session_connector: UnixSocketConnector::new(socket),
        session_token,
        connect_binary_socket: UnixSocketConnector::new,
        wait: thread::sleep,
    })
}

fn run_bridge<S, B>(
    harness_pid: u32,
    effects: BridgeEffects<S, impl Fn(PathBuf) -> B>,
    processes: &impl ProcessTable,
) -> Result<BridgeOutcome, CommandError>
where
    S: InboxConnector + Send + 'static,
    B: InboxConnector + Send + 'static,
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
        started: effects.started,
        wait,
    });
    report_write_failure(write_dropped_frames(&session.dropped_frames_file, 0));
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

fn read_settings_file(path: PathBuf) -> Result<Option<SettingsFile>, CommandError> {
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(SettingsFile { path, text })),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CommandError::SettingsUnreadable { path, source }),
    }
}

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

fn is_visible_settings_file(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some(SETTINGS_EXTENSION)
        && !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| name.starts_with(HIDDEN_FILE_PREFIX))
}

fn managed_settings_paths(directory: &Path, unreadable: &mut Vec<PathBuf>) -> Vec<PathBuf> {
    let drop_in_directory = directory.join(MANAGED_DROP_IN_DIRECTORY);
    let mut paths = vec![directory.join(MANAGED_SETTINGS_FILE)];
    match fs::read_dir(&drop_in_directory) {
        Ok(listing) => {
            for entry in listing {
                match entry {
                    Ok(entry) if is_visible_settings_file(&entry.path()) => {
                        paths.push(entry.path());
                    }
                    Ok(_) => {}
                    Err(_) => unreadable.push(drop_in_directory.clone()),
                }
            }
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {}
        Err(_) => unreadable.push(drop_in_directory),
    }
    paths.sort();
    paths
}

fn read_managed_settings(directory: &Path) -> ManagedSettings {
    let mut managed = ManagedSettings::default();
    for path in managed_settings_paths(directory, &mut managed.unreadable) {
        match fs::read_to_string(&path) {
            Ok(text) => managed.files.push(SettingsFile { path, text }),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(_) => managed.unreadable.push(path),
        }
    }
    managed
}

fn messaging_socket_state(environment: &Environment) -> MessagingSocket {
    match &environment.messaging_socket {
        None => MessagingSocket::Unset,
        Some(path) if path.exists() => MessagingSocket::Present(path.clone()),
        Some(path) => MessagingSocket::Missing(path.clone()),
    }
}

fn run_doctor_command(environment: &Environment) -> Result<bool, CommandError> {
    let mut settings_files: Vec<SettingsFile> =
        settings_file_paths(environment, &environment.working_dir)
            .into_iter()
            .map(read_settings_file)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
    let managed = read_managed_settings(Path::new(MANAGED_SETTINGS_DIRECTORY));
    settings_files.extend(managed.files);
    let inputs = DoctorInputs {
        own_pid: environment.own_pid,
        messaging_socket: messaging_socket_state(environment),
        settings_files,
        unreadable_settings: managed.unreadable,
        temporary_dir: environment.temporary_dir.clone(),
        runtime_inputs: runtime_inputs(environment)?,
    };
    let report = run_doctor(&inputs, &PsProcessTable, &|path: &Path| {
        fs::read_to_string(path).ok()
    })
    .map_err(CommandError::ProcessTable)?;
    println!("{report}");
    Ok(report.all_passed())
}

fn exit_code_of(outcome: Result<bool, CommandError>) -> ExitCode {
    match outcome {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(EXIT_CHECK_FAILED),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let environment = match read_environment() {
        Ok(environment) => environment,
        Err(error) => return exit_code_of(Err(error)),
    };
    match parse_command(&arguments) {
        Some(Command::Run { harness_pid }) => exit_code_of(
            bridge_effects(&environment)
                .and_then(|effects| run_bridge(harness_pid, effects, &PsProcessTable))
                .map(|outcome| {
                    if let BridgeOutcome::AlreadyServed { bridge_pid } = outcome {
                        eprintln!("bridge {bridge_pid} already serves process {harness_pid}");
                    }
                    true
                }),
        ),
        Some(Command::Doctor) => exit_code_of(run_doctor_command(&environment)),
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
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

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

    struct HeldConnector(Arc<Mutex<mpsc::Receiver<()>>>);

    impl InboxConnector for HeldConnector {
        type Stream = UnixStream;

        fn connect(&mut self) -> io::Result<UnixStream> {
            let released = lock(&self.0).recv();
            Err(io::Error::other(format!("{released:?}")))
        }
    }

    struct Wiring {
        effects: BridgeEffects<HeldConnector, Box<dyn Fn(PathBuf) -> HeldConnector>>,
        followed: Arc<Mutex<Vec<PathBuf>>>,
        release: mpsc::Sender<()>,
        scratch: PathBuf,
    }

    fn wiring(label: &str) -> Wiring {
        let (release, held) = mpsc::channel();
        let held = Arc::new(Mutex::new(held));
        let followed = Arc::new(Mutex::new(Vec::new()));
        let scratch = scratch_directory(label);
        let recorded = Arc::clone(&followed);
        let connector_held = Arc::clone(&held);
        Wiring {
            effects: BridgeEffects {
                own_pid: 900,
                started: Instant::now(),
                runtime_inputs: RuntimeDirectoryInputs {
                    xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
                    temporary_dir: None,
                    user_id: 1000,
                },
                temporary_dir: scratch.clone(),
                session_connector: HeldConnector(Arc::clone(&held)),
                session_token: "token".to_owned(),
                connect_binary_socket: Box::new(move |socket_path| {
                    lock(&recorded).push(socket_path);
                    HeldConnector(Arc::clone(&connector_held))
                }),
                wait: |_| {},
            },
            followed,
            release,
            scratch,
        }
    }

    #[test]
    fn spec_3_12_a_second_start_for_the_same_harness_leaves_the_running_bridge_alone() {
        let running = FixedProcessTable(vec![
            bridge_process(900, "100"),
            bridge_process(700, "100"),
            bridge_process(701, "999"),
        ]);
        let wiring = wiring("guard");
        assert_eq!(
            run_bridge(100, wiring.effects, &running).expect("guarded start"),
            BridgeOutcome::AlreadyServed { bridge_pid: 700 }
        );
        assert_eq!(lock(&wiring.followed).len(), 0);
        fs::remove_dir_all(&wiring.scratch).expect("cleanup");
    }

    #[test]
    fn spec_3_12_the_bridge_needs_the_session_socket_and_token_before_it_starts() {
        assert!(matches!(
            bridge_effects(&bare_environment()),
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
        let wiring = wiring("followers");
        assert_eq!(
            run_bridge(100, wiring.effects, &polls).expect("bridge runs"),
            BridgeOutcome::HarnessExited
        );
        assert_eq!(
            *lock(&wiring.followed),
            [
                PathBuf::from("/run/user/1000/estafette-1000/101.sock"),
                PathBuf::from("/run/user/1000/estafette-1000/102.sock"),
            ]
        );
        assert_eq!(
            fs::read_to_string(dropped_frames_path(&wiring.scratch, 100)).expect("count file"),
            "0"
        );
        drop(wiring.release);
        fs::remove_dir_all(&wiring.scratch).expect("cleanup");
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
        let managed = read_managed_settings(&directory);
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
        let managed = read_managed_settings(&directory);
        assert_eq!(managed.unreadable, [unreadable]);
        assert_eq!(managed.files.len(), 0);
        let absent = read_managed_settings(&directory.join("missing"));
        assert_eq!((absent.files.len(), absent.unreadable.len()), (0, 0));
        fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[test]
    fn spec_3_12_a_failed_check_ends_the_doctor_with_exit_3_and_other_outcomes_with_0_and_1() {
        assert_eq!(exit_code_of(Ok(true)), ExitCode::SUCCESS);
        assert_eq!(exit_code_of(Ok(false)), ExitCode::from(3));
        assert_eq!(
            exit_code_of(Err(CommandError::MissingVariable(HOME_VARIABLE))),
            ExitCode::from(1)
        );
    }
}
