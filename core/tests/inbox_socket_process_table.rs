use std::process::Command;

use estafette::inbox::socket::{OperatingSystemProcessTable, ProcessTable};

#[test]
fn spec_3_3_operating_system_process_table_sees_this_process_and_not_a_reaped_child() {
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();
    let child_id = child.id();
    assert!(OperatingSystemProcessTable.is_running(child_id).unwrap());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        OperatingSystemProcessTable
            .is_running(std::process::id())
            .unwrap()
    );
    assert!(!OperatingSystemProcessTable.is_running(child_id).unwrap());
}
