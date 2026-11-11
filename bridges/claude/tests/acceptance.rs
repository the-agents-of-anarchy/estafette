use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use common::Fixture;

mod common;

const NETWORK: &str = "lab";
const CARD: &str = "ana-card";
const HANDLE: &str = "ana";
const DOCTOR_CHECK_COUNT: usize = 5;
const BRIDGE_START_DEADLINE: Duration = Duration::from_secs(30);
const BRIDGE_TEMPORARY_DIRECTORY: &str = "tmp";

fn dropped_frames_file(fixture: &Fixture) -> PathBuf {
    fixture.root.join(BRIDGE_TEMPORARY_DIRECTORY).join(format!(
        "estafette-bridge-claude-{}.dropped-frames",
        std::process::id()
    ))
}

fn wait_until_the_bridge_has_started(fixture: &Fixture) {
    let dropped_frames_file = dropped_frames_file(fixture);
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
