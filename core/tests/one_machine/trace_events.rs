use std::fs::{self, OpenOptions};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use estafette::net::endpoint::GateFailure;

use crate::gate_closings::GateClosings;
use crate::local_network::{LocalNetwork, NETWORK};
use crate::member_events::MemberEvents;
use crate::scratch_machine::{RunRecordKeeping, ScratchMachine, TIMELINE_HEADING};
use crate::trace_reader::{NO_EVENT_RECORDED, events_lines_in};

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
const COMMAND_FAILED_LINE: &str = "{\"v\":1,\"run\":\"6c935f82c118f7b3\",\"n\":1,\"mono_ms\":9,\"wall_ms\":1794524353724,\"event\":\"process.command_failed\",\"message\":\"stopped\"}\n";
const HANDSHAKE_FAILED_LINE: &str = "{\"v\":1,\"run\":\"6c935f82c118f7b3\",\"n\":2,\"mono_ms\":12,\"wall_ms\":1794524353727,\"event\":\"tool.handshake_failed\"}\n";
const EVENTS_FILE_NAME: &str = "20261112T225913Z-6c935f82c118f7b3-estafette.events.jsonl";
const FORCED_FAILURE_VARIABLE: &str = "ESTAFETTE_FORCED_FAILURE";
const FORCED_FAILURE_TEST: &str =
    "trace_events::forced_failure_after_two_members_for_the_timeline_print";
const FORCED_FAILURE_MESSAGE: &str = "the failure the timeline print follows";
/// How far a stand-in clock moves at each reading, longer than any notice takes to arrive.
const TICK: Duration = Duration::from_secs(10);
/// The bound, in ticks, that holds a member wait's readings of its clock up to the end of the look its first notice wakes.
const TICKS_UNTIL_THE_LOOK_AFTER_A_NOTICE_ENDS: u32 = 3;

/// A stand-in clock whose every reading is one tick later than the last, so where a wait's bound runs out follows from how often it reads the time.
fn clock_ticking_by(tick: Duration) -> impl FnMut() -> Instant {
    let mut now = Instant::now();
    move || {
        now = now.checked_add(tick).unwrap();
        now
    }
}

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

    let member = MemberEvents::new(
        "member 0",
        &machine.traces_directory_of_member(FIRST_MEMBER),
    );
    let line = member.wait_for(COMMAND_FAILED_EVENT, EVENT_BOUND);

    assert_eq!(line.event, COMMAND_FAILED_EVENT);
}

#[test]
fn observability_decision_14_a_line_appended_while_the_wait_runs_to_an_events_file_its_writer_keeps_open_is_found_when_it_is_written()
 {
    let network = LocalNetwork::create("w2", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    let traces = machine.traces_directory_of_member(FIRST_MEMBER);
    fs::create_dir_all(&traces).unwrap();
    let mut events_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(traces.join(EVENTS_FILE_NAME))
        .unwrap();
    events_file
        .write_all(NETWORK_CREATED_LINE.as_bytes())
        .unwrap();
    let member = MemberEvents::new("member 0", &traces);

    let line = member.wait_for_after(COMMAND_FAILED_EVENT, EVENT_BOUND, Instant::now, || {
        events_file
            .write_all(COMMAND_FAILED_LINE.as_bytes())
            .unwrap();
    });

    assert_eq!(line.event, COMMAND_FAILED_EVENT);
}

#[test]
fn observability_decision_14_a_wait_whose_bound_runs_out_fails_naming_the_event_and_the_members_timeline_so_far()
 {
    let network = LocalNetwork::create("w3", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    let traces = machine.traces_directory_of_member(FIRST_MEMBER);
    fs::create_dir_all(&traces).unwrap();
    fs::write(traces.join(EVENTS_FILE_NAME), NETWORK_CREATED_LINE).unwrap();
    let member = MemberEvents::new("member 0", &traces);

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
fn observability_decision_14_a_wait_whose_bound_ran_out_before_a_notice_was_taken_fails_though_the_line_announced_is_there()
 {
    let network = LocalNetwork::create("w5", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    let traces = machine.traces_directory_of_member(FIRST_MEMBER);
    fs::create_dir_all(&traces).unwrap();
    let mut events_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(traces.join(EVENTS_FILE_NAME))
        .unwrap();
    events_file
        .write_all(NETWORK_CREATED_LINE.as_bytes())
        .unwrap();
    let member = MemberEvents::new("member 0", &traces);
    let observer = MemberEvents::new("observer of member 0", &traces);

    let failure = failure_text_of(|| {
        member.wait_for_after(COMMAND_FAILED_EVENT, Duration::ZERO, Instant::now, || {
            events_file
                .write_all(COMMAND_FAILED_LINE.as_bytes())
                .unwrap();
            observer.wait_for_after(HANDSHAKE_FAILED_EVENT, EVENT_BOUND, Instant::now, || {
                events_file
                    .write_all(HANDSHAKE_FAILED_LINE.as_bytes())
                    .unwrap();
            });
        });
    });

    assert_eq!(
        failure,
        format!(
            "member 0: no {COMMAND_FAILED_EVENT} within {:?}; its timeline so far:\n         5 ms  #0    {NETWORK_CREATED_EVENT} network=\"acme\"\n         9 ms  #1    {COMMAND_FAILED_EVENT} message=\"stopped\"\n        12 ms  #2    {HANDSHAKE_FAILED_EVENT}",
            Duration::ZERO
        )
    );
}

#[test]
fn observability_decision_14_a_line_a_look_finds_once_the_bound_ran_out_fails_the_wait_though_the_notice_came_inside_it()
 {
    let network = LocalNetwork::create("w6", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    let traces = machine.traces_directory_of_member(FIRST_MEMBER);
    fs::create_dir_all(&traces).unwrap();
    let mut events_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(traces.join(EVENTS_FILE_NAME))
        .unwrap();
    events_file
        .write_all(NETWORK_CREATED_LINE.as_bytes())
        .unwrap();
    let member = MemberEvents::new("member 0", &traces);
    let within = TICK
        .checked_mul(TICKS_UNTIL_THE_LOOK_AFTER_A_NOTICE_ENDS)
        .unwrap();

    let waiting_since = Instant::now();
    let failure = failure_text_of(|| {
        member.wait_for_after(COMMAND_FAILED_EVENT, within, clock_ticking_by(TICK), || {
            events_file
                .write_all(COMMAND_FAILED_LINE.as_bytes())
                .unwrap();
        });
    });
    let waited = waiting_since.elapsed();

    assert_eq!(
        failure,
        format!(
            "member 0: no {COMMAND_FAILED_EVENT} within {within:?}; its timeline so far:\n         5 ms  #0    {NETWORK_CREATED_EVENT} network=\"acme\"\n         9 ms  #1    {COMMAND_FAILED_EVENT} message=\"stopped\""
        )
    );
    assert!(
        waited < TICK,
        "the wait ran out on its notice, not after its look: {waited:?}"
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
    let first = MemberEvents::new("member 0", &traces_of_first);
    let second = MemberEvents::new(
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

#[test]
fn observability_decision_14_a_gate_closing_the_sink_already_took_is_counted_without_a_further_report()
 {
    let closings = GateClosings::default();
    let report_gate_closing = closings.sink();
    report_gate_closing(GateFailure::Timeout);
    report_gate_closing(GateFailure::WrongProof);

    let counted = closings.wait_beyond(0, EVENT_BOUND);

    assert_eq!(counted, 2);
}

#[test]
fn observability_decision_14_a_gate_closing_reported_while_the_wait_runs_is_counted_when_it_is_reported_not_when_the_bound_runs_out()
 {
    let closings = GateClosings::default();
    let report_gate_closing = closings.sink();
    let (release_reporter, reporter_released) = mpsc::channel::<()>();
    let reporter = thread::spawn(move || {
        reporter_released.recv().unwrap();
        report_gate_closing(GateFailure::Timeout);
    });

    let waiting_since = Instant::now();
    let counted = closings.wait_beyond_after(0, EVENT_BOUND, || {
        release_reporter.send(()).unwrap();
    });
    let waited = waiting_since.elapsed();

    assert_eq!(counted, 1);
    assert!(
        waited < EVENT_BOUND,
        "the wait ran to its bound: {waited:?}"
    );
    reporter.join().unwrap();
}

#[test]
fn observability_decision_14_a_wait_for_a_gate_closing_whose_bound_runs_out_fails_naming_the_closing_and_the_count_so_far()
 {
    let closings = GateClosings::default();

    let failure = failure_text_of(|| {
        closings.wait_beyond(0, EXHAUSTED_BOUND);
    });

    assert_eq!(
        failure,
        format!("no gate closing beyond 0 within {EXHAUSTED_BOUND:?}; 0 reported so far")
    );
}

#[test]
fn observability_decision_14_a_gate_closing_an_earlier_wait_counted_is_not_taken_for_a_later_one() {
    let closings = GateClosings::default();
    let report_gate_closing = closings.sink();
    report_gate_closing(GateFailure::Timeout);
    let counted_first = closings.wait_beyond(0, EVENT_BOUND);

    let failure = failure_text_of(|| {
        closings.wait_beyond(counted_first, EXHAUSTED_BOUND);
    });
    report_gate_closing(GateFailure::WrongProof);
    let counted_second = closings.wait_beyond(counted_first, EVENT_BOUND);

    assert_eq!(counted_first, 1);
    assert_eq!(
        failure,
        format!("no gate closing beyond 1 within {EXHAUSTED_BOUND:?}; 1 reported so far")
    );
    assert_eq!(counted_second, 2);
}

#[test]
fn observability_decision_15_a_failing_test_prints_the_timeline_of_each_member_it_started() {
    let forced = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", FORCED_FAILURE_TEST, "--ignored", "--nocapture"])
        .env(FORCED_FAILURE_VARIABLE, "1")
        .output()
        .unwrap();

    let printed = String::from_utf8_lossy(&forced.stderr);
    assert!(!forced.status.success(), "{printed}");
    let [_before, first, second] = printed.split(TIMELINE_HEADING).collect::<Vec<_>>()[..] else {
        panic!("one timeline for each of the two members: {printed}");
    };
    assert!(first.starts_with(" 0 on "), "{printed}");
    assert!(first.contains(HANDSHAKE_FAILED_EVENT), "{printed}");
    assert!(second.starts_with(" 1 on "), "{printed}");
    assert!(second.contains(NO_EVENT_RECORDED), "{printed}");
    assert!(!second.contains(HANDSHAKE_FAILED_EVENT), "{printed}");
}

/// Starts two members, the second killed before it records anything, and fails when the variable is set, which the test of the timeline print sets in the child process it runs this fixture in; ignored in the suite, and passing at once when run without the variable.
#[test]
#[ignore = "a fixture: the child process of observability_decision_15_a_failing_test_prints_the_timeline_of_each_member_it_started"]
fn forced_failure_after_two_members_for_the_timeline_print() {
    if std::env::var_os(FORCED_FAILURE_VARIABLE).is_none() {
        return;
    }
    let network = LocalNetwork::create("f1", &RunRecordKeeping::from_environment());
    let machine = network.machine();
    let mut first = machine.spawn_mcp_server();
    drop(first.stdin.take());
    assert!(first.wait().unwrap().success());
    let mut second = machine.spawn_mcp_server();
    second.kill().unwrap();
    second.wait().unwrap();
    panic!("{FORCED_FAILURE_MESSAGE}");
}
