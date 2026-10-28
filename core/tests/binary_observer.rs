use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread::sleep;
use std::time::Duration;

mod common;

const SHORT_SCRATCH_PARENT: &str = "/tmp";
const SCRATCH_PREFIX: &str = "estafette-binary-observer";
const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
const NETWORK: &str = "lab";
const ABSENT_NETWORK: &str = "absent";
const HANDLE: &str = "bob";
const BODY: &str = "hello";
const TAIL_RUNNING_TIME: Duration = Duration::from_secs(2);

fn binary(root: &Path) -> Command {
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"));
    command
}

fn run_binary(root: &Path, arguments: &[&str]) -> io::Result<Output> {
    binary(root).args(arguments).output()
}

fn short_scratch_root() -> PathBuf {
    PathBuf::from(SHORT_SCRATCH_PARENT).join(format!("eo-{}", std::process::id()))
}

fn create_network(root: &Path) -> io::Result<Output> {
    run_binary(root, &["network", "create", NETWORK])
}

fn assert_refused_naming_the_missing_configuration(root: &Path, output: &Output) {
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, Vec::<u8>::new());
    let message = String::from_utf8_lossy(&output.stderr).into_owned();
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
    assert_eq!(output.stderr, Vec::<u8>::new());
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
    let root = short_scratch_root();
    fs::create_dir_all(&root).unwrap();
    assert_eq!(create_network(&root).unwrap().status.code(), Some(0));

    let output = run_binary(&root, &["test-send", NETWORK, HANDLE, BODY]).unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, Vec::<u8>::new());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "the message was not delivered: No present member has that handle.\n"
    );
    let listed = run_binary(&root, &["list-cards", NETWORK]).unwrap();
    assert!(
        String::from_utf8(listed.stdout)
            .unwrap()
            .starts_with("operator\t")
    );
    drop(fs::remove_dir_all(&root));
}
