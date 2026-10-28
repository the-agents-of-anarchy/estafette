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
use std::time::Instant;

use estafette_bridge_claude::discover::{
    ProcessTable, ProcessTableError, PsProcessTable, RuntimeDirectoryInputs, follow_binary_sockets,
};
use estafette_bridge_claude::doctor::{
    DoctorInputs, MESSAGING_SOCKET_VARIABLE, MessagingSocket, RUN_SUBCOMMAND, SettingsFile,
    dropped_frames_path, run_doctor, running_bridge_for,
};
use estafette_bridge_claude::inbox_client::{InboxFrame, UnixSocketConnector, follow_inbox};
use estafette_bridge_claude::inject::SessionNotifier;

const DOCTOR_SUBCOMMAND: &str = "doctor";
const PROJECT_DIR_FLAG: &str = "--project-dir";
const USAGE: &str =
    "usage: estafette-bridge-claude run <harness-pid> | doctor [--project-dir <dir>]";
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
    Doctor { project_dir: Option<PathBuf> },
}

fn parse_command(arguments: &[String]) -> Option<Command> {
    match arguments {
        [subcommand, harness_pid] if subcommand == RUN_SUBCOMMAND => Some(Command::Run {
            harness_pid: harness_pid.parse().ok()?,
        }),
        [subcommand] if subcommand == DOCTOR_SUBCOMMAND => {
            Some(Command::Doctor { project_dir: None })
        }
        [subcommand, flag, directory]
            if subcommand == DOCTOR_SUBCOMMAND && flag == PROJECT_DIR_FLAG =>
        {
            Some(Command::Doctor {
                project_dir: Some(PathBuf::from(directory)),
            })
        }
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
}

fn non_empty_variable(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn read_environment() -> Environment {
    Environment {
        messaging_socket: non_empty_variable(MESSAGING_SOCKET_VARIABLE).map(PathBuf::from),
        messaging_token: non_empty_variable(MESSAGING_TOKEN_VARIABLE)
            .and_then(|token| token.into_string().ok()),
        home: non_empty_variable(HOME_VARIABLE).map(PathBuf::from),
        claude_config_dir: non_empty_variable(CLAUDE_CONFIG_DIR_VARIABLE).map(PathBuf::from),
        xdg_runtime_dir: non_empty_variable(RUNTIME_DIRECTORY_VARIABLE).map(PathBuf::from),
        temporary_dir: non_empty_variable(TEMPORARY_DIRECTORY_VARIABLE)
            .map_or_else(|| PathBuf::from(SYSTEM_TEMPORARY_DIRECTORY), PathBuf::from),
    }
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

type SharedNotifier = Arc<Mutex<SessionNotifier<UnixSocketConnector>>>;

struct BridgeSession {
    notifier: SharedNotifier,
    followed_sockets: Arc<Mutex<BTreeSet<PathBuf>>>,
    dropped_frames_file: PathBuf,
    started: Instant,
}

impl BridgeSession {
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

fn follow_binary_socket(session: &Arc<BridgeSession>, socket_path: PathBuf) {
    let session = Arc::clone(session);
    thread::spawn(move || {
        let mut connector = UnixSocketConnector::new(socket_path.clone());
        let outcome = follow_inbox(&mut connector, &mut thread::sleep, &mut |frame| {
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

fn run_bridge(
    harness_pid: u32,
    own_pid: u32,
    environment: &Environment,
    processes: &impl ProcessTable,
) -> Result<BridgeOutcome, CommandError> {
    let entries = processes.list().map_err(CommandError::ProcessTable)?;
    if let Some(bridge_pid) = running_bridge_for(harness_pid, own_pid, &entries) {
        return Ok(BridgeOutcome::AlreadyServed { bridge_pid });
    }
    let socket = environment
        .messaging_socket
        .clone()
        .ok_or(CommandError::MissingVariable(MESSAGING_SOCKET_VARIABLE))?;
    let token = environment
        .messaging_token
        .clone()
        .ok_or(CommandError::MissingVariable(MESSAGING_TOKEN_VARIABLE))?;
    let runtime_inputs = runtime_inputs(environment)?;
    let session = Arc::new(BridgeSession {
        notifier: Arc::new(Mutex::new(SessionNotifier::new(
            UnixSocketConnector::new(socket),
            token,
        ))),
        followed_sockets: Arc::default(),
        dropped_frames_file: dropped_frames_path(&environment.temporary_dir, harness_pid),
        started: Instant::now(),
    });
    report_write_failure(write_dropped_frames(&session.dropped_frames_file, 0));
    follow_binary_sockets(
        harness_pid,
        &runtime_inputs,
        processes,
        &mut thread::sleep,
        &mut |sockets| {
            for socket_path in sockets {
                if lock(&session.followed_sockets).insert(socket_path.clone()) {
                    follow_binary_socket(&session, socket_path.clone());
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

fn run_doctor_command(
    project_dir: Option<PathBuf>,
    environment: &Environment,
) -> Result<bool, CommandError> {
    let project_dir = match project_dir {
        Some(directory) => directory,
        None => std::env::current_dir().map_err(CommandError::WorkingDirectoryUnreadable)?,
    };
    let mut settings_files: Vec<SettingsFile> = settings_file_paths(environment, &project_dir)
        .into_iter()
        .map(read_settings_file)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect();
    let managed = read_managed_settings(Path::new(MANAGED_SETTINGS_DIRECTORY));
    settings_files.extend(managed.files);
    let inputs = DoctorInputs {
        own_pid: std::process::id(),
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
    let environment = read_environment();
    match parse_command(&arguments) {
        Some(Command::Run { harness_pid }) => exit_code_of(
            run_bridge(
                harness_pid,
                std::process::id(),
                &environment,
                &PsProcessTable,
            )
            .map(|outcome| {
                if let BridgeOutcome::AlreadyServed { bridge_pid } = outcome {
                    eprintln!("bridge {bridge_pid} already serves process {harness_pid}");
                }
                true
            }),
        ),
        Some(Command::Doctor { project_dir }) => {
            exit_code_of(run_doctor_command(project_dir, &environment))
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
    fn spec_3_12_doctor_takes_an_optional_project_directory() {
        assert_eq!(
            parse_command(&arguments(&["doctor"])),
            Some(Command::Doctor { project_dir: None })
        );
        assert_eq!(
            parse_command(&arguments(&["doctor", "--project-dir", "/work"])),
            Some(Command::Doctor {
                project_dir: Some(PathBuf::from("/work"))
            })
        );
    }

    #[test]
    fn spec_3_12_anything_else_is_refused_with_the_usage() {
        for refused in [
            &[][..],
            &["run"],
            &["run", "not-a-pid"],
            &["run", "-5"],
            &["doctor", "--project-dir"],
            &["doctor", "--other", "/work"],
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

    #[test]
    fn spec_3_12_a_second_start_for_the_same_harness_leaves_the_running_bridge_alone() {
        let running = FixedProcessTable(vec![
            bridge_process(900, "100"),
            bridge_process(700, "100"),
            bridge_process(701, "999"),
        ]);
        assert_eq!(
            run_bridge(100, 900, &bare_environment(), &running).expect("guarded start"),
            BridgeOutcome::AlreadyServed { bridge_pid: 700 }
        );
        let only_itself_and_another_session =
            FixedProcessTable(vec![bridge_process(900, "100"), bridge_process(701, "999")]);
        assert!(matches!(
            run_bridge(
                100,
                900,
                &bare_environment(),
                &only_itself_and_another_session
            ),
            Err(CommandError::MissingVariable(MESSAGING_SOCKET_VARIABLE))
        ));
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
}
