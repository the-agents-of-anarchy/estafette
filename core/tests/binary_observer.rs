use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

mod common;
#[cfg(test)]
#[path = "support/file_growth.rs"]
mod file_growth;
#[cfg(test)]
#[path = "support/member_events.rs"]
mod member_events;
#[cfg(test)]
#[path = "support/node_start.rs"]
mod node_start;
#[path = "common/remove_scratch.rs"]
mod remove_scratch;
#[cfg(test)]
#[path = "support/trace_files.rs"]
mod trace_files;
#[cfg(test)]
#[path = "support/trace_reader.rs"]
mod trace_reader;

use common::this_process_directory;
use member_events::{COUNTERS_EVENT, MemberEvents};
use node_start::{NODE_START_MOMENT, is_counters_snapshot_of_moment};

const SHORT_SCRATCH_PARENT: &str = "/tmp";
const SCRATCH_PREFIX: &str = "estafette-binary-observer";
const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
const NETWORK: &str = "lab";
const ABSENT_NETWORK: &str = "absent";
const HANDLE: &str = "bob";
const BODY: &str = "hello";
const INTERRUPTED_COMMAND_ENDS_WITHIN: Duration = Duration::from_millis(1500);
const SIGINT_NUMBER: i32 = 2;
const DATA_HOME: &str = "data";
const NODE_RUNS_WITHIN: Duration = Duration::from_secs(10);
const COMMAND_LABEL: &str = "the command";
/// The command that sends a process a signal.
const SIGNAL_COMMAND: &str = "kill";
/// The `kill` flag of SIGINT, the signal whose prompt end spec 3.13 asks of an interrupted command.
const INTERRUPT_SIGNAL_FLAG: &str = "-INT";

fn binary(root: &Path) -> Command {
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join(DATA_HOME))
        .env("XDG_RUNTIME_DIR", root.join("runtime"));
    command
}

fn run_binary(root: &Path, arguments: &[&str]) -> io::Result<Output> {
    binary(root).args(arguments).output()
}

fn short_scratch_root(test_name: &str) -> io::Result<PathBuf> {
    common::this_process_directory::fresh_directory_of_this_process_in(
        Path::new(SHORT_SCRATCH_PARENT),
        &format!("eo-{test_name}"),
    )
}

/// A scratch root removed on every way out of the scope that holds it, a removal that fails failing the test unless it is already failing, and then reported on standard error.
struct ScratchRemovedOnDrop {
    path: PathBuf,
}

impl Drop for ScratchRemovedOnDrop {
    fn drop(&mut self) {
        if let Err(failure) = remove_scratch::remove_scratch(&self.path) {
            let report = format!("scratch not removed: {failure}");
            assert!(std::thread::panicking(), "{report}");
            drop(writeln!(io::stderr(), "{report}"));
        }
    }
}

/// Sends the signal to the process, failing with an error naming the signal and the process id when `kill` does not succeed.
fn signal_process(signal: &str, process_id: u32) -> io::Result<()> {
    let sent = Command::new(SIGNAL_COMMAND)
        .args([signal, &process_id.to_string()])
        .status()?;
    if sent.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "kill {signal} {process_id}: {sent}"
        )))
    }
}

fn create_network(root: &Path) -> io::Result<Output> {
    run_binary(root, &["network", "create", NETWORK])
}

fn assert_refused_naming_the_missing_configuration(root: &Path, output: &Output) {
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, Vec::<u8>::new());
    let announcement = trace_files::announcement_of_one_run(&root.join(DATA_HOME));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.starts_with(&announcement),
        "{announcement:?}: {stderr}"
    );
    let message = stderr
        .get(announcement.len()..)
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.starts_with(&root.display().to_string()),
        "{message}"
    );
    assert!(message.ends_with('\n'), "{message}");
    assert_eq!(message.lines().count(), 1, "{message}");
}

#[test]
fn binary_show_peers_on_a_network_without_configuration_is_refused_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "show_peers_absent").unwrap();

    let output = run_binary(&root, &["show-peers", ABSENT_NETWORK]).unwrap();

    assert_refused_naming_the_missing_configuration(&root, &output);
}

#[test]
fn binary_tail_on_a_network_without_configuration_is_refused_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "tail_absent").unwrap();

    let output = run_binary(&root, &["tail", ABSENT_NETWORK]).unwrap();

    assert_refused_naming_the_missing_configuration(&root, &output);
}

#[test]
fn binary_test_send_on_a_network_without_configuration_is_refused_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "test_send_absent").unwrap();

    let output = run_binary(&root, &["test-send", ABSENT_NETWORK, HANDLE, BODY]).unwrap();

    assert_refused_naming_the_missing_configuration(&root, &output);
}

#[test]
fn binary_show_peers_on_a_network_without_members_prints_nothing_and_exits_zero_of_spec_3_15() {
    let root = common::scratch_root(SCRATCH_PREFIX, "show_peers_empty").unwrap();
    assert_eq!(create_network(&root).unwrap().status.code(), Some(0));

    let output = run_binary(&root, &["show-peers", NETWORK]).unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        trace_files::announcement_of_one_run(&root.join(DATA_HOME))
    );
}

#[test]
fn binary_tail_runs_until_interrupted_then_exits_zero_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "tail_interrupted").unwrap();
    assert_eq!(create_network(&root).unwrap().status.code(), Some(0));
    let mut tail = binary(&root)
        .args(["tail", NETWORK])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut errors = BufReader::new(tail.stderr.take().unwrap());

    ended_when_the_wait_fails(&mut tail, || {
        wait_for_the_node_start_snapshot(&root, &mut errors)
    })
    .unwrap();
    assert!(tail.try_wait().unwrap().is_none());
    let tail_id = tail.id();
    ended_when_the_wait_fails(&mut tail, || signal_process(INTERRUPT_SIGNAL_FLAG, tail_id))
        .unwrap();
    let output = tail.wait_with_output().unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, Vec::<u8>::new());
}

#[test]
fn binary_test_send_creates_the_operator_card_and_reports_an_unknown_handle_of_spec_3_13() {
    let root = short_scratch_root("test_send").unwrap();
    assert_eq!(create_network(&root).unwrap().status.code(), Some(0));

    let output = run_binary(&root, &["test-send", NETWORK, HANDLE, BODY]).unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "{}the message was not delivered: No present member has that handle.\n",
            trace_files::announcement_of_one_run(&root.join(DATA_HOME))
        )
    );
    let listed = run_binary(&root, &["list-cards", NETWORK]).unwrap();
    assert!(
        String::from_utf8(listed.stdout)
            .unwrap()
            .starts_with("operator\t")
    );
    remove_scratch::remove_scratch(&root).unwrap();
}

/// Runs the wait, and when it fails, by an error or a panic, kills and reaps the command before the failure leaves the test.
fn ended_when_the_wait_fails(
    command: &mut Child,
    wait: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let waited = panic::catch_unwind(AssertUnwindSafe(wait));
    if !matches!(waited, Ok(Ok(()))) {
        drop(command.kill());
        drop(command.wait());
    }
    waited.unwrap_or_else(|failure| panic::resume_unwind(failure))
}

/// Reads the first line the command writes on standard error, which names its trace files once their directory exists (spec/observability.md decision 3).
fn read_the_announcement(errors: &mut dyn BufRead) -> io::Result<()> {
    let mut announcement = String::new();
    errors.read_line(&mut announcement)?;
    Ok(())
}

/// Waits until the command records in its events file the counters snapshot of its node's start, the event that says its node runs (spec/observability.md decision 9).
fn wait_for_the_node_start_snapshot(root: &Path, errors: &mut dyn BufRead) -> io::Result<()> {
    read_the_announcement(errors)?;
    MemberEvents::new(
        COMMAND_LABEL,
        &trace_files::traces_directory(&root.join(DATA_HOME)),
    )
    .wait_until(
        &format!("{COUNTERS_EVENT} of its node's {NODE_START_MOMENT}"),
        NODE_RUNS_WITHIN,
        |lines| {
            lines
                .iter()
                .any(|line| is_counters_snapshot_of_moment(line, NODE_START_MOMENT))
                .then_some(())
        },
    );
    Ok(())
}

/// Starts the command on a fresh network, sends it SIGINT once `node_runs` returns, and says how it ended, its own status when it ended first, none when it outlived the bound.
fn status_after_sigint(
    test_name: &str,
    arguments: &[&str],
    node_runs: fn(&Path, &mut dyn BufRead) -> io::Result<()>,
) -> io::Result<Option<ExitStatus>> {
    let scratch = ScratchRemovedOnDrop {
        path: short_scratch_root(test_name)?,
    };
    let root = scratch.path.as_path();
    let created = create_network(root)?;
    if !created.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&created.stderr).into_owned(),
        ));
    }
    let mut command = binary(root)
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut errors = BufReader::new(
        command
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("the command's standard error is not piped"))?,
    );
    ended_when_the_wait_fails(&mut command, || node_runs(root, &mut errors))?;
    if let Some(ended_first) = command.try_wait()? {
        return Ok(Some(ended_first));
    }
    let command_id = command.id();
    ended_when_the_wait_fails(&mut command, || {
        signal_process(INTERRUPT_SIGNAL_FLAG, command_id)
    })?;
    let ended = exit_within(command, INTERRUPTED_COMMAND_ENDS_WITHIN)?;
    drop(errors);
    Ok(ended)
}

#[test]
fn binary_test_send_interrupted_by_sigint_ends_at_once_by_that_signal_of_spec_3_13() {
    let ended = status_after_sigint(
        "test_send_sigint",
        &["test-send", NETWORK, HANDLE, BODY],
        wait_for_the_node_start_snapshot,
    )
    .unwrap();

    assert_eq!(
        ended.map(|status| status.signal()),
        Some(Some(SIGINT_NUMBER)),
        "the command did not end by SIGINT within {INTERRUPTED_COMMAND_ENDS_WITHIN:?} of it: {ended:?}"
    );
}

#[test]
fn binary_doctor_interrupted_by_sigint_ends_at_once_by_that_signal_of_spec_3_13() {
    let ended = status_after_sigint(
        "doctor_sigint",
        &["doctor", NETWORK],
        wait_for_the_node_start_snapshot,
    )
    .unwrap();

    assert_eq!(
        ended.map(|status| status.signal()),
        Some(Some(SIGINT_NUMBER)),
        "the command did not end by SIGINT within {INTERRUPTED_COMMAND_ENDS_WITHIN:?} of it: {ended:?}"
    );
}

/// The event the server records for each client its inbox socket accepts (spec/observability.md decision 9).
const CLIENT_CONNECTED_EVENT: &str = "inbox.client_connected";
/// The event the server records when a card is claimed (spec/observability.md decision 9).
const CLAIMED_EVENT: &str = "process.claimed";
/// The first line an inbox client writes to read every frame from the first (spec 3.11).
const SPEC_SINCE_EVERY_FRAME: &str = "{\"since\": 0}\n";
/// How long the server may take to bind its inbox socket once it starts.
const INBOX_SOCKET_BOUND_WITHIN: Duration = Duration::from_secs(10);
/// How long a server with no card claimed may take to end once its standard input ends.
const UNCLAIMED_SERVER_ENDS_WITHIN: Duration = Duration::from_secs(10);

/// The inbox socket of the process, `<runtime>/estafette-<user>/<pid>.sock`, under a scratch root the current user owns (spec 3.3).
fn inbox_socket_of(root: &Path, process_id: u32) -> io::Result<PathBuf> {
    let user_id = std::fs::metadata(root)?.uid();
    Ok(root
        .join("runtime")
        .join(format!("estafette-{user_id}"))
        .join(format!("{process_id}.sock")))
}

/// Connects to the socket as soon as it listens, the thread yielding between two tries and never sleeping, failing with the socket and the bound when it never does.
fn connect_once_listening(socket: &Path, within: Duration) -> io::Result<UnixStream> {
    let tried = Instant::now();
    loop {
        match UnixStream::connect(socket) {
            Ok(client) => return Ok(client),
            Err(failure) if tried.elapsed() >= within => {
                return Err(io::Error::other(format!(
                    "nothing listened on {} within {within:?}: {failure}",
                    socket.display()
                )));
            }
            Err(_) => std::thread::yield_now(),
        }
    }
}

#[test]
fn binary_mcp_inbox_socket_accepts_a_client_before_any_card_is_claimed_of_spec_3_11() {
    let scratch = ScratchRemovedOnDrop {
        path: short_scratch_root("mcp_inbox_before_init").unwrap(),
    };
    let root = scratch.path.as_path();
    assert_eq!(create_network(root).unwrap().status.code(), Some(0));
    let mut server = binary(root)
        .args(["mcp", NETWORK])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut errors = BufReader::new(server.stderr.take().unwrap());
    let socket = inbox_socket_of(root, server.id()).unwrap();
    let mut events_when_connected = Vec::new();

    ended_when_the_wait_fails(&mut server, || {
        read_the_announcement(&mut errors)?;
        let mut client = connect_once_listening(&socket, INBOX_SOCKET_BOUND_WITHIN)?;
        client.write_all(SPEC_SINCE_EVERY_FRAME.as_bytes())?;
        events_when_connected = MemberEvents::new(
            COMMAND_LABEL,
            &trace_files::traces_directory(&root.join(DATA_HOME)),
        )
        .wait_until(CLIENT_CONNECTED_EVENT, NODE_RUNS_WITHIN, |lines| {
            lines
                .iter()
                .any(|line| line.event == CLIENT_CONNECTED_EVENT)
                .then(|| lines.into_iter().map(|line| line.event).collect())
        });
        Ok(())
    })
    .unwrap();
    drop(server.stdin.take());
    let ended = exit_within(server, UNCLAIMED_SERVER_ENDS_WITHIN).unwrap();

    assert!(
        !events_when_connected.contains(&CLAIMED_EVENT.to_owned()),
        "{events_when_connected:?}"
    );
    assert_eq!(ended.map(|status| status.code()), Some(Some(0)));
}

/// How the command exited when its handle reaps it within the bound, none when it outlives it and is killed through that handle, the thread yielding between two asks and never sleeping.
fn exit_within(mut command: Child, bound: Duration) -> io::Result<Option<ExitStatus>> {
    let asked = Instant::now();
    loop {
        if let Some(status) = command.try_wait()? {
            return Ok(Some(status));
        }
        if asked.elapsed() >= bound {
            command.kill()?;
            command.wait()?;
            return Ok(None);
        }
        std::thread::yield_now();
    }
}

#[test]
fn binary_help_describes_each_observer_command_and_leaves_the_program_description_alone_of_spec_3_13()
 {
    let root = common::scratch_root(SCRATCH_PREFIX, "help").unwrap();

    let help = run_binary(&root, &["--help"]).unwrap();
    let tail_help = run_binary(&root, &["tail", "--help"]).unwrap();

    let help_text = String::from_utf8_lossy(&help.stdout).into_owned();
    let described = |name: &str, words: &str| {
        help_text
            .lines()
            .any(|line| line.trim_start().starts_with(name) && line.contains(words))
    };
    assert!(described("show-peers", "Join the network without a card"));
    assert!(described("tail", "print each gossip payload"));
    assert!(described(
        "test-send",
        "Send one message as the reserved operator card"
    ));
    assert!(!help_text.contains("The operator commands"));
    assert!(
        String::from_utf8_lossy(&tail_help.stdout).contains("The network whose gossip to print")
    );
}
