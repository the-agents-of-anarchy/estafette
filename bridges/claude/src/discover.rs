use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::Duration;

pub const POLL_INTERVAL: Duration = Duration::from_millis(2000);
const BINARY_EXECUTABLE_NAME: &str = "estafette";
const MCP_MODE_ARGUMENT: &str = "mcp";
const RUNTIME_DIRECTORY_PREFIX: &str = "estafette-";
const DEFAULT_TEMPORARY_DIRECTORY: &str = "/tmp";
const SOCKET_FILE_SUFFIX: &str = ".sock";
const PS_LISTING_ARGUMENTS: [&str; 3] = ["-A", "-o", "pid=,ppid=,args="];

#[derive(Clone, Debug)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent_pid: u32,
    pub executable: String,
    pub arguments: Vec<String>,
}

#[derive(Debug)]
pub enum ProcessTableError {
    ListingFailed(std::io::Error),
    ListingExitedUnsuccessfully(ExitStatus),
    MalformedListingLine(String),
}

impl fmt::Display for ProcessTableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListingFailed(source) => write!(formatter, "cannot run ps: {source}"),
            Self::ListingExitedUnsuccessfully(status) => write!(formatter, "ps failed: {status}"),
            Self::MalformedListingLine(line) => {
                write!(
                    formatter,
                    "ps printed a line without pid and parent pid: {line}"
                )
            }
        }
    }
}

impl std::error::Error for ProcessTableError {}

pub struct PsProcessTable;

impl ProcessTable for PsProcessTable {
    fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError> {
        let listing = Command::new("ps")
            .args(PS_LISTING_ARGUMENTS)
            .output()
            .map_err(ProcessTableError::ListingFailed)?;
        if !listing.status.success() {
            return Err(ProcessTableError::ListingExitedUnsuccessfully(
                listing.status,
            ));
        }
        parse_ps_listing(&String::from_utf8_lossy(&listing.stdout))
    }
}

fn parse_ps_listing(listing: &str) -> Result<Vec<ProcessEntry>, ProcessTableError> {
    listing
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_ps_line)
        .collect()
}

fn parse_ps_line(line: &str) -> Result<ProcessEntry, ProcessTableError> {
    let malformed = || ProcessTableError::MalformedListingLine(line.to_owned());
    let mut tokens = line.split_whitespace();
    let pid = tokens
        .next()
        .and_then(|token| token.parse().ok())
        .ok_or_else(malformed)?;
    let parent_pid = tokens
        .next()
        .and_then(|token| token.parse().ok())
        .ok_or_else(malformed)?;
    let executable = tokens.next().unwrap_or_default().to_owned();
    let arguments = tokens.map(str::to_owned).collect();
    Ok(ProcessEntry {
        pid,
        parent_pid,
        executable,
        arguments,
    })
}

pub trait ProcessTable {
    /// Lists every process of the machine with its parent, executable and arguments.
    fn list(&self) -> Result<Vec<ProcessEntry>, ProcessTableError>;
}

pub struct RuntimeDirectoryInputs {
    pub xdg_runtime_dir: Option<PathBuf>,
    pub temporary_dir: Option<PathBuf>,
    pub user_id: u32,
}

/// Reports the inbox socket paths of the binary's MCP-mode processes under the harness at every poll, until the harness exits.
pub fn follow_binary_sockets(
    harness_pid: u32,
    runtime_inputs: &RuntimeDirectoryInputs,
    processes: &impl ProcessTable,
    wait_for_next_poll: &mut impl FnMut(),
    report_sockets: &mut impl FnMut(&[PathBuf]),
) -> Result<(), ProcessTableError> {
    let runtime_dir = runtime_directory(runtime_inputs);
    loop {
        let entries = processes.list()?;
        if !is_running(harness_pid, &entries) {
            return Ok(());
        }
        let sockets: Vec<PathBuf> = binary_pids_under(harness_pid, &entries)
            .into_iter()
            .map(|pid| socket_path(&runtime_dir, pid))
            .collect();
        report_sockets(&sockets);
        wait_for_next_poll();
    }
}

fn runtime_directory(inputs: &RuntimeDirectoryInputs) -> PathBuf {
    let base = inputs
        .xdg_runtime_dir
        .clone()
        .or_else(|| inputs.temporary_dir.clone())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_TEMPORARY_DIRECTORY));
    base.join(format!("{RUNTIME_DIRECTORY_PREFIX}{}", inputs.user_id))
}

fn socket_path(runtime_dir: &Path, pid: u32) -> PathBuf {
    runtime_dir.join(format!("{pid}{SOCKET_FILE_SUFFIX}"))
}

fn is_running(pid: u32, entries: &[ProcessEntry]) -> bool {
    entries.iter().any(|entry| entry.pid == pid)
}

fn binary_pids_under(harness_pid: u32, entries: &[ProcessEntry]) -> BTreeSet<u32> {
    let descendants = descendant_pids(harness_pid, entries);
    entries
        .iter()
        .filter(|entry| descendants.contains(&entry.pid) && runs_binary_in_mcp_mode(entry))
        .map(|entry| entry.pid)
        .collect()
}

fn descendant_pids(harness_pid: u32, entries: &[ProcessEntry]) -> BTreeSet<u32> {
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
    let executable_name = Path::new(&entry.executable).file_name();
    executable_name == Some(OsStr::new(BINARY_EXECUTABLE_NAME))
        && entry.arguments.first().map(String::as_str) == Some(MCP_MODE_ARGUMENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::thread::sleep;

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

    fn entry(pid: u32, parent_pid: u32, executable: &str, arguments: &[&str]) -> ProcessEntry {
        ProcessEntry {
            pid,
            parent_pid,
            executable: executable.to_owned(),
            arguments: arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
        }
    }

    fn runtime_inputs() -> RuntimeDirectoryInputs {
        RuntimeDirectoryInputs {
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
            temporary_dir: Some(PathBuf::from("/var/tmp")),
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
        follow_binary_sockets(harness_pid, inputs, &table, &mut || {}, &mut |sockets| {
            reports.push(sockets.to_vec());
        })
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

    #[test]
    fn spec_3_12_step_5_poll_interval_is_2000_milliseconds() {
        assert_eq!(POLL_INTERVAL.as_millis(), 2000);
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
    fn spec_3_3_runtime_falls_back_to_tmp_without_xdg_runtime_dir_and_temporary_dir() {
        let inputs = RuntimeDirectoryInputs {
            xdg_runtime_dir: None,
            temporary_dir: None,
            ..runtime_inputs()
        };
        let reports = collect_reports(100, &inputs, vec![single_binary_under_harness(), vec![]]);
        assert_eq!(
            reports,
            vec![vec![PathBuf::from("/tmp/estafette-1000/101.sock")]]
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
        let mut waits = 0_u32;
        follow_binary_sockets(
            100,
            &runtime_inputs(),
            &table,
            &mut || waits += 1,
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
        assert_eq!(waits, 3);
    }

    #[test]
    fn spec_3_12_step_6_reports_nothing_when_harness_is_not_running() {
        let reports = collect_reports(
            100,
            &runtime_inputs(),
            vec![vec![entry(101, 1, "/usr/bin/estafette", &["mcp"])]],
        );
        assert_eq!(reports, Vec::<Vec<PathBuf>>::new());
    }

    const TREE_STARTUP_POLL_LIMIT: u32 = 200;
    const TREE_STARTUP_POLL_PAUSE: Duration = Duration::from_millis(25);
    #[test]
    fn spec_3_12_step_2_ps_listing_without_a_parent_pid_is_refused_with_the_line() {
        let refusal = parse_ps_listing("  77 1 /bin/sh -c run\n  88\n").expect_err("refused");
        assert_eq!(
            refusal.to_string(),
            "ps printed a line without pid and parent pid:   88"
        );
    }

    const FAKE_BINARY_PERL_PROGRAM: &str = "sleep 600;\n";
    const FAKE_TREE_SCRIPT: &str = r#"
        "$1" mcp >/dev/null & echo $!
        "$1" send >/dev/null & echo $!
        sh -c '"$1" mcp >/dev/null & echo $!; wait' _ "$1" &
        wait
    "#;

    struct FakeProcessTree {
        harness: Child,
        mcp_pids: Vec<u32>,
        every_binary_pid: Vec<u32>,
        directory: PathBuf,
    }

    impl FakeProcessTree {
        fn start() -> Self {
            let directory =
                std::env::temp_dir().join(format!("estafette-discover-{}", std::process::id()));
            std::fs::create_dir_all(&directory).expect("create fake binary directory");
            let fake_binary = directory.join(BINARY_EXECUTABLE_NAME);
            let perl_path = Command::new("which")
                .arg("perl")
                .output()
                .expect("run which")
                .stdout;
            let perl_path = String::from_utf8(perl_path).expect("utf8 path");
            std::os::unix::fs::symlink(perl_path.trim(), &fake_binary).expect("link perl");
            for mode_argument in [MCP_MODE_ARGUMENT, "send"] {
                std::fs::write(directory.join(mode_argument), FAKE_BINARY_PERL_PROGRAM)
                    .expect("write the perl program named after a mode argument");
            }
            let mut harness = Command::new("sh")
                .args(["-c", FAKE_TREE_SCRIPT, "_"])
                .arg(&fake_binary)
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
            let nested_mcp = next_pid();
            Self {
                harness,
                mcp_pids: vec![direct_mcp, nested_mcp],
                every_binary_pid: vec![direct_mcp, direct_send, nested_mcp],
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
                Command::new("kill")
                    .arg(pid.to_string())
                    .status()
                    .expect("kill a fake binary");
            }
            std::fs::remove_dir_all(&self.directory).expect("remove fake binary directory");
        }
    }

    #[test]
    fn spec_3_12_steps_2_5_6_ps_table_finds_mcp_binaries_in_a_real_process_tree_and_stops_with_harness()
     {
        let mut tree = FakeProcessTree::start();
        let mut expected: Vec<PathBuf> = tree
            .mcp_pids
            .iter()
            .map(|pid| PathBuf::from(format!("/run/user/1000/estafette-1000/{pid}.sock")))
            .collect();
        expected.sort();
        let mut polls = 0_u32;
        let matched = Cell::new(false);
        let harness_pid = tree.harness.id();
        follow_binary_sockets(
            harness_pid,
            &runtime_inputs(),
            &PsProcessTable,
            &mut || {
                polls += 1;
                sleep(TREE_STARTUP_POLL_PAUSE);
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
