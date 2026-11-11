use std::fs;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

mod common;
#[cfg(test)]
#[path = "support/trace_files.rs"]
mod trace_files;

const SHORT_SCRATCH_PARENT: &str = "/tmp";
const SCRATCH_PREFIX: &str = "estafette-binary-observer";
const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
const NETWORK: &str = "lab";
const ABSENT_NETWORK: &str = "absent";
const HANDLE: &str = "bob";
const BODY: &str = "hello";
const TAIL_RUNNING_TIME: Duration = Duration::from_secs(2);
const RUNNING_BEFORE_THE_INTERRUPT: Duration = Duration::from_secs(1);
const INTERRUPTED_COMMAND_ENDS_WITHIN: Duration = Duration::from_millis(1500);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const SIGINT_NUMBER: i32 = 2;
const DATA_HOME: &str = "data";

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

fn short_scratch_root(test_name: &str) -> PathBuf {
    PathBuf::from(SHORT_SCRATCH_PARENT).join(format!("eo-{}-{test_name}", std::process::id()))
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

    sleep(TAIL_RUNNING_TIME);
    assert!(tail.try_wait().unwrap().is_none());
    let interrupt = Command::new("kill")
        .args(["-INT", &tail.id().to_string()])
        .status()
        .unwrap();
    let output = tail.wait_with_output().unwrap();

    assert!(interrupt.success());
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, Vec::<u8>::new());
}

#[test]
fn binary_test_send_creates_the_operator_card_and_reports_an_unknown_handle_of_spec_3_13() {
    let root = short_scratch_root("test_send");
    fs::create_dir_all(&root).unwrap();
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
    drop(fs::remove_dir_all(&root));
}

/// Starts the command on a fresh network, sends it SIGINT once it has run a while, and says how it ended, its own status when it ended first, none when it outlived the bound.
fn status_after_sigint(test_name: &str, arguments: &[&str]) -> io::Result<Option<ExitStatus>> {
    let root = short_scratch_root(test_name);
    fs::create_dir_all(&root)?;
    let created = create_network(&root)?;
    if !created.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&created.stderr).into_owned(),
        ));
    }
    let mut command = binary(&root)
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    sleep(RUNNING_BEFORE_THE_INTERRUPT);
    if let Some(ended_first) = command.try_wait()? {
        return Ok(Some(ended_first));
    }
    let interrupt = Command::new("kill")
        .args(["-INT", &command.id().to_string()])
        .status()?;
    if !interrupt.success() {
        return Err(io::Error::other(format!(
            "kill -INT {}: {interrupt}",
            command.id()
        )));
    }
    let interrupted = Instant::now();
    let ended = loop {
        if let Some(status) = command.try_wait()? {
            break Some(status);
        }
        if interrupted.elapsed() >= INTERRUPTED_COMMAND_ENDS_WITHIN {
            drop(command.kill());
            drop(command.wait());
            break None;
        }
        sleep(EXIT_POLL_INTERVAL);
    };
    drop(fs::remove_dir_all(&root));
    Ok(ended)
}

#[test]
fn binary_test_send_interrupted_by_sigint_ends_at_once_by_that_signal_of_spec_3_13() {
    let ended =
        status_after_sigint("test_send_sigint", &["test-send", NETWORK, HANDLE, BODY]).unwrap();

    assert_eq!(
        ended.map(|status| status.signal()),
        Some(Some(SIGINT_NUMBER)),
        "{ended:?}"
    );
}

#[test]
fn binary_doctor_interrupted_by_sigint_ends_at_once_by_that_signal_of_spec_3_13() {
    let ended = status_after_sigint("doctor_sigint", &["doctor", NETWORK]).unwrap();

    assert_eq!(
        ended.map(|status| status.signal()),
        Some(Some(SIGINT_NUMBER)),
        "{ended:?}"
    );
}

#[test]
fn binary_help_describes_each_observer_command_and_leaves_the_program_description_alone() {
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
