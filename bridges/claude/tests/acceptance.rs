use std::os::unix::net::UnixListener;
use std::process::Stdio;

use common::{Fixture, Spawned, claim_card_over_mcp, forward_json_lines};

mod common;

const NETWORK: &str = "lab";
const CARD: &str = "ana-card";
const HANDLE: &str = "ana";
const DOCTOR_CHECK_COUNT: usize = 5;

#[test]
fn spec_3_12_doctor_passes_every_check_for_a_bridge_beside_a_real_binary_serving_mcp() {
    let fixture = Fixture::new().unwrap();
    assert_eq!(
        fixture
            .run_estafette(&["network", "create", NETWORK])
            .unwrap()
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        fixture
            .run_estafette(&["create-card", NETWORK, CARD, HANDLE])
            .unwrap()
            .status
            .code(),
        Some(0)
    );
    let _messaging_socket = UnixListener::bind(fixture.messaging_socket()).unwrap();
    let mut member = Spawned(
        fixture
            .estafette()
            .args(["mcp", NETWORK])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut member_input = member.0.stdin.take().unwrap();
    let answers = forward_json_lines(member.0.stdout.take().unwrap());
    let claimed = claim_card_over_mcp(&mut member_input, &answers, CARD).unwrap();
    assert_eq!(claimed["ok"], true, "{claimed}");
    assert_eq!(claimed["you"]["handle"], HANDLE, "{claimed}");
    let _bridge = Spawned(
        fixture
            .bridge()
            .args(["run", &std::process::id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let doctor = fixture.bridge().arg("doctor").output().unwrap();
    let report = String::from_utf8(doctor.stdout).unwrap();
    assert_eq!(doctor.status.code(), Some(0), "{report}");
    assert_eq!(
        report.lines().filter(|line| line.starts_with("ok")).count(),
        DOCTOR_CHECK_COUNT,
        "{report}"
    );
}
