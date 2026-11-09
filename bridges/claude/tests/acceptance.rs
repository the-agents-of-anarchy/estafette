use std::os::unix::net::UnixListener;

use common::Fixture;

mod common;

const NETWORK: &str = "lab";
const CARD: &str = "ana-card";
const HANDLE: &str = "ana";
const DOCTOR_CHECK_COUNT: usize = 5;

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

    let doctor = fixture.bridge().arg("doctor").output().unwrap();
    let report = String::from_utf8(doctor.stdout).unwrap();
    assert_eq!(doctor.status.code(), Some(0), "{report}");
    assert_eq!(
        report.lines().filter(|line| line.starts_with("ok")).count(),
        DOCTOR_CHECK_COUNT,
        "{report}"
    );
}
