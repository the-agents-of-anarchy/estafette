use crate::local_network::LocalNetwork;
use crate::scratch_machine::RunRecordKeeping;
use crate::trace_reader::events_lines_in;

const MEMBERS_STARTED: u32 = 2;
const HANDSHAKE_FAILED_EVENT: &str = "tool.handshake_failed";

#[test]
fn observability_decision_16_each_member_starts_with_the_default_and_writes_its_trace_in_a_data_directory_of_its_own()
 {
    let network = LocalNetwork::create("m1", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    for _member in 0..MEMBERS_STARTED {
        let mut server = machine.spawn_mcp_server();
        drop(server.stdin.take());
        assert!(server.wait().unwrap().success());
    }

    let events_of_member = |number: u32| {
        events_lines_in(&machine.traces_directory_of_member(number))
            .unwrap()
            .into_iter()
            .map(|line| line.event)
            .collect::<Vec<_>>()
    };

    assert_eq!(machine.members_started(), MEMBERS_STARTED);
    assert_ne!(
        machine.traces_directory_of_member(0),
        machine.traces_directory_of_member(1)
    );
    for number in 0..MEMBERS_STARTED {
        assert_eq!(
            events_of_member(number),
            [HANDSHAKE_FAILED_EVENT],
            "member {number}"
        );
    }
}
