use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use common::Fixture;
use estafette_bridge_claude::discover::Pid;
use estafette_bridge_claude::doctor::dropped_frames_path;
use estafette_trace::TraceDirectory;

mod common;

const NETWORK: &str = "lab";
const CARD: &str = "ana-card";
const HANDLE: &str = "ana";
const DOCTOR_CHECK_COUNT: usize = 5;
const BRIDGE_START_DEADLINE: Duration = Duration::from_secs(30);

fn wait_until_the_bridge_has_started(fixture: &Fixture) {
    let dropped_frames_file =
        dropped_frames_path(&fixture.temporary_directory(), Pid(std::process::id()));
    let started_waiting = Instant::now();
    while !dropped_frames_file.exists() {
        assert!(
            started_waiting.elapsed() < BRIDGE_START_DEADLINE,
            "the bridge wrote no dropped-frames file {} within {BRIDGE_START_DEADLINE:?}",
            dropped_frames_file.display()
        );
        thread::yield_now();
    }
}

#[test]
fn spec_3_12_doctor_passes_every_check_for_a_bridge_beside_a_real_binary_serving_mcp() {
    let fixture = Fixture::new().unwrap();
    fixture
        .run_estafette_successfully(&["network", "create", NETWORK])
        .unwrap();
    fixture
        .run_estafette_successfully(&["create-card", NETWORK, CARD, HANDLE])
        .unwrap();
    let _messaging_socket = UnixListener::bind(&fixture.messaging_socket).unwrap();
    let member = fixture.spawn_claimed_member(NETWORK, CARD).unwrap();
    assert_eq!(member.claimed["ok"], true, "{}", member.claimed);
    assert_eq!(
        member.claimed["you"]["handle"], HANDLE,
        "{}",
        member.claimed
    );
    let _bridge = fixture.spawn_bridge().unwrap();

    wait_until_the_bridge_has_started(&fixture);
    let doctor = fixture.bridge().arg("doctor").output().unwrap();
    let report = String::from_utf8(doctor.stdout).unwrap();
    assert_eq!(doctor.status.code(), Some(0), "{report}");
    assert_eq!(
        report.lines().filter(|line| line.starts_with("ok")).count(),
        DOCTOR_CHECK_COUNT,
        "{report}"
    );
}

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const PERMISSION_BITS: u32 = 0o777;
const SPEC_RUN_HEX_CHARS: usize = 16;
const USAGE_EXIT_CODE: i32 = 2;

/// The pid of a process that has already exited, so a bridge started for it serves nothing and ends.
fn exited_harness_pid() -> io::Result<String> {
    let mut exited = Command::new("true").spawn()?;
    exited.wait()?;
    Ok(exited.id().to_string())
}

fn run_bridge(fixture: &Fixture, words: &[&str]) -> io::Result<Output> {
    fixture.bridge().args(words).output()
}

fn traces_directory(fixture: &Fixture) -> PathBuf {
    fixture.data_home().join("estafette").join("traces")
}

fn mode_of(path: &Path) -> io::Result<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & PERMISSION_BITS)
}

#[test]
fn observability_decision_3_the_binary_names_its_events_file_first_on_standard_error_and_writes_it_under_the_data_directory()
 {
    let fixture = Fixture::new().unwrap();

    let output = run_bridge(&fixture, &["run", &exited_harness_pid().unwrap()]).unwrap();

    let errors = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{errors}");
    let first_line = errors.lines().next().unwrap();
    let events_file = PathBuf::from(
        first_line
            .strip_prefix("writing the debug trace: ")
            .and_then(|rest| rest.strip_suffix(" (safe to post)"))
            .unwrap_or_else(|| panic!("first line names no events file: {first_line}")),
    );
    assert_eq!(
        events_file.parent(),
        Some(traces_directory(&fixture).as_path())
    );
    let file_name = events_file.file_name().unwrap().to_str().unwrap();
    let run_hex = file_name
        .strip_suffix("-bridge.events.jsonl")
        .and_then(|run_name| run_name.split('-').nth(1))
        .unwrap_or_else(|| panic!("not a bridge events file: {file_name}"));
    assert_eq!(run_hex.len(), SPEC_RUN_HEX_CHARS, "{file_name}");
    assert_eq!(
        mode_of(&traces_directory(&fixture)).unwrap(),
        PRIVATE_DIRECTORY_MODE
    );
    assert_eq!(mode_of(&events_file).unwrap(), PRIVATE_FILE_MODE);
    let events = fs::read_to_string(&events_file).unwrap();
    let first_event: serde_json::Value =
        serde_json::from_str(events.lines().next().unwrap()).unwrap();
    assert_eq!(first_event["event"], "process.started", "{events}");
    assert_eq!(first_event["run"], run_hex, "{events}");
    assert_eq!(first_event["n"], 0, "{events}");
    assert!(first_event.get("node").is_none(), "{events}");
}

#[test]
fn observability_decision_3_the_binary_run_with_no_debug_names_no_trace_and_creates_no_trace_directory()
 {
    let fixture = Fixture::new().unwrap();

    let output = run_bridge(
        &fixture,
        &["run", &exited_harness_pid().unwrap(), "--no-debug"],
    )
    .unwrap();

    let errors = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{errors}");
    assert!(!errors.contains("debug trace"), "{errors}");
    assert!(!fixture.data_home().exists());
}

#[test]
fn observability_decision_3_the_binary_prints_the_help_of_run_with_the_trace_it_writes() {
    let fixture = Fixture::new().unwrap();

    let output = run_bridge(&fixture, &["run", "--help"]).unwrap();

    let help = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(0), "{help}");
    assert!(help.contains(&TraceDirectory::described()), "{help}");
    assert!(help.contains("safe to post"), "{help}");
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "");
}

#[test]
fn spec_3_12_the_binary_refuses_a_malformed_command_line_with_the_usage_naming_every_form() {
    let fixture = Fixture::new().unwrap();

    let output = run_bridge(&fixture, &["run", "--no-debug", "4242"]).unwrap();

    assert_eq!(output.status.code(), Some(USAGE_EXIT_CODE));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "usage: estafette-bridge-claude run <harness-pid> [--debug | --no-debug] | run --help | doctor\n"
    );
    assert!(!fixture.data_home().exists());
}
