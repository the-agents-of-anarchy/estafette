use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt;
#[cfg(target_os = "linux")]
use std::io::ErrorKind;
use std::num::ParseIntError;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::str::FromStr;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_millis(2000);
const BINARY_EXECUTABLE_NAME: &str = "estafette";
const MCP_MODE_ARGUMENT: &str = "mcp";
const RUNTIME_DIRECTORY_PREFIX: &str = "estafette-";
const SOCKET_FILE_SUFFIX: &str = ".sock";
const PS_PROGRAM: &str = "ps";
const PS_ALL_PROCESSES_FLAG: &str = "-A";
const PS_OUTPUT_FORMAT_FLAG: &str = "-o";
const PS_COMMAND_LINE_COLUMNS: &str = "pid=,ppid=,args=";
#[cfg(target_os = "macos")]
const PS_EXECUTABLE_PATH_COLUMNS: &str = "pid=,comm=";
#[cfg(target_os = "linux")]
const PROC_DIRECTORY: &str = "/proc";
#[cfg(target_os = "linux")]
const EXECUTABLE_LINK_NAME: &str = "exe";

/// An operating system process id.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Pid(pub u32);

impl fmt::Display for Pid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for Pid {
    type Err = ParseIntError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

#[derive(Clone, Debug)]
pub struct ProcessEntry {
    pub pid: Pid,
    pub parent_pid: Pid,
    pub executable: Option<String>,
    pub arguments: Vec<String>,
}

#[derive(Debug)]
pub enum ProcessTableError {
    ListingFailed(std::io::Error),
    ListingExitedUnsuccessfully(ExitStatus),
    MalformedListingLine,
}

impl fmt::Display for ProcessTableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListingFailed(source) => write!(formatter, "cannot run {PS_PROGRAM}: {source}"),
            Self::ListingExitedUnsuccessfully(status) => {
                write!(formatter, "{PS_PROGRAM} failed: {status}")
            }
            Self::MalformedListingLine => {
                write!(formatter, "{PS_PROGRAM} printed a line it cannot read")
            }
        }
    }
}

impl std::error::Error for ProcessTableError {}

pub struct PsProcessTable;

impl ProcessTable for PsProcessTable {
    fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
        let command_lines = run_ps(PS_COMMAND_LINE_COLUMNS)?;
        let executables = executable_paths()?;
        entries_from_command_lines(&command_lines, &executables)
    }
}

fn run_ps(columns: &str) -> Result<String, ProcessTableError> {
    let listing = Command::new(PS_PROGRAM)
        .args([PS_ALL_PROCESSES_FLAG, PS_OUTPUT_FORMAT_FLAG, columns])
        .output()
        .map_err(ProcessTableError::ListingFailed)?;
    if !listing.status.success() {
        return Err(ProcessTableError::ListingExitedUnsuccessfully(
            listing.status,
        ));
    }
    Ok(String::from_utf8_lossy(&listing.stdout).into_owned())
}

#[cfg(target_os = "macos")]
fn executable_paths() -> Result<BTreeMap<Pid, String>, ProcessTableError> {
    run_ps(PS_EXECUTABLE_PATH_COLUMNS)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let (pid_text, path) = split_first_token(line);
            pid_text
                .parse()
                .ok()
                .map(|pid| (pid, path.trim().to_owned()))
                .ok_or(ProcessTableError::MalformedListingLine)
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn executable_paths() -> Result<BTreeMap<Pid, String>, ProcessTableError> {
    let mut paths = BTreeMap::new();
    for process_directory in
        std::fs::read_dir(PROC_DIRECTORY).map_err(ProcessTableError::ListingFailed)?
    {
        let process_directory = process_directory.map_err(ProcessTableError::ListingFailed)?;
        let Some(pid) = process_directory
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        match std::fs::read_link(process_directory.path().join(EXECUTABLE_LINK_NAME)) {
            Ok(path) => {
                paths.insert(pid, path.to_string_lossy().into_owned());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::PermissionDenied
                ) => {}
            Err(error) => return Err(ProcessTableError::ListingFailed(error)),
        }
    }
    Ok(paths)
}

fn split_first_token(text: &str) -> (&str, &str) {
    let trimmed = text.trim_start();
    trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""))
}

fn entries_from_command_lines(
    listing: &str,
    executables: &BTreeMap<Pid, String>,
) -> Result<Vec<ProcessEntry>, ProcessTableError> {
    let mut entries = Vec::new();
    for line in listing.lines().filter(|line| !line.trim().is_empty()) {
        let (pid_text, after_pid) = split_first_token(line);
        let (parent_pid_text, command_line) = split_first_token(after_pid);
        let pid = pid_text
            .parse()
            .ok()
            .ok_or(ProcessTableError::MalformedListingLine)?;
        let parent_pid = parent_pid_text
            .parse()
            .ok()
            .ok_or(ProcessTableError::MalformedListingLine)?;
        let executable = executables.get(&pid).cloned();
        entries.push(ProcessEntry {
            pid,
            parent_pid,
            arguments: arguments_after_executable(command_line, executable.as_deref()),
            executable,
        });
    }
    Ok(entries)
}

fn arguments_after_executable(command_line: &str, executable: Option<&str>) -> Vec<String> {
    let after_program_name = match executable.and_then(|path| command_line.strip_prefix(path)) {
        Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => rest,
        _ => split_first_token(command_line).1,
    };
    after_program_name
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn is_binary_path(path: &str) -> bool {
    Path::new(path).file_name() == Some(OsStr::new(BINARY_EXECUTABLE_NAME))
}

pub trait ProcessTable {
    /// Lists every process the operating system reports.
    /// # Errors
    /// Returns a `ProcessTableError` when the listing cannot be taken or read.
    fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError>;
}

pub struct RuntimeDirectoryInputs {
    pub xdg_runtime_dir: Option<PathBuf>,
    pub temporary_dir: PathBuf,
    pub user_id: u32,
}

/// Reports the inbox socket paths of the binary's MCP-mode processes under the harness at every poll, until the harness exits.
/// # Errors
/// Returns the `ProcessTableError` of the listing failure that ended the following.
pub fn follow_binary_sockets(
    harness_pid: Pid,
    runtime_inputs: &RuntimeDirectoryInputs,
    processes: &impl ProcessTable,
    wait_for_next_poll: &mut impl FnMut(Duration),
    report_sockets: &mut impl FnMut(&[PathBuf]),
) -> Result<(), ProcessTableError> {
    loop {
        let entries = processes.list()?;
        if !is_running(harness_pid, &entries) {
            return Ok(());
        }
        report_sockets(&binary_sockets_under(harness_pid, runtime_inputs, &entries));
        wait_for_next_poll(POLL_INTERVAL);
    }
}

/// Lists the inbox socket paths of the binary's MCP-mode processes among the descendants of the harness process.
#[must_use]
pub fn binary_sockets_under(
    harness_pid: Pid,
    runtime_inputs: &RuntimeDirectoryInputs,
    entries: &[ProcessEntry],
) -> Vec<PathBuf> {
    let runtime_dir = runtime_directory(runtime_inputs);
    binary_pids_under(harness_pid, entries)
        .into_iter()
        .map(|pid| socket_path(&runtime_dir, pid))
        .collect()
}

fn runtime_directory(inputs: &RuntimeDirectoryInputs) -> PathBuf {
    let base = inputs
        .xdg_runtime_dir
        .clone()
        .unwrap_or_else(|| inputs.temporary_dir.clone());
    base.join(format!("{RUNTIME_DIRECTORY_PREFIX}{}", inputs.user_id))
}

fn socket_path(runtime_dir: &Path, pid: Pid) -> PathBuf {
    runtime_dir.join(format!("{pid}{SOCKET_FILE_SUFFIX}"))
}

fn is_running(pid: Pid, entries: &[ProcessEntry]) -> bool {
    entries.iter().any(|entry| entry.pid == pid)
}

fn binary_pids_under(harness_pid: Pid, entries: &[ProcessEntry]) -> BTreeSet<Pid> {
    let descendants = descendant_pids(harness_pid, entries);
    entries
        .iter()
        .filter(|entry| descendants.contains(&entry.pid) && runs_binary_in_mcp_mode(entry))
        .map(|entry| entry.pid)
        .collect()
}

fn descendant_pids(harness_pid: Pid, entries: &[ProcessEntry]) -> BTreeSet<Pid> {
    let mut descendants = BTreeSet::new();
    let mut parents_to_expand = vec![harness_pid];
    while let Some(parent_pid) = parents_to_expand.pop() {
        for child in entries
            .iter()
            .filter(|entry| entry.parent_pid == parent_pid)
        {
            if descendants.insert(child.pid) {
                parents_to_expand.push(child.pid);
            }
        }
    }
    descendants
}

fn runs_binary_in_mcp_mode(entry: &ProcessEntry) -> bool {
    entry.executable.as_deref().is_some_and(is_binary_path)
        && entry.arguments.first().map(String::as_str) == Some(MCP_MODE_ARGUMENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seeded::next_below;
    use std::cell::{Cell, RefCell};
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};

    struct SnapshotSequence(RefCell<Vec<Vec<ProcessEntry>>>);

    impl ProcessTable for SnapshotSequence {
        fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
            let mut remaining = self.0.borrow_mut();
            let snapshot = if remaining.len() > 1 {
                remaining.remove(0)
            } else {
                remaining.first().cloned().unwrap_or_default()
            };
            Ok(snapshot)
        }
    }

    struct UnreadableListing;

    impl ProcessTable for UnreadableListing {
        fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
            Err(ProcessTableError::MalformedListingLine)
        }
    }

    const GENERATED_CASES: u64 = 300;
    const NON_PID_TOKENS: [&str; 6] = ["abc", "-5", "1.5", "4294967296", "0x10", "12a"];
    const TOKEN_SUFFIXES: [&str; 3] = ["", "z", "9"];

    #[test]
    fn spec_3_12_step_2_a_listing_line_whose_first_token_is_not_a_pid_is_malformed() {
        for seed in 0..GENERATED_CASES {
            let mut state = seed;
            let token = format!(
                "{}{}",
                NON_PID_TOKENS[next_below(&mut state, NON_PID_TOKENS.len())],
                TOKEN_SUFFIXES[next_below(&mut state, TOKEN_SUFFIXES.len())]
            );
            let parent_pid = next_below(&mut state, usize::from(u16::MAX));
            let line = format!("{token} {parent_pid} /bin/sh");
            let listing = entries_from_command_lines(&line, &BTreeMap::new());
            assert!(
                matches!(&listing, Err(ProcessTableError::MalformedListingLine)),
                "{line}"
            );
        }
    }

    #[test]
    fn spec_3_12_step_2_listing_failure_stops_following_with_the_listing_error() {
        let mut reported = false;
        let following = follow_binary_sockets(
            Pid(100),
            &runtime_inputs(),
            &UnreadableListing,
            &mut |_| {},
            &mut |_| reported = true,
        );
        assert!(
            matches!(following, Err(ProcessTableError::MalformedListingLine)),
            "the listing failure ends the following"
        );
        assert!(!reported, "reported sockets from a failed listing");
    }

    fn entry(pid: u32, parent_pid: u32, executable: &str, arguments: &[&str]) -> ProcessEntry {
        ProcessEntry {
            pid: Pid(pid),
            parent_pid: Pid(parent_pid),
            executable: Some(executable.to_owned()),
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
        }
    }

    fn runtime_inputs() -> RuntimeDirectoryInputs {
        RuntimeDirectoryInputs {
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
            temporary_dir: PathBuf::from("/var/tmp"),
            user_id: 1000,
        }
    }

    fn collect_reports(
        harness_pid: u32,
        inputs: &RuntimeDirectoryInputs,
        snapshots: Vec<Vec<ProcessEntry>>,
    ) -> Vec<Vec<PathBuf>> {
        let table = SnapshotSequence(RefCell::new(snapshots));
        let mut reports = Vec::new();
        follow_binary_sockets(
            Pid(harness_pid),
            inputs,
            &table,
            &mut |_| {},
            &mut |sockets| {
                reports.push(sockets.to_vec());
            },
        )
        .expect("the table never fails");
        reports
    }

    #[test]
    fn spec_3_12_steps_1_2_reports_sockets_of_mcp_binaries_among_all_descendants() {
        let tree = vec![
            entry(1, 0, "/sbin/init", &[]),
            entry(100, 1, "/usr/bin/claude", &[]),
            entry(101, 100, "/usr/bin/estafette", &["mcp"]),
            entry(102, 100, "/bin/sh", &["-c", "run"]),
            entry(103, 102, "/opt/bin/estafette", &["mcp"]),
            entry(104, 100, "/usr/bin/estafette", &["send"]),
            entry(105, 100, "/usr/bin/estafette-other", &["mcp"]),
            entry(106, 103, "/usr/bin/estafette", &["mcp"]),
            entry(200, 1, "/usr/bin/estafette", &["mcp"]),
        ];
        let reports = collect_reports(100, &runtime_inputs(), vec![tree, vec![]]);
        assert_eq!(
            reports,
            vec![vec![
                PathBuf::from("/run/user/1000/estafette-1000/101.sock"),
                PathBuf::from("/run/user/1000/estafette-1000/103.sock"),
                PathBuf::from("/run/user/1000/estafette-1000/106.sock"),
            ]]
        );
    }

    fn single_binary_under_harness() -> Vec<ProcessEntry> {
        vec![
            entry(100, 1, "/usr/bin/claude", &[]),
            entry(101, 100, "/usr/bin/estafette", &["mcp"]),
        ]
    }

    #[test]
    fn spec_3_3_runtime_falls_back_to_temporary_dir_without_xdg_runtime_dir() {
        let inputs = RuntimeDirectoryInputs {
            xdg_runtime_dir: None,
            ..runtime_inputs()
        };
        let reports = collect_reports(100, &inputs, vec![single_binary_under_harness(), vec![]]);
        assert_eq!(
            reports,
            vec![vec![PathBuf::from("/var/tmp/estafette-1000/101.sock")]]
        );
    }

    #[test]
    fn spec_3_12_steps_5_6_polls_again_after_each_wait_and_returns_when_harness_exits() {
        let without_binary = vec![entry(100, 1, "/usr/bin/claude", &[])];
        let snapshots = vec![
            without_binary.clone(),
            single_binary_under_harness(),
            without_binary,
            vec![],
        ];
        let table = SnapshotSequence(RefCell::new(snapshots));
        let mut reports = Vec::new();
        let mut waited = Vec::new();
        follow_binary_sockets(
            Pid(100),
            &runtime_inputs(),
            &table,
            &mut |duration| waited.push(duration),
            &mut |sockets| reports.push(sockets.to_vec()),
        )
        .expect("the table never fails");
        assert_eq!(
            reports,
            vec![
                vec![],
                vec![PathBuf::from("/run/user/1000/estafette-1000/101.sock")],
                vec![],
            ]
        );
        assert_eq!(waited, [Duration::from_millis(2000); 3]);
    }

    #[test]
    fn spec_3_12_step_6_returns_after_exactly_the_polls_made_while_harness_ran() {
        const MOST_POLLS_WHILE_HARNESS_RUNS: usize = 4;
        for polls_while_harness_runs in 0..=MOST_POLLS_WHILE_HARNESS_RUNS {
            let mut snapshots: Vec<Vec<ProcessEntry>> = (0..polls_while_harness_runs)
                .map(|_| single_binary_under_harness())
                .collect();
            snapshots.push(vec![entry(101, 1, "/usr/bin/estafette", &["mcp"])]);
            let table = SnapshotSequence(RefCell::new(snapshots));
            let mut polls = 0_usize;
            let mut waits = 0_usize;
            follow_binary_sockets(
                Pid(100),
                &runtime_inputs(),
                &table,
                &mut |_| {
                    waits = waits.saturating_add(1);
                    assert!(
                        waits <= polls_while_harness_runs,
                        "kept polling after the harness exited"
                    );
                },
                &mut |_| polls = polls.saturating_add(1),
            )
            .expect("the table never fails");
            assert_eq!(
                (polls, waits),
                (polls_while_harness_runs, polls_while_harness_runs)
            );
        }
    }

    const TREE_STARTUP_POLL_LIMIT: u32 = 200;
    fn executable_table(pid: u32, executable: &str) -> BTreeMap<Pid, String> {
        BTreeMap::from([(Pid(pid), executable.to_owned())])
    }

    fn only_entry(entries: &[ProcessEntry]) -> &ProcessEntry {
        let [entry] = entries else {
            panic!("exactly one entry expected, got {entries:?}");
        };
        entry
    }

    #[test]
    fn spec_3_12_step_2_executable_with_a_space_is_taken_whole_from_the_os() {
        let entries = entries_from_command_lines(
            "  77     1 /Users/First Last/bin/estafette mcp --x\n",
            &executable_table(77, "/Users/First Last/bin/estafette"),
        )
        .expect("parsed");
        let entry = only_entry(&entries);
        assert_eq!(
            entry.executable.as_deref(),
            Some("/Users/First Last/bin/estafette")
        );
        assert_eq!(entry.arguments, ["mcp", "--x"]);
        assert!(runs_binary_in_mcp_mode(entry));
    }

    #[test]
    fn spec_3_12_step_2_interpreter_running_a_script_named_like_the_binary_is_not_the_binary() {
        let entries = entries_from_command_lines(
            "  78     1 /usr/bin/python3 scripts/estafette mcp\n",
            &executable_table(78, "/usr/bin/python3"),
        )
        .expect("parsed");
        let entry = only_entry(&entries);
        assert_eq!(entry.executable.as_deref(), Some("/usr/bin/python3"));
        assert!(!runs_binary_in_mcp_mode(entry));
    }

    #[test]
    fn spec_3_12_step_2_binary_started_by_bare_name_has_its_arguments_after_the_name() {
        let entries = entries_from_command_lines(
            "  79     1 estafette mcp\n",
            &executable_table(79, "/opt/bin/estafette"),
        )
        .expect("parsed");
        let entry = only_entry(&entries);
        assert_eq!(entry.arguments, ["mcp"]);
        assert!(runs_binary_in_mcp_mode(entry));
    }

    #[test]
    fn spec_3_12_step_2_process_without_an_os_executable_path_is_kept_without_one() {
        let entries = entries_from_command_lines("  80     1 estafette mcp\n", &BTreeMap::new())
            .expect("parsed");
        let entry = only_entry(&entries);
        assert_eq!(entry.executable, None);
        assert_eq!(entry.arguments, ["mcp"]);
        assert!(!runs_binary_in_mcp_mode(entry));
    }

    #[test]
    fn spec_3_12_step_6_harness_whose_executable_cannot_be_resolved_is_still_running() {
        let harness_without_executable = ProcessEntry {
            executable: None,
            ..entry(100, 1, "", &[])
        };
        let snapshots = vec![
            vec![harness_without_executable.clone()],
            vec![harness_without_executable],
            vec![],
        ];
        let reports = collect_reports(100, &runtime_inputs(), snapshots);
        assert_eq!(reports.len(), 2);
    }

    #[test]
    fn spec_3_12_step_2_ps_listing_without_a_parent_pid_is_refused() {
        let refusal = entries_from_command_lines("  77 1 /bin/sh -c run\n  88\n", &BTreeMap::new())
            .expect_err("refused");
        assert_eq!(refusal.to_string(), "ps printed a line it cannot read");
    }

    #[test]
    fn spec_3_12_step_2_a_refused_ps_line_is_not_echoed_so_no_process_arguments_leak() {
        const ARGUMENT_SECRET: &str = "hunter2";
        let refusal = entries_from_command_lines(
            &format!("77 not-a-pid /bin/tool --token {ARGUMENT_SECRET}"),
            &BTreeMap::new(),
        )
        .expect_err("refused");
        assert!(!refusal.to_string().contains(ARGUMENT_SECRET));
    }

    const PROPERTY_CASES: u64 = 300;
    const WORD_ALPHABET: &str = "abcdefghijklmnopqrstuvwxyz0123456789-_.";
    const LONGEST_GENERATED_WORD: u64 = 8;
    const MOST_GENERATED_PATH_SEGMENTS: u64 = 3;
    const MOST_GENERATED_ARGUMENTS: u64 = 4;
    const MOST_GENERATED_PADDING: u64 = 4;

    struct SeededGenerator(u64);

    impl SeededGenerator {
        fn below(&mut self, bound: u64) -> u64 {
            let bound = usize::try_from(bound).unwrap_or_default();
            u64::try_from(next_below(&mut self.0, bound)).unwrap_or_default()
        }

        fn word(&mut self) -> String {
            let alphabet_size = u64::try_from(WORD_ALPHABET.len()).unwrap_or(1);
            let length = self.below(LONGEST_GENERATED_WORD).saturating_add(1);
            (0..length)
                .map(|_| {
                    let index = usize::try_from(self.below(alphabet_size)).unwrap_or_default();
                    WORD_ALPHABET.chars().nth(index).unwrap_or('a')
                })
                .collect()
        }

        fn path_segment(&mut self) -> String {
            let first_word = self.word();
            if self.below(2) == 0 {
                first_word
            } else {
                format!("{first_word} {}", self.word())
            }
        }

        fn absolute_path_that_may_contain_spaces(&mut self) -> String {
            let segment_count = self.below(MOST_GENERATED_PATH_SEGMENTS).saturating_add(1);
            let segments: Vec<String> = (0..segment_count).map(|_| self.path_segment()).collect();
            format!("/{}", segments.join("/"))
        }

        fn arguments(&mut self) -> Vec<String> {
            let argument_count = self.below(MOST_GENERATED_ARGUMENTS);
            (0..argument_count).map(|_| self.word()).collect()
        }

        fn pid(&mut self) -> u32 {
            u32::try_from(self.below(u64::from(u32::MAX))).unwrap_or_default()
        }
    }

    #[test]
    fn spec_3_12_step_2_generated_ps_lines_yield_their_pids_executable_and_arguments() {
        for seed in 0..PROPERTY_CASES {
            let mut generator = SeededGenerator(seed);
            let (pid, parent_pid) = (generator.pid(), generator.pid());
            let executable = generator.absolute_path_that_may_contain_spaces();
            let arguments = generator.arguments();
            let padding = " ".repeat(
                usize::try_from(generator.below(MOST_GENERATED_PADDING)).unwrap_or_default(),
            );
            let mut rendered_arguments = String::new();
            for argument in &arguments {
                rendered_arguments.push(' ');
                rendered_arguments.push_str(argument);
            }
            let line = format!("{padding}{pid} {parent_pid} {executable}{rendered_arguments}\n");
            let entries = entries_from_command_lines(&line, &executable_table(pid, &executable))
                .expect("a rendered line parses");
            let entry = only_entry(&entries);
            assert_eq!(
                (entry.pid, entry.parent_pid, entry.executable.as_deref()),
                (Pid(pid), Pid(parent_pid), Some(executable.as_str())),
                "seed {seed}: {line}"
            );
            assert_eq!(entry.arguments, arguments, "seed {seed}: {line}");
        }
    }

    #[test]
    fn spec_3_12_step_2_generated_ps_lines_with_a_non_numeric_parent_pid_are_refused() {
        for seed in 0..PROPERTY_CASES {
            let mut generator = SeededGenerator(seed);
            let line = format!(
                "{} x{} {}",
                generator.pid(),
                generator.word(),
                generator.absolute_path_that_may_contain_spaces()
            );
            let refusal = entries_from_command_lines(&line, &BTreeMap::new()).expect_err("refused");
            assert_eq!(
                refusal.to_string(),
                "ps printed a line it cannot read",
                "seed {seed}: {line}"
            );
        }
    }

    const WHICH_PROGRAM: &str = "which";
    const PERL_PROGRAM: &str = "perl";
    const SHELL_PROGRAM: &str = "sh";
    const SHELL_COMMAND_FLAG: &str = "-c";
    const KILL_PROGRAM: &str = "kill";
    const OTHER_MODE_ARGUMENT: &str = "send";
    const SCRIPTS_DIRECTORY_NAME: &str = "scripts";
    const FAKE_BINARY_PERL_PROGRAM: &str = "select(undef, undef, undef, undef);\n";

    fn fake_tree_script() -> String {
        format!(
            r#"
        "$1" {MCP_MODE_ARGUMENT} >/dev/null & echo $!
        "$1" {OTHER_MODE_ARGUMENT} >/dev/null & echo $!
        "$2" {SCRIPTS_DIRECTORY_NAME}/{BINARY_EXECUTABLE_NAME} {MCP_MODE_ARGUMENT} >/dev/null & echo $!
        {SHELL_PROGRAM} -c '"$1" {MCP_MODE_ARGUMENT} >/dev/null & echo $!; wait' _ "$1" &
        wait
    "#
        )
    }

    struct FakeProcessTree {
        harness: Child,
        mcp_pids: Vec<u32>,
        every_binary_pid: Vec<u32>,
        directory: PathBuf,
    }

    impl FakeProcessTree {
        fn start(directory_label: &str) -> Self {
            let directory =
                std::env::temp_dir().join(format!("{directory_label}-{}", std::process::id()));
            std::fs::create_dir_all(&directory).expect("create fake binary directory");
            let fake_binary = directory.join(BINARY_EXECUTABLE_NAME);
            let perl_path = Command::new(WHICH_PROGRAM)
                .arg(PERL_PROGRAM)
                .output()
                .expect("run which")
                .stdout;
            let perl_path = String::from_utf8(perl_path).expect("utf8 path");
            std::fs::copy(perl_path.trim(), &fake_binary).expect("copy perl");
            for mode_argument in [MCP_MODE_ARGUMENT, OTHER_MODE_ARGUMENT] {
                std::fs::write(directory.join(mode_argument), FAKE_BINARY_PERL_PROGRAM)
                    .expect("write the perl program named after a mode argument");
            }
            let scripts_directory = directory.join(SCRIPTS_DIRECTORY_NAME);
            std::fs::create_dir_all(&scripts_directory).expect("create scripts directory");
            std::fs::write(
                scripts_directory.join(BINARY_EXECUTABLE_NAME),
                FAKE_BINARY_PERL_PROGRAM,
            )
            .expect("write a perl script named like the binary");
            let mut harness = Command::new(SHELL_PROGRAM)
                .args([SHELL_COMMAND_FLAG, fake_tree_script().as_str(), "_"])
                .arg(&fake_binary)
                .arg(perl_path.trim())
                .current_dir(&directory)
                .stdout(Stdio::piped())
                .spawn()
                .expect("start the fake harness");
            let stdout = harness.stdout.take().expect("harness stdout");
            let mut lines = BufReader::new(stdout).lines();
            let mut next_pid = || -> u32 {
                lines
                    .next()
                    .expect("a pid line")
                    .expect("readable line")
                    .trim()
                    .parse()
                    .expect("a pid")
            };
            let direct_mcp = next_pid();
            let direct_send = next_pid();
            let interpreter_running_script = next_pid();
            let nested_mcp = next_pid();
            Self {
                harness,
                mcp_pids: vec![direct_mcp, nested_mcp],
                every_binary_pid: vec![
                    direct_mcp,
                    direct_send,
                    interpreter_running_script,
                    nested_mcp,
                ],
                directory,
            }
        }

        fn stop_harness(&mut self) {
            self.harness.kill().expect("stop the harness");
            self.harness.wait().expect("reap the harness");
        }
    }

    impl Drop for FakeProcessTree {
        fn drop(&mut self) {
            for pid in &self.every_binary_pid {
                if let Err(error) = Command::new(KILL_PROGRAM).arg(pid.to_string()).status() {
                    eprintln!("could not stop the fake process {pid}: {error}");
                }
            }
            if let Err(error) = std::fs::remove_dir_all(&self.directory) {
                eprintln!("could not remove {}: {error}", self.directory.display());
            }
        }
    }

    #[test]
    fn spec_3_12_steps_2_5_6_ps_table_finds_mcp_binaries_in_a_real_process_tree_and_stops_with_harness()
     {
        assert_ps_table_finds_mcp_binaries_installed_in("estafette-discover");
    }

    #[test]
    fn spec_3_12_step_2_ps_table_finds_a_binary_installed_under_a_path_with_a_space() {
        assert_ps_table_finds_mcp_binaries_installed_in("discover with space");
    }

    fn assert_ps_table_finds_mcp_binaries_installed_in(directory_label: &str) {
        let mut tree = FakeProcessTree::start(directory_label);
        let mut expected_pids = tree.mcp_pids.clone();
        expected_pids.sort_unstable();
        let expected: Vec<PathBuf> = expected_pids
            .iter()
            .map(|pid| PathBuf::from(format!("/run/user/1000/estafette-1000/{pid}.sock")))
            .collect();
        let mut polls = 0_u32;
        let matched = Cell::new(false);
        let harness_pid = Pid(tree.harness.id());
        follow_binary_sockets(
            harness_pid,
            &runtime_inputs(),
            &PsProcessTable,
            &mut |_| {
                polls = polls.saturating_add(1);
                if matched.get() || polls >= TREE_STARTUP_POLL_LIMIT {
                    tree.stop_harness();
                }
            },
            &mut |sockets| matched.set(matched.get() || sockets == expected),
        )
        .expect("ps works");
        assert!(matched.get(), "no poll reported exactly {expected:?}");
    }
}
