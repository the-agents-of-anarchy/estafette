use std::fs;
use std::io::{self, BufRead};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use common::{Fixture, bridge_alive_line_stamped_from};
use estafette_bridge_claude::discover::Pid;
use estafette_bridge_claude::doctor::dropped_frames_path;
use estafette_trace::{TraceDirectory, events_trace_file_name};

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
    let traces = fixture.traces_directory().unwrap();
    let first_line = errors.lines().next().unwrap();
    let events_file = PathBuf::from(
        first_line
            .strip_prefix("writing the debug trace: ")
            .and_then(|rest| rest.strip_suffix(" (safe to post)"))
            .unwrap_or_else(|| panic!("first line names no events file: {first_line}")),
    );
    assert_eq!(events_file.parent(), Some(traces.as_path()));
    let file_name = events_file.file_name().unwrap().to_str().unwrap();
    let run_hex = file_name
        .strip_suffix("-bridge.events.jsonl")
        .and_then(|run_name| run_name.split('-').nth(1))
        .unwrap_or_else(|| panic!("not a bridge events file: {file_name}"));
    assert_eq!(run_hex.len(), SPEC_RUN_HEX_CHARS, "{file_name}");
    assert_eq!(mode_of(&traces).unwrap(), PRIVATE_DIRECTORY_MODE);
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

const WINDOW_ENDS_WALL_MS: u64 = 1_763_400_000_123;
const BRIDGE_RUN: &str = "20251117T172000Z-a1b2c3d4e5f60718-bridge";
const MEMBER_RUN: &str = "20251117T172000Z-0718a1b2c3d4e5f6-estafette";
const SIZED_AHEAD_ZEROS: [u8; 64] = [0; 64];
const LATER_BY_MS: u64 = 1000;

fn events_line(event: &str, wall_ms: u64) -> String {
    format!(
        "{{\"v\":1,\"run\":\"a1b2c3d4e5f60718\",\"n\":0,\"mono_ms\":0,\"wall_ms\":{wall_ms},\"event\":\"{event}\"}}\n"
    )
}

fn write_events_file(path: &Path, lines: &[String]) -> io::Result<()> {
    fs::write(
        path,
        [lines.concat().as_bytes(), &SIZED_AHEAD_ZEROS].concat(),
    )
}

#[test]
fn observability_decision_13_the_bridge_lab_window_ends_on_a_bridge_alive_line_stamped_at_or_after_its_end_and_on_no_other_line()
 {
    let fixture = Fixture::new().unwrap();
    let traces = fixture.traces_directory().unwrap();
    fs::create_dir_all(&traces).unwrap();
    let window_ends = UNIX_EPOCH
        .checked_add(Duration::from_millis(WINDOW_ENDS_WALL_MS))
        .unwrap();
    let after_the_end = WINDOW_ENDS_WALL_MS.checked_add(LATER_BY_MS).unwrap();
    let before_the_end = WINDOW_ENDS_WALL_MS.checked_sub(1).unwrap();
    let bridge_events = traces.join(events_trace_file_name(BRIDGE_RUN));
    let lines_before_the_end = [
        events_line("process.started", before_the_end),
        events_line("process.alive", before_the_end),
        events_line("notice.written", after_the_end),
    ];
    write_events_file(&bridge_events, &lines_before_the_end).unwrap();
    write_events_file(
        &traces.join(events_trace_file_name(MEMBER_RUN)),
        &[events_line("process.alive", after_the_end)],
    )
    .unwrap();

    assert!(!bridge_alive_line_stamped_from(&traces, window_ends).unwrap());

    write_events_file(
        &bridge_events,
        &[
            lines_before_the_end.concat(),
            events_line("process.alive", WINDOW_ENDS_WALL_MS),
        ],
    )
    .unwrap();

    assert!(bridge_alive_line_stamped_from(&traces, window_ends).unwrap());
}

const GROUP_OTHER_READABLE_DIRECTORY_MODE: u32 = 0o755;
const REFUSED_RUNTIME_DIRECTORY_EXIT_CODE: i32 = 1;
const BRIDGE_EXIT_DEADLINE: Duration = Duration::from_secs(10);

fn runtime_directory_of(fixture: &Fixture) -> io::Result<PathBuf> {
    let user_id = fs::metadata(&fixture.root)?.uid();
    Ok(fixture
        .root
        .join("runtime")
        .join(format!("estafette-{user_id}")))
}

/// A process standing in for the harness, alive while its standard input stays open and until it is dropped.
struct LiveHarness(Child);

impl LiveHarness {
    fn start() -> io::Result<Self> {
        Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .map(Self)
    }
}

impl Drop for LiveHarness {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

/// The output of a bridge serving a harness that stays alive, which must end by itself within the deadline.
fn output_of_a_bridge_that_must_end_by_itself(fixture: &Fixture) -> io::Result<Output> {
    let harness = LiveHarness::start()?;
    let mut bridge = fixture
        .bridge()
        .args(["run", &harness.0.id().to_string(), "--no-debug"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let started_waiting = Instant::now();
    while bridge.try_wait()?.is_none() {
        if started_waiting.elapsed() >= BRIDGE_EXIT_DEADLINE {
            bridge.kill()?;
            bridge.wait()?;
            return Err(io::Error::other(format!(
                "the bridge was still serving {BRIDGE_EXIT_DEADLINE:?} after it started"
            )));
        }
        thread::yield_now();
    }
    bridge.wait_with_output()
}

#[test]
fn spec_3_3_the_bridge_refuses_an_existing_runtime_directory_that_grants_access_to_group_and_other_runtime_directory_is_private_or_refused()
 {
    let fixture = Fixture::new().unwrap();
    let runtime = runtime_directory_of(&fixture).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(
        &runtime,
        fs::Permissions::from_mode(GROUP_OTHER_READABLE_DIRECTORY_MODE),
    )
    .unwrap();

    let output = output_of_a_bridge_that_must_end_by_itself(&fixture).unwrap();

    assert_eq!(
        output.status.code(),
        Some(REFUSED_RUNTIME_DIRECTORY_EXIT_CODE)
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "{} already exists and is mode 0755, which grants access to group or other; it must be a directory of yours with no access for group or other\n",
            runtime.display()
        )
    );
    assert_eq!(
        mode_of(&runtime).unwrap(),
        GROUP_OTHER_READABLE_DIRECTORY_MODE
    );
}

#[test]
fn spec_3_3_the_bridge_refuses_a_symbolic_link_in_place_of_the_runtime_directory_runtime_directory_is_private_or_refused()
 {
    let fixture = Fixture::new().unwrap();
    let runtime = runtime_directory_of(&fixture).unwrap();
    let target = fixture.root.join("elsewhere");
    fs::create_dir_all(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
    std::os::unix::fs::symlink(&target, &runtime).unwrap();

    let output = output_of_a_bridge_that_must_end_by_itself(&fixture).unwrap();

    assert_eq!(
        output.status.code(),
        Some(REFUSED_RUNTIME_DIRECTORY_EXIT_CODE)
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!(
            "{} already exists and is a symbolic link; it must be a directory of yours with no access for group or other\n",
            runtime.display()
        )
    );
}

const READ_SEARCH_ONLY_DIRECTORY_MODE: u32 = 0o500;
const DROPPED_FRAME_COUNT_NOT_WRITTEN: &str = "cannot write the dropped-frame count: ";
const EXIT_CHECK_FAILED_CODE: i32 = 3;
/// The descriptor limit that leaves the three standard streams open and one descriptor free, where a pipe needs two.
const NO_ROOM_FOR_A_PIPE: &str = "4";
/// Closes the descriptors a test runner may leave open, such as the heavy gate's lock, which a process under so low a limit aborts on.
const CLOSE_INHERITED_DESCRIPTORS: &str = "exec 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-";

/// A bridge process that is killed when dropped.
struct ServingBridge(Child);

impl Drop for ServingBridge {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

/// Starts a bridge serving a live harness with the given temporary directory, waits for it to say that it cannot write its dropped-frame count, and says that line and whether the bridge still serves after it.
fn dropped_frame_line_and_whether_the_bridge_still_serves(
    fixture: &Fixture,
    temporary_directory: &Path,
) -> io::Result<(String, bool)> {
    let harness = LiveHarness::start()?;
    let mut bridge = ServingBridge(
        fixture
            .bridge()
            .env("TMPDIR", temporary_directory)
            .args(["run", &harness.0.id().to_string(), "--no-debug"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut errors = io::BufReader::new(
        bridge
            .0
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("the bridge has no standard error"))?,
    );
    loop {
        let mut line = String::new();
        if errors.read_line(&mut line)? == 0 {
            return Err(io::Error::other(
                "the bridge ended before it said it cannot write its dropped-frame count",
            ));
        }
        if line.starts_with(DROPPED_FRAME_COUNT_NOT_WRITTEN) {
            return Ok((line, bridge.0.try_wait()?.is_none()));
        }
    }
}

#[test]
fn spec_3_12_the_bridge_serves_though_the_temporary_directory_is_missing_runtime_directory_is_private_or_refused()
 {
    let fixture = Fixture::new().unwrap();

    let (line, still_serving) = dropped_frame_line_and_whether_the_bridge_still_serves(
        &fixture,
        &fixture.root.join("no-such-temporary-directory"),
    )
    .unwrap();

    assert_eq!(
        line,
        "cannot write the dropped-frame count: No such file or directory (os error 2)\n"
    );
    assert!(still_serving);
}

#[test]
fn spec_3_12_the_bridge_serves_though_the_temporary_directory_is_not_writable_runtime_directory_is_private_or_refused()
 {
    let fixture = Fixture::new().unwrap();
    let temporary_directory = fixture.root.join("read-only-temporary-directory");
    fs::create_dir(&temporary_directory).unwrap();
    fs::set_permissions(
        &temporary_directory,
        fs::Permissions::from_mode(READ_SEARCH_ONLY_DIRECTORY_MODE),
    )
    .unwrap();

    let (line, still_serving) =
        dropped_frame_line_and_whether_the_bridge_still_serves(&fixture, &temporary_directory)
            .unwrap();

    assert_eq!(
        line,
        "cannot write the dropped-frame count: Permission denied (os error 13)\n"
    );
    assert!(still_serving);
}

#[test]
fn spec_3_12_the_doctor_reports_its_report_though_the_temporary_directory_is_unusable_runtime_directory_is_private_or_refused()
 {
    let fixture = Fixture::new().unwrap();
    let unusable = [
        fixture.root.join("no-such-temporary-directory"),
        fixture.root.join("read-only-temporary-directory"),
    ];
    fs::create_dir(&unusable[1]).unwrap();
    fs::set_permissions(
        &unusable[1],
        fs::Permissions::from_mode(READ_SEARCH_ONLY_DIRECTORY_MODE),
    )
    .unwrap();

    for temporary_directory in &unusable {
        let doctor = fixture
            .bridge()
            .env("TMPDIR", temporary_directory)
            .arg("doctor")
            .output()
            .unwrap();

        let report = String::from_utf8(doctor.stdout).unwrap();
        assert_eq!(
            doctor.status.code(),
            Some(EXIT_CHECK_FAILED_CODE),
            "{} {report}",
            temporary_directory.display()
        );
        assert!(
            report.ends_with("attested permission class: unknown\ndropped frames: unknown\n"),
            "{report}"
        );
        assert_eq!(String::from_utf8(doctor.stderr).unwrap(), "");
    }
}

#[test]
fn spec_3_3_the_bridge_refuses_to_start_when_it_cannot_make_a_pipe_to_learn_its_user_runtime_directory_is_private_or_refused()
 {
    let fixture = Fixture::new().unwrap();
    let configured = fixture.bridge();
    let mut limited = Command::new("sh");
    limited
        .args([
            "-c",
            &format!(
                "{CLOSE_INHERITED_DESCRIPTORS}; ulimit -n {NO_ROOM_FOR_A_PIPE}; exec \"$0\" \"$@\""
            ),
            common::BRIDGE,
            "run",
            &exited_harness_pid().unwrap(),
        ])
        .env_clear()
        .envs(
            configured
                .get_envs()
                .filter_map(|(name, value)| value.map(|value| (name, value))),
        )
        .current_dir(&fixture.root);

    let output = limited.output().unwrap();

    assert_eq!(
        output.status.code(),
        Some(REFUSED_RUNTIME_DIRECTORY_EXIT_CODE)
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "cannot learn the user this process runs as: it could not make a pipe: Too many open files (os error 24)\n"
    );
}
