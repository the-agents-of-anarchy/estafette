use std::cell::Cell;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::discover::{
    ProcessEntry, ProcessTable, ProcessTableError, RuntimeDirectoryInputs, follow_binary_sockets,
};

const BRIDGE_EXECUTABLE_NAME: &str = "estafette-bridge-claude";
const DROPPED_FRAMES_SUFFIX: &str = ".dropped-frames";
pub const RUN_SUBCOMMAND: &str = "run";
pub const MESSAGING_SOCKET_VARIABLE: &str = "CLAUDE_CODE_MESSAGING_SOCKET";
const CROSS_SESSION_INBOUND_KEY: &str = "crossSessionInbound";
const SETTINGS_FLAG: &str = "--settings";
const COMMAND_LINE_SETTINGS_NAME: &str = "--settings on the harness command line";
const UNREADABLE_FILE_NOTE: &str = "(the file cannot be read)";
const REFUSE_VALUE: &str = "refuse";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Check {
    HookInstalled,
    BridgeRunning,
    SocketPathExists,
    NoRefuseApplies,
    BinaryProcessFound,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Verdict {
    Passed,
    Failed(String),
    Unverified(String),
}

#[derive(Debug, Eq, PartialEq)]
pub struct CheckResult {
    pub check: Check,
    pub verdict: Verdict,
}

#[derive(Debug)]
pub struct DoctorReport {
    pub results: Vec<CheckResult>,
    pub dropped_frames: Option<u64>,
}

impl DoctorReport {
    /// Tells whether every check passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        self.results
            .iter()
            .all(|result| result.verdict == Verdict::Passed)
    }
}

impl fmt::Display for Check {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::HookInstalled => "hook installed",
            Self::BridgeRunning => "bridge process runs for this session",
            Self::SocketPathExists => "socket path exists",
            Self::NoRefuseApplies => "no refuse applies",
            Self::BinaryProcessFound => "binary process found",
        })
    }
}

impl fmt::Display for DoctorReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for result in &self.results {
            match &result.verdict {
                Verdict::Passed => writeln!(formatter, "ok    {}", result.check)?,
                Verdict::Failed(reason) => {
                    writeln!(formatter, "FAIL  {}: {reason}", result.check)?;
                }
                Verdict::Unverified(reason) => {
                    writeln!(formatter, "UNVERIFIED {}: {reason}", result.check)?;
                }
            }
        }
        match self.dropped_frames {
            Some(count) => write!(formatter, "dropped frames: {count}"),
            None => write!(formatter, "dropped frames: unknown"),
        }
    }
}

pub struct SettingsFile {
    pub path: PathBuf,
    pub text: String,
}

pub enum MessagingSocket {
    Unset,
    Missing(PathBuf),
    Present(PathBuf),
}

pub struct DoctorInputs {
    pub own_pid: u32,
    pub messaging_socket: MessagingSocket,
    pub settings_files: Vec<SettingsFile>,
    pub unreadable_settings: Vec<PathBuf>,
    pub temporary_dir: PathBuf,
    pub runtime_inputs: RuntimeDirectoryInputs,
}

/// Names the file in which the bridge of this harness process keeps its count of dropped frames.
#[must_use]
pub fn dropped_frames_path(temporary_dir: &Path, harness_pid: u32) -> PathBuf {
    temporary_dir.join(format!(
        "{BRIDGE_EXECUTABLE_NAME}-{harness_pid}{DROPPED_FRAMES_SUFFIX}"
    ))
}

/// Judges the wiring of the bridge to this session from what the environment, the settings files, the process table and the bridge's dropped-frames file show.
/// # Errors
/// Returns the `ProcessTableError` of the failed process listing.
pub fn run_doctor(
    inputs: &DoctorInputs,
    processes: &impl ProcessTable,
    read_text_file: &impl Fn(&Path) -> Option<String>,
) -> Result<DoctorReport, ProcessTableError> {
    let entries = processes.list()?;
    let harness_pid = harness_of_bridge_for_session(inputs.own_pid, &entries);
    let command_line = settings_of_harness_command_line(harness_pid, &entries, read_text_file);
    Ok(DoctorReport {
        results: vec![
            CheckResult {
                check: Check::HookInstalled,
                verdict: judge_hook_installed(&inputs.settings_files),
            },
            CheckResult {
                check: Check::BridgeRunning,
                verdict: judge_bridge_running(harness_pid),
            },
            CheckResult {
                check: Check::SocketPathExists,
                verdict: judge_socket_path_exists(&inputs.messaging_socket),
            },
            CheckResult {
                check: Check::NoRefuseApplies,
                verdict: judge_no_refuse_applies(
                    inputs.settings_files.iter().chain(&command_line.files),
                    command_line.unverified.iter().cloned().chain(
                        inputs
                            .unreadable_settings
                            .iter()
                            .map(|path| format!("{} {UNREADABLE_FILE_NOTE}", path.display())),
                    ),
                ),
            },
            CheckResult {
                check: Check::BinaryProcessFound,
                verdict: judge_binary_process_found(harness_pid, &entries, &inputs.runtime_inputs)?,
            },
        ],
        dropped_frames: harness_pid
            .and_then(|pid| read_text_file(&dropped_frames_path(&inputs.temporary_dir, pid)))
            .and_then(|text| text.trim().parse().ok()),
    })
}

fn judge_socket_path_exists(socket: &MessagingSocket) -> Verdict {
    match socket {
        MessagingSocket::Present(_) => Verdict::Passed,
        MessagingSocket::Missing(path) => Verdict::Failed(format!(
            "{MESSAGING_SOCKET_VARIABLE} names {}, which does not exist",
            path.display()
        )),
        MessagingSocket::Unset => Verdict::Failed(format!(
            "{MESSAGING_SOCKET_VARIABLE} is not set; run the doctor inside the session"
        )),
    }
}

fn judge_no_refuse_applies<'a>(
    settings_files: impl Iterator<Item = &'a SettingsFile>,
    unreadable_sources: impl Iterator<Item = String>,
) -> Verdict {
    let mut unverified: Vec<String> = unreadable_sources.collect();
    for file in settings_files {
        match parse_settings(file) {
            Some(document) if document.cross_session_inbound.as_deref() == Some(REFUSE_VALUE) => {
                return Verdict::Failed(format!(
                    "{} sets {CROSS_SESSION_INBOUND_KEY} to {REFUSE_VALUE}, which drops the bridge's frames",
                    file.path.display()
                ));
            }
            Some(_) => {}
            None => unverified.push(format!(
                "{} is not valid settings JSON",
                file.path.display()
            )),
        }
    }
    if unverified.is_empty() {
        Verdict::Passed
    } else {
        Verdict::Unverified(format!("not verified: {}", unverified.join("; ")))
    }
}

struct CommandLineSettings {
    files: Vec<SettingsFile>,
    unverified: Vec<String>,
}

fn settings_of_harness_command_line(
    harness_pid: Option<u32>,
    entries: &[ProcessEntry],
    read_text_file: &impl Fn(&Path) -> Option<String>,
) -> CommandLineSettings {
    let mut settings = CommandLineSettings {
        files: Vec::new(),
        unverified: Vec::new(),
    };
    let Some(harness_pid) = harness_pid else {
        settings
            .unverified
            .push("the harness command line is unknown without a running bridge".to_owned());
        return settings;
    };
    let Some(harness) = entries.iter().find(|entry| entry.pid == harness_pid) else {
        settings
            .unverified
            .push(format!("process {harness_pid} is not in the process table"));
        return settings;
    };
    for value in settings_flag_values(&harness.arguments) {
        if value.trim_start().starts_with('{') {
            settings.files.push(SettingsFile {
                path: PathBuf::from(COMMAND_LINE_SETTINGS_NAME),
                text: value,
            });
        } else if let Some(text) = read_text_file(Path::new(&value)) {
            settings.files.push(SettingsFile {
                path: PathBuf::from(value),
                text,
            });
        } else {
            settings
                .unverified
                .push(format!("{value} (the {SETTINGS_FLAG} file cannot be read)"));
        }
    }
    settings
}

fn settings_flag_values(arguments: &[String]) -> Vec<String> {
    let inline_prefix = format!("{SETTINGS_FLAG}=");
    arguments
        .iter()
        .enumerate()
        .filter_map(|(position, argument)| {
            let inline_head = argument.strip_prefix(&inline_prefix);
            if inline_head.is_none() && argument != SETTINGS_FLAG {
                return None;
            }
            let following = arguments.get(position.saturating_add(1)..)?;
            let words: Vec<&str> = inline_head
                .into_iter()
                .chain(
                    following
                        .iter()
                        .map(String::as_str)
                        .take_while(|word| !word.starts_with('-')),
                )
                .collect();
            Some(words.join(" "))
        })
        .collect()
}

struct SingleSnapshot {
    entries: Vec<ProcessEntry>,
    already_listed: Cell<bool>,
}

impl ProcessTable for SingleSnapshot {
    fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
        if self.already_listed.replace(true) {
            Ok(Vec::new())
        } else {
            Ok(self.entries.clone())
        }
    }
}

fn judge_binary_process_found(
    harness_pid: Option<u32>,
    entries: &[ProcessEntry],
    runtime_inputs: &RuntimeDirectoryInputs,
) -> Result<Verdict, ProcessTableError> {
    let Some(harness_pid) = harness_pid else {
        return Ok(Verdict::Failed(
            "the session is unknown without a running bridge, so no binary process can be looked for".to_owned(),
        ));
    };
    let snapshot = SingleSnapshot {
        entries: entries.to_vec(),
        already_listed: Cell::new(false),
    };
    let mut found_sockets = Vec::new();
    follow_binary_sockets(
        harness_pid,
        runtime_inputs,
        &snapshot,
        &mut |_| {},
        &mut |sockets| found_sockets = sockets.to_vec(),
    )?;
    Ok(if found_sockets.is_empty() {
        Verdict::Failed(format!(
            "no estafette process in mcp mode runs under process {harness_pid}"
        ))
    } else {
        Verdict::Passed
    })
}

fn judge_bridge_running(harness_pid: Option<u32>) -> Verdict {
    match harness_pid {
        Some(_) => Verdict::Passed,
        None => Verdict::Failed(format!(
            "no {BRIDGE_EXECUTABLE_NAME} process runs for a parent process of this command"
        )),
    }
}

fn ancestor_pids(pid: u32, entries: &[ProcessEntry]) -> BTreeSet<u32> {
    let mut ancestors = BTreeSet::new();
    let mut current = pid;
    while let Some(parent) = entries.iter().find(|entry| entry.pid == current) {
        if !ancestors.insert(parent.parent_pid) {
            break;
        }
        current = parent.parent_pid;
    }
    ancestors
}

fn harness_of_bridge_for_session(own_pid: u32, entries: &[ProcessEntry]) -> Option<u32> {
    let ancestors = ancestor_pids(own_pid, entries);
    entries
        .iter()
        .filter_map(served_harness_pid)
        .find(|harness_pid| ancestors.contains(harness_pid))
}

/// Names the pid of a bridge process other than this one that already serves the harness process.
#[must_use]
pub fn running_bridge_for(harness_pid: u32, own_pid: u32, entries: &[ProcessEntry]) -> Option<u32> {
    entries
        .iter()
        .find(|entry| entry.pid != own_pid && served_harness_pid(entry) == Some(harness_pid))
        .map(|entry| entry.pid)
}

fn served_harness_pid(entry: &ProcessEntry) -> Option<u32> {
    if is_bridge_process(entry) {
        entry.arguments.get(1)?.parse().ok()
    } else {
        None
    }
}

fn is_bridge_process(entry: &ProcessEntry) -> bool {
    entry
        .executable
        .as_deref()
        .is_some_and(|path| Path::new(path).file_name() == Some(OsStr::new(BRIDGE_EXECUTABLE_NAME)))
        && entry.arguments.first().map(String::as_str) == Some(RUN_SUBCOMMAND)
}

#[derive(Default, Deserialize)]
struct SettingsDocument {
    #[serde(default)]
    hooks: HooksSection,
    #[serde(rename = "crossSessionInbound")]
    cross_session_inbound: Option<String>,
}

#[derive(Default, Deserialize)]
struct HooksSection {
    #[serde(rename = "SessionStart", default)]
    session_start: Vec<HookGroup>,
}

#[derive(Deserialize)]
struct HookGroup {
    #[serde(default)]
    hooks: Vec<HookEntry>,
}

#[derive(Deserialize)]
struct HookEntry {
    command: Option<String>,
}

fn parse_settings(file: &SettingsFile) -> Option<SettingsDocument> {
    serde_json::from_str(&file.text).ok()
}

fn judge_hook_installed(settings_files: &[SettingsFile]) -> Verdict {
    let starts_bridge = |document: SettingsDocument| {
        document
            .hooks
            .session_start
            .iter()
            .flat_map(|group| &group.hooks)
            .filter_map(|hook| hook.command.as_deref())
            .any(|command| command.contains(BRIDGE_EXECUTABLE_NAME))
    };
    if settings_files
        .iter()
        .filter_map(parse_settings)
        .any(starts_bridge)
    {
        Verdict::Passed
    } else {
        Verdict::Failed(format!(
            "no SessionStart hook in the user, project or local settings starts {BRIDGE_EXECUTABLE_NAME}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::ProcessEntry;

    const TEST_TEMPORARY_DIR: &str = "/var/tmp";
    const DROPPED_FRAMES_FILE_OF_HARNESS_100: &str =
        "/var/tmp/estafette-bridge-claude-100.dropped-frames";

    fn read_only_the_dropped_frames_file_of_harness_100(path: &Path) -> Option<String> {
        (path == Path::new(DROPPED_FRAMES_FILE_OF_HARNESS_100)).then(|| "7\n".to_owned())
    }

    fn doctor_report(inputs: &DoctorInputs, processes: &FixedProcessTable) -> DoctorReport {
        run_doctor(
            inputs,
            processes,
            &read_only_the_dropped_frames_file_of_harness_100,
        )
        .expect("the table never fails")
    }

    struct FixedProcessTable(Vec<ProcessEntry>);

    impl ProcessTable for FixedProcessTable {
        fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
            Ok(self.0.clone())
        }
    }

    fn inputs_without_any_wiring() -> DoctorInputs {
        DoctorInputs {
            own_pid: 500,
            messaging_socket: MessagingSocket::Unset,
            settings_files: Vec::new(),
            unreadable_settings: Vec::new(),
            temporary_dir: PathBuf::from(TEST_TEMPORARY_DIR),
            runtime_inputs: RuntimeDirectoryInputs {
                xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
                temporary_dir: None,
                user_id: 1000,
            },
        }
    }

    fn verdict_of(report: &DoctorReport, check: Check) -> &Verdict {
        &report
            .results
            .iter()
            .find(|result| result.check == check)
            .expect("every check is reported")
            .verdict
    }

    #[test]
    fn spec_3_12_doctor_without_a_session_start_hook_fails_the_hook_check() {
        let report = doctor_report(&inputs_without_any_wiring(), &FixedProcessTable(Vec::new()));
        assert!(matches!(
            verdict_of(&report, Check::HookInstalled),
            Verdict::Failed(_)
        ));
        assert!(!report.all_passed());
    }

    const SHIPPED_HOOK_SNIPPET: &str = include_str!("../hooks/session-start.json");
    const BRIDGE_ON_ANOTHER_EVENT: &str = r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"estafette-bridge-claude run 1"}]}],"SessionStart":[{"hooks":[{"type":"command","command":"echo started"}]}]}}"#;

    fn inputs_with_settings(text: &str) -> DoctorInputs {
        DoctorInputs {
            settings_files: vec![SettingsFile {
                path: PathBuf::from("/home/ana/.claude/settings.json"),
                text: text.to_owned(),
            }],
            ..inputs_without_any_wiring()
        }
    }

    fn hook_verdict_for(text: &str) -> Verdict {
        let report = doctor_report(&inputs_with_settings(text), &FixedProcessTable(Vec::new()));
        report
            .results
            .into_iter()
            .find(|result| result.check == Check::HookInstalled)
            .expect("hook check reported")
            .verdict
    }

    #[test]
    fn spec_3_12_doctor_accepts_the_shipped_hook_snippet_and_only_a_session_start_hook() {
        assert_eq!(hook_verdict_for(SHIPPED_HOOK_SNIPPET), Verdict::Passed);
        assert!(matches!(
            hook_verdict_for(BRIDGE_ON_ANOTHER_EVENT),
            Verdict::Failed(_)
        ));
    }

    fn process(pid: u32, parent_pid: u32, executable: &str, arguments: &[&str]) -> ProcessEntry {
        ProcessEntry {
            pid,
            parent_pid,
            executable: Some(executable.to_owned()),
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
        }
    }

    fn session_with_doctor_beneath_harness_100() -> Vec<ProcessEntry> {
        vec![
            process(1, 0, "/sbin/init", &[]),
            process(100, 1, "/usr/bin/claude", &[]),
            process(400, 100, "/bin/zsh", &["-c"]),
            process(500, 400, "/opt/bin/estafette-bridge-claude", &["doctor"]),
        ]
    }

    fn bridge_verdict_among(extra: Vec<ProcessEntry>) -> Verdict {
        let mut entries = session_with_doctor_beneath_harness_100();
        entries.extend(extra);
        doctor_report(&inputs_without_any_wiring(), &FixedProcessTable(entries))
            .results
            .into_iter()
            .find(|result| result.check == Check::BridgeRunning)
            .expect("bridge check reported")
            .verdict
    }

    #[test]
    fn spec_3_12_doctor_passes_the_bridge_check_only_for_a_bridge_of_an_ancestor_session() {
        let bridge_of_this_session =
            process(700, 1, "/opt/bin/estafette-bridge-claude", &["run", "100"]);
        let bridge_of_another_session =
            process(701, 1, "/opt/bin/estafette-bridge-claude", &["run", "999"]);
        assert_eq!(
            bridge_verdict_among(vec![
                bridge_of_another_session.clone(),
                bridge_of_this_session
            ]),
            Verdict::Passed
        );
        assert!(matches!(
            bridge_verdict_among(vec![bridge_of_another_session]),
            Verdict::Failed(_)
        ));
    }

    const REFUSING_SETTINGS: &str = r#"{"crossSessionInbound":"refuse"}"#;
    const ACCEPTING_SETTINGS: &str = r#"{"crossSessionInbound":"accept"}"#;

    fn settings_file(path: &str, text: &str) -> SettingsFile {
        SettingsFile {
            path: PathBuf::from(path),
            text: text.to_owned(),
        }
    }

    fn healthy_session() -> (DoctorInputs, FixedProcessTable) {
        let mut entries = session_with_doctor_beneath_harness_100();
        entries.push(process(
            700,
            1,
            "/opt/bin/estafette-bridge-claude",
            &["run", "100"],
        ));
        entries.push(process(800, 100, "/usr/bin/estafette", &["mcp"]));
        let inputs = DoctorInputs {
            messaging_socket: MessagingSocket::Present(PathBuf::from("/tmp/cc-socks-1000/s.sock")),
            settings_files: vec![
                settings_file("/home/ana/.claude/settings.json", SHIPPED_HOOK_SNIPPET),
                settings_file("/work/.claude/settings.json", ACCEPTING_SETTINGS),
            ],
            ..inputs_without_any_wiring()
        };
        (inputs, FixedProcessTable(entries))
    }

    fn report_of_healthy_session_after(
        change: impl FnOnce(&mut DoctorInputs, &mut FixedProcessTable),
    ) -> DoctorReport {
        let (mut inputs, mut processes) = healthy_session();
        change(&mut inputs, &mut processes);
        doctor_report(&inputs, &processes)
    }

    #[test]
    fn spec_3_12_doctor_passes_every_check_of_a_wired_session_and_reports_dropped_frames() {
        let report = report_of_healthy_session_after(|_, _| {});
        assert!(report.all_passed(), "{report}");
        assert_eq!(report.dropped_frames, Some(7));
    }

    #[test]
    fn spec_3_12_doctor_fails_the_socket_check_for_a_missing_or_unset_socket() {
        let missing = report_of_healthy_session_after(|inputs, _| {
            inputs.messaging_socket = MessagingSocket::Missing(PathBuf::from("/tmp/gone.sock"));
        });
        assert_eq!(
            verdict_of(&missing, Check::SocketPathExists),
            &Verdict::Failed(
                "CLAUDE_CODE_MESSAGING_SOCKET names /tmp/gone.sock, which does not exist"
                    .to_owned()
            )
        );
        let unset = report_of_healthy_session_after(|inputs, _| {
            inputs.messaging_socket = MessagingSocket::Unset;
        });
        assert!(matches!(
            verdict_of(&unset, Check::SocketPathExists),
            Verdict::Failed(_)
        ));
        assert_eq!(verdict_of(&unset, Check::HookInstalled), &Verdict::Passed);
    }

    #[test]
    fn spec_3_12_doctor_fails_the_refuse_check_naming_the_file_that_refuses() {
        let report = report_of_healthy_session_after(|inputs, _| {
            inputs.settings_files.push(settings_file(
                "/work/.claude/settings.local.json",
                REFUSING_SETTINGS,
            ));
        });
        assert_eq!(
            verdict_of(&report, Check::NoRefuseApplies),
            &Verdict::Failed(
                "/work/.claude/settings.local.json sets crossSessionInbound to refuse, which drops the bridge's frames"
                    .to_owned()
            )
        );
    }

    #[test]
    fn spec_3_12_doctor_fails_the_binary_check_when_no_mcp_binary_runs_under_the_session() {
        let report = report_of_healthy_session_after(|_, processes| {
            processes.0.retain(|entry| entry.pid != 800);
            processes
                .0
                .push(process(801, 1, "/usr/bin/estafette", &["mcp"]));
            processes
                .0
                .push(process(802, 100, "/usr/bin/estafette", &["send"]));
        });
        assert_eq!(
            verdict_of(&report, Check::BinaryProcessFound),
            &Verdict::Failed("no estafette process in mcp mode runs under process 100".to_owned())
        );
        assert_eq!(verdict_of(&report, Check::BridgeRunning), &Verdict::Passed);
    }

    #[test]
    fn spec_3_12_doctor_renders_each_check_and_the_dropped_frame_count() {
        let report = report_of_healthy_session_after(|inputs, _| {
            inputs.messaging_socket = MessagingSocket::Unset;
        });
        assert_eq!(
            report.to_string(),
            "ok    hook installed\n\
             ok    bridge process runs for this session\n\
             FAIL  socket path exists: CLAUDE_CODE_MESSAGING_SOCKET is not set; run the doctor inside the session\n\
             ok    no refuse applies\n\
             ok    binary process found\n\
             dropped frames: 7"
        );
    }

    #[test]
    fn spec_3_12_doctor_reports_unknown_dropped_frames_when_the_file_is_unreadable_or_not_a_count()
    {
        let (inputs, processes) = healthy_session();
        let count_after_reading = |text: Option<&str>| {
            run_doctor(&inputs, &processes, &|_: &Path| text.map(str::to_owned))
                .expect("the table never fails")
                .dropped_frames
        };
        assert_eq!(count_after_reading(None), None);
        assert_eq!(count_after_reading(Some("many")), None);
        assert_eq!(count_after_reading(Some(" 12 ")), Some(12));
    }

    const REFUSE_JSON: &str = r#"{"crossSessionInbound":"refuse"}"#;

    fn harness_with_arguments(arguments: &[&str]) -> ProcessEntry {
        process(100, 1, "/usr/bin/claude", arguments)
    }

    fn refuse_verdict_with_harness_arguments(
        arguments: &[&str],
        read_text_file: &impl Fn(&Path) -> Option<String>,
    ) -> Verdict {
        let (inputs, mut processes) = healthy_session();
        processes.0.retain(|entry| entry.pid != 100);
        processes.0.push(harness_with_arguments(arguments));
        run_doctor(&inputs, &processes, read_text_file)
            .expect("the table never fails")
            .results
            .into_iter()
            .find(|result| result.check == Check::NoRefuseApplies)
            .expect("refuse check reported")
            .verdict
    }

    fn read_nothing(_: &Path) -> Option<String> {
        None
    }

    #[test]
    fn spec_3_12_doctor_fails_the_refuse_check_for_a_refuse_passed_inline_on_the_harness_command_line()
     {
        let refusing = Verdict::Failed(
            "--settings on the harness command line sets crossSessionInbound to refuse, which drops the bridge's frames"
                .to_owned(),
        );
        for arguments in [
            vec!["--settings", REFUSE_JSON],
            vec![
                "--model",
                "opus",
                "--settings",
                "{\"crossSessionInbound\":",
                "\"refuse\"}",
                "--verbose",
            ],
            vec!["--settings={\"crossSessionInbound\":\"refuse\"}"],
            vec![
                "--settings",
                r#"{"crossSessionInbound":"accept"}"#,
                "--settings",
                REFUSE_JSON,
            ],
        ] {
            assert_eq!(
                refuse_verdict_with_harness_arguments(&arguments, &read_nothing),
                refusing,
                "{arguments:?}"
            );
        }
    }

    #[test]
    fn spec_3_12_doctor_reads_a_settings_file_named_on_the_harness_command_line() {
        let read_refusing_file =
            |path: &Path| (path == Path::new("/work/session.json")).then(|| REFUSE_JSON.to_owned());
        assert_eq!(
            refuse_verdict_with_harness_arguments(
                &["--settings", "/work/session.json"],
                &read_refusing_file
            ),
            Verdict::Failed(
                "/work/session.json sets crossSessionInbound to refuse, which drops the bridge's frames"
                    .to_owned()
            )
        );
        assert_eq!(
            refuse_verdict_with_harness_arguments(
                &["--settings", "/work/session.json"],
                &read_nothing
            ),
            Verdict::Unverified(
                "not verified: /work/session.json (the --settings file cannot be read)".to_owned()
            )
        );
        assert_eq!(
            refuse_verdict_with_harness_arguments(
                &["--settings", "{\"crossSession"],
                &read_nothing
            ),
            Verdict::Unverified(
                "not verified: --settings on the harness command line is not valid settings JSON"
                    .to_owned()
            )
        );
    }

    #[test]
    fn spec_3_12_doctor_never_passes_the_refuse_check_over_a_source_it_could_not_read() {
        let (mut inputs, processes) = healthy_session();
        inputs.unreadable_settings = vec![PathBuf::from("/etc/claude-code/managed-settings.json")];
        let report = doctor_report(&inputs, &processes);
        assert_eq!(
            verdict_of(&report, Check::NoRefuseApplies),
            &Verdict::Unverified(
                "not verified: /etc/claude-code/managed-settings.json (the file cannot be read)"
                    .to_owned()
            )
        );
        assert!(!report.all_passed());
        inputs.settings_files.push(settings_file(
            "/etc/claude-code/managed-settings.d/10.json",
            REFUSE_JSON,
        ));
        assert!(matches!(
            verdict_of(&doctor_report(&inputs, &processes), Check::NoRefuseApplies),
            Verdict::Failed(_)
        ));
    }

    #[test]
    fn spec_3_12_doctor_cannot_verify_the_command_line_of_an_unknown_session() {
        let report = doctor_report(&inputs_without_any_wiring(), &FixedProcessTable(Vec::new()));
        assert_eq!(
            verdict_of(&report, Check::NoRefuseApplies),
            &Verdict::Unverified(
                "not verified: the harness command line is unknown without a running bridge"
                    .to_owned()
            )
        );
        assert!(
            report
                .to_string()
                .contains("UNVERIFIED no refuse applies: not verified:")
        );
    }
}
