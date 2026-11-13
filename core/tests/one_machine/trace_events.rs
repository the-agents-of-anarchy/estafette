use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use crate::local_network::{LocalNetwork, NETWORK};
use crate::member_events::MemberEvents;
use crate::scratch_machine::{RunRecordKeeping, ScratchMachine};
use crate::trace_reader::events_lines_in;

const MEMBERS_STARTED: u32 = 2;
const HANDSHAKE_FAILED_EVENT: &str = "tool.handshake_failed";
const COMMAND_FAILED_EVENT: &str = "process.command_failed";
const NETWORK_CREATED_EVENT: &str = "process.network_created";
const EVENT_BOUND: Duration = Duration::from_secs(30);
const EXHAUSTED_BOUND: Duration = Duration::from_millis(300);
const FIRST_MEMBER: u32 = 0;
const SECOND_MEMBER: u32 = 1;
const UNKNOWN_HANDLE: &str = "bob";
const BODY: &str = "hello";
const NETWORK_CREATED_LINE: &str = "{\"v\":1,\"run\":\"6c935f82c118f7b3\",\"n\":0,\"mono_ms\":5,\"wall_ms\":1794524353720,\"event\":\"process.network_created\",\"network\":\"acme\"}\n";

/// Runs `test-send` to an unknown handle as the member of that number, which ends having recorded `process.command_failed` in the member's trace directory.
fn send_to_nobody_as_member(machine: &ScratchMachine, number: u32) {
    fs::create_dir_all(machine.traces_directory_of_member(number)).unwrap();
    let status = machine
        .member_command(number)
        .args(["test-send", NETWORK, UNKNOWN_HANDLE, BODY])
        .output()
        .unwrap()
        .status;
    assert_eq!(status.code(), Some(1));
}

/// What the failure of a wait says, which the wait panics with.
fn failure_text_of(wait: impl FnOnce()) -> String {
    let payload = catch_unwind(AssertUnwindSafe(wait)).unwrap_err();
    payload.downcast_ref::<String>().unwrap().clone()
}

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

#[test]
fn observability_decision_14_an_event_already_in_the_file_is_found_without_a_further_write() {
    let network = LocalNetwork::create("w1", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    send_to_nobody_as_member(machine, FIRST_MEMBER);

    let member = MemberEvents::watch(
        "member 0",
        &machine.traces_directory_of_member(FIRST_MEMBER),
    );
    let line = member.wait_for(COMMAND_FAILED_EVENT, EVENT_BOUND);

    assert_eq!(line.event, COMMAND_FAILED_EVENT);
}

#[test]
fn observability_decision_14_an_event_the_file_gains_while_the_wait_runs_is_found_when_it_is_written()
 {
    let network = LocalNetwork::create("w2", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    let traces = machine.traces_directory_of_member(FIRST_MEMBER);
    fs::create_dir_all(&traces).unwrap();
    let member = MemberEvents::watch("member 0", &traces);
    let mut server = machine.spawn_mcp_server();
    let events_so_far = || {
        events_lines_in(&machine.traces_directory_of_member(FIRST_MEMBER))
            .unwrap()
            .into_iter()
            .map(|line| line.event)
            .collect::<Vec<_>>()
    };
    assert_eq!(events_so_far(), Vec::<String>::new());

    drop(server.stdin.take());
    let line = member.wait_for(HANDSHAKE_FAILED_EVENT, EVENT_BOUND);

    assert_eq!(line.event, HANDSHAKE_FAILED_EVENT);
    assert!(server.wait().unwrap().success());
}

#[test]
fn observability_decision_14_a_wait_whose_bound_runs_out_fails_naming_the_event_and_the_members_timeline_so_far()
 {
    let network = LocalNetwork::create("w3", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    let traces = machine.traces_directory_of_member(FIRST_MEMBER);
    fs::create_dir_all(&traces).unwrap();
    fs::write(
        traces.join("20261112T225913Z-6c935f82c118f7b3-estafette.events.jsonl"),
        NETWORK_CREATED_LINE,
    )
    .unwrap();
    let member = MemberEvents::watch("member 0", &traces);

    let failure = failure_text_of(|| {
        member.wait_for(COMMAND_FAILED_EVENT, EXHAUSTED_BOUND);
    });

    assert_eq!(
        failure,
        format!(
            "member 0: no {COMMAND_FAILED_EVENT} within {EXHAUSTED_BOUND:?}; its timeline so far:\n         5 ms  #0    {NETWORK_CREATED_EVENT} network=\"acme\""
        )
    );
}

#[test]
fn observability_decision_14_a_second_members_event_of_the_same_name_is_not_taken_for_the_first_members()
 {
    let network = LocalNetwork::create("w4", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    send_to_nobody_as_member(machine, SECOND_MEMBER);
    let traces_of_first = machine.traces_directory_of_member(FIRST_MEMBER);
    fs::create_dir_all(&traces_of_first).unwrap();
    let first = MemberEvents::watch("member 0", &traces_of_first);
    let second = MemberEvents::watch(
        "member 1",
        &machine.traces_directory_of_member(SECOND_MEMBER),
    );

    let first_failure = failure_text_of(|| {
        first.wait_for(COMMAND_FAILED_EVENT, EXHAUSTED_BOUND);
    });
    let second_line = second.wait_for(COMMAND_FAILED_EVENT, EVENT_BOUND);

    assert!(first_failure.starts_with("member 0: no process.command_failed"));
    assert_eq!(second_line.event, COMMAND_FAILED_EVENT);
}
