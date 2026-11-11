use std::fs;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use data_encoding::HEXLOWER;
use estafette::config::{Host, NetworkConfig};
use estafette::wire::HostAddr;
use serde_json::{Value, json};

use crate::inbox_client::InboxClient;
use crate::killable_host::KillableHost;
use crate::lab_machine::LabMachine;
use crate::lab_server::{KillableHostServer, LabServer};
use crate::live_host::live_host_to_itself;
use crate::live_network::LiveNetwork;
use crate::mcp_server::McpServer;
use crate::scratch_machine::ScratchMachine;
use crate::server_member::ServerMember;
use crate::tail::Tail;
use crate::timed_step::timed;

/// How long a member may take to list a peer that just started as online.
const PRESENCE_WAIT: Duration = Duration::from_secs(30);
/// The column separator of a `show-peers` line.
const COLUMN_SEPARATOR: char = '\t';
/// The column of a `show-peers` line holding the peer's status, after its handle (spec 3.13).
const PEER_STATUS_COLUMN: usize = 1;
/// The status of a peer whose Presence arrived.
const ONLINE: &str = "online";
/// The body every send of this module carries.
const SENT_BODY: &str = "the relay carried this";
/// How long an inbox socket may take to emit the frame of a delivered message.
const FRAME_WAIT: Duration = Duration::from_secs(10);
/// How long an inbox socket is watched for a frame that must not come.
const NO_FURTHER_FRAME_WAIT: Duration = Duration::from_millis(1000);
/// How many clock-drawn letters follow the base of a fresh handle.
const HANDLE_TAG_LETTERS: usize = 6;
/// How many letters a handle letter is drawn from.
const ALPHABET_LENGTH: u64 = 26;
/// The first letter a handle letter is drawn from.
const FIRST_LETTER: u8 = b'a';
/// The handle bases of the three members a test starts on this machine.
const TRIO: [&str; 3] = ["amy", "bob", "cid"];
/// How long the three members run with no host killed (spec 3.15 test 24).
const SPEC_QUIET_RUN: Duration = Duration::from_secs(180);
/// How a member's run record begins the line of a rebind (`NodeRecord::Rebinding`).
const REBIND_RECORD_START: &str = "rebinding the endpoint";
/// How long the killable host may take to send a record after its start began, the recipe's checks of the server included; chosen, not measured.
const HOST_START_WAIT: Duration = Duration::from_secs(300);
/// How `tail` begins the line of an infrastructure record (spec 3.13).
const INFRASTRUCTURE_KIND: &str = "infrastructure";
/// The column of an infrastructure line holding the host's node id.
const RECORD_HOST_COLUMN: usize = 2;
/// The column of an infrastructure line holding the record's ts.
const RECORD_TS_COLUMN: usize = 5;
/// How soon after the added host's first record every member's configuration must name it (spec 3.15 test 22).
const SPEC_HOST_ADDED_WITHIN: Duration = Duration::from_millis(1000);
/// How often the members' network configurations are read while waiting for a host.
const CONFIG_POLL: Duration = Duration::from_millis(100);

/// The base followed by letters drawn from the clock and the process, so the handle and the card named for it are free on the live network and on every lab server whatever earlier runs left.
fn fresh_handle(base: &str) -> String {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let mut drawn = u64::from(since_epoch.subsec_nanos())
        ^ since_epoch.as_secs()
        ^ u64::from(std::process::id());
    let mut handle = base.to_owned();
    for _ in 0..HANDLE_TAG_LETTERS {
        let letter = u8::try_from(drawn % ALPHABET_LENGTH).unwrap();
        handle.push(char::from(FIRST_LETTER.checked_add(letter).unwrap()));
        drawn /= ALPHABET_LENGTH;
    }
    handle
}

/// Whether the peer as the peers tool lists it is online.
fn online(peer: &Value) -> bool {
    peer["status"] == json!(ONLINE)
}

/// The status `show-peers` printed for the handle, if it printed the handle.
fn shown_status<'printed>(printed: &'printed str, handle: &str) -> Option<&'printed str> {
    printed
        .lines()
        .map(|line| line.split(COLUMN_SEPARATOR).collect::<Vec<_>>())
        .find(|columns| columns.first() == Some(&handle))
        .and_then(|columns| columns.get(PEER_STATUS_COLUMN).copied())
}

/// Spec 3.15 test 15: `show-peers` on this machine, which runs no session, lists online the members on the two lab servers, idle since they met.
#[test]
fn spec_3_15_test_15_show_peers_on_a_machine_with_no_session_lists_the_members_of_the_other_machines()
 {
    let _live_host = live_host_to_itself();
    let network = LiveNetwork::from_environment();
    let live_host_server = LabServer::of_the_live_host();
    let killable = KillableHostServer::from_environment();
    let amy = timed("amy serving on the live host's server", || {
        ServerMember::started(&live_host_server, &network, &fresh_handle("amy"))
    });
    let bob = timed("bob serving on the killable host's server", || {
        ServerMember::started(killable.server(), &network, &fresh_handle("bob"))
    });
    timed("amy lists bob online", || {
        amy.peer_within(bob.handle(), PRESENCE_WAIT, online)
    });
    timed("bob lists amy online", || {
        bob.peer_within(amy.handle(), PRESENCE_WAIT, online)
    });
    let observer = LabMachine::joined("t15-observer", &network);

    let shown = timed("show-peers on this machine, which runs no session", || {
        observer
            .command()
            .args(["show-peers", network.name()])
            .output()
            .unwrap()
    });

    let printed = String::from_utf8(shown.stdout.clone()).unwrap();
    assert!(shown.status.success(), "{shown:?}");
    assert_eq!(
        shown_status(&printed, amy.handle()),
        Some(ONLINE),
        "{printed}"
    );
    assert_eq!(
        shown_status(&printed, bob.handle()),
        Some(ONLINE),
        "{printed}"
    );
}

/// Spec 3.15 test 3, with the result of test 1: a member on the killable host's server, its UDP to and from this machine dropped, sends to a member on this machine through a relay alone; the receiver's inbox socket emits one frame and the send answers delivered and attached.
#[test]
fn spec_3_15_test_3_two_machines_with_no_direct_path_deliver_through_a_relay_as_one_frame_attached()
{
    let _live_host = live_host_to_itself();
    let network = LiveNetwork::from_environment();
    let killable = KillableHostServer::from_environment();
    let _relay_only = timed(
        "UDP between the killable host's server and this machine dropped",
        || killable.server().relay_only_to_here(),
    );
    let amy = timed("amy serving on the killable host's server", || {
        ServerMember::started(killable.server(), &network, &fresh_handle("amy"))
    });
    let bob_machine = LabMachine::joined("t3-bob", &network);
    let bob_handle = fresh_handle("bob");
    let bob_card = bob_machine.create_card(&bob_handle, &bob_handle);
    let mut bob = bob_machine.start_server();
    let claim = timed("bob claims his card on this machine", || {
        bob.call("init", &json!({ "card": bob_card.name }))
    });
    let mut inbox = InboxClient::connect(&claim["result"]["inbox_path"], 0);
    timed("amy lists bob online and attached", || {
        amy.peer_within(&bob_handle, PRESENCE_WAIT, |bob| {
            online(bob) && bob["attached"] == json!(true)
        })
    });

    let sent = timed("amy's send to bob", || {
        amy.call("send", &json!({ "to": bob_handle, "body": SENT_BODY }))
    });
    let frame = timed("bob's inbox socket emits the frame", || {
        inbox.next_frame(FRAME_WAIT)
    });
    let further = timed("bob's inbox socket emits nothing more", || {
        inbox.next_frame(NO_FURTHER_FRAME_WAIT)
    });

    let id = sent["result"]["id"].as_str().unwrap();
    assert_eq!(claim["ok"], json!(true), "{claim}");
    assert_eq!(
        sent["result"],
        json!({ "id": id, "delivered": true, "attached": true }),
        "{sent}"
    );
    let frame = frame.unwrap();
    assert_eq!(frame["seq"], json!(1), "{frame}");
    assert_eq!(frame["id"], json!(id), "{frame}");
    assert_eq!(frame["from"], json!(amy.handle()), "{frame}");
    assert_eq!(frame["body"], json!(SENT_BODY), "{frame}");
    assert_eq!(further, None);
}

/// A member serving a card on a scratch machine of this machine, its run record kept by the machine when asked.
struct Member {
    server: McpServer,
    machine: ScratchMachine,
    handle: String,
}

impl Member {
    /// A member requesting a fresh handle from the base, on a machine joined to the network by the invitation.
    #[track_caller]
    fn joined_by_invitation(network: &LiveNetwork, base: &str) -> Self {
        let handle = fresh_handle(base);
        let machine = ScratchMachine::fresh(&handle, network.name());
        network.join(&machine.directories());
        Self::claimed_on(machine, handle)
    }

    /// Creates a card named for the handle and requesting it on the machine, then serves and claims it, requiring the claim to succeed.
    #[track_caller]
    fn claimed_on(machine: ScratchMachine, handle: String) -> Self {
        let card = machine.create_card(&handle, &handle);
        let mut server = McpServer::handshaken(machine.spawn_mcp_server());
        let claim = server.call("init", &json!({ "card": card.name }));
        assert_eq!(claim["ok"], json!(true), "{claim}");
        Self {
            server,
            machine,
            handle,
        }
    }

    /// How many rebinds the member's run record names so far, read from a redacted copy kept for the count and removed after it.
    fn rebinds_recorded(&self) -> usize {
        let copies = std::env::temp_dir().join(format!(
            "ea-{}-{}-rebind-count",
            std::process::id(),
            self.handle
        ));
        let kept = self.machine.keep_run_records(&copies).unwrap();
        let rebinds = kept
            .iter()
            .map(|record| {
                fs::read_to_string(record)
                    .unwrap()
                    .lines()
                    .filter(|line| line.starts_with(REBIND_RECORD_START))
                    .count()
            })
            .sum();
        drop(fs::remove_dir_all(&copies));
        rebinds
    }

    /// The hosts the member's network configuration names now.
    fn configured_hosts(&self) -> Vec<Host> {
        let config_file = self
            .machine
            .directories()
            .network_config_file(self.machine.network())
            .unwrap();
        NetworkConfig::load(&config_file).unwrap().hosts
    }
}

/// A `tail` of the network on the observer's machine, returned once it printed its first payload.
#[track_caller]
fn tail_of(observer: &LabMachine, network: &LiveNetwork) -> Tail {
    let mut command = observer.command();
    command.args(["tail", network.name()]);
    Tail::start(command)
}

/// The host's node id as `tail` and the network configuration write it.
fn host_id(host: &HostAddr) -> String {
    HEXLOWER.encode(&host.id)
}

/// The `ts` of a `tail` line when it is an infrastructure record of the host.
fn record_ts_of(line: &str, host: &str) -> Option<u64> {
    let columns: Vec<&str> = line.split(COLUMN_SEPARATOR).collect();
    if columns.first() != Some(&INFRASTRUCTURE_KIND)
        || columns.get(RECORD_HOST_COLUMN) != Some(&host)
    {
        return None;
    }
    columns.get(RECORD_TS_COLUMN)?.parse().ok()
}

/// Starts the killable host unless it runs and waits for the tail to print a record of it sent after the start began.
#[track_caller]
fn killable_host_serving(killable: &KillableHostServer, host: &HostAddr, tail: &Tail) {
    let starting = Instant::now();
    killable.start_host();
    tail.first_line_after(starting, HOST_START_WAIT, |line| {
        record_ts_of(line, &host_id(host)).is_some()
    });
}

/// Spec 3.15 test 24: three members joined by the invitation run 180 s with both hosts serving and none killed; by the end each still lists the other two online and no member's run record names a rebind.
#[test]
fn spec_3_15_test_24_three_members_running_180_s_with_no_host_killed_never_rebind() {
    let _live_host = live_host_to_itself();
    let network = LiveNetwork::from_environment();
    let killable = KillableHostServer::from_environment();
    let killable_host = KillableHost::from_environment();
    let observer = LabMachine::joined("t24-tail", &network);
    let tail = timed("tail joined through the live host", || {
        tail_of(&observer, &network)
    });
    timed("the killable host serving", || {
        killable_host_serving(&killable, killable_host.address(), &tail);
    });
    let mut trio: Vec<Member> = timed("three members claimed through the live host", || {
        TRIO.iter()
            .map(|base| Member::joined_by_invitation(&network, base))
            .collect()
    });

    timed("three members run for 180 s with no host killed", || {
        std::thread::sleep(SPEC_QUIET_RUN);
    });
    let handles: Vec<String> = trio.iter().map(|member| member.handle.clone()).collect();
    for member in &mut trio {
        for other in handles.iter().filter(|other| **other != member.handle) {
            timed(&format!("{} lists {other} online", member.handle), || {
                member.server.peer_within(other, PRESENCE_WAIT, online)
            });
        }
    }
    let rebinds: Vec<(String, usize)> = trio
        .iter()
        .map(|member| (member.handle.clone(), member.rebinds_recorded()))
        .collect();

    assert!(
        rebinds.iter().all(|(_handle, count)| *count == 0),
        "{rebinds:?}"
    );
}

/// Each member's saved entry for the host with the instant it was first read, every configuration read at each poll until each names the host or the deadline passed; a member that did not name it by then has none.
fn hosts_first_read(
    members: &[Member],
    id: &str,
    deadline: Instant,
) -> Vec<Option<(Host, Instant)>> {
    let mut first_read: Vec<Option<(Host, Instant)>> = members.iter().map(|_| None).collect();
    loop {
        for (member, read) in members.iter().zip(first_read.iter_mut()) {
            if read.is_none() {
                *read = member
                    .configured_hosts()
                    .into_iter()
                    .find(|host| host.id == id)
                    .map(|host| (host, Instant::now()));
            }
        }
        if first_read.iter().all(Option::is_some) || Instant::now() > deadline {
            return first_read;
        }
        std::thread::sleep(CONFIG_POLL);
    }
}

/// Spec 3.15 test 22: the killable host, stopped before the members start, is started while they run, and within 1000 ms of its first record reaching this machine's `tail` every member's network configuration names it with its relay, that record or a later one being the first each member accepted.
#[test]
fn spec_3_15_test_22_a_host_added_while_members_run_is_in_every_members_configuration_within_1000_ms_of_its_first_record()
 {
    let _live_host = live_host_to_itself();
    let network = LiveNetwork::from_environment();
    let killable = KillableHostServer::from_environment();
    let killable_host = KillableHost::from_environment();
    let added = killable_host.address();
    let added_id = host_id(added);
    timed("the killable host stopped before any member starts", || {
        killable.stop_host();
    });
    let observer = LabMachine::joined("t22-tail", &network);
    let tail = timed("tail joined through the live host", || {
        tail_of(&observer, &network)
    });
    let members = timed("amy and bob claimed through the live host", || {
        [
            Member::joined_by_invitation(&network, "amy"),
            Member::joined_by_invitation(&network, "bob"),
        ]
    });
    let named_before: Vec<bool> = members
        .iter()
        .map(|member| {
            member
                .configured_hosts()
                .iter()
                .any(|host| host.id == added_id)
        })
        .collect();

    let starting = Instant::now();
    timed("the killable host started while the members run", || {
        killable.start_host();
    });
    let (first_record, after) = timed("tail prints the added host's first record", || {
        tail.first_line_after(starting, HOST_START_WAIT, |line| {
            record_ts_of(line, &added_id).is_some()
        })
    });
    let first_record_ts = record_ts_of(&first_record, &added_id).unwrap();
    let deadline = starting
        .checked_add(after)
        .and_then(|arrived| arrived.checked_add(SPEC_HOST_ADDED_WITHIN))
        .unwrap();
    let first_read = timed("every member's configuration names the added host", || {
        hosts_first_read(&members, &added_id, deadline)
    });

    assert_eq!(named_before, [false, false]);
    for (member, read) in members.iter().zip(&first_read) {
        let Some((host, read_at)) = read else {
            panic!(
                "{} did not name the added host within {SPEC_HOST_ADDED_WITHIN:?} of its first record: {:?}",
                member.handle,
                member.configured_hosts()
            );
        };
        assert_eq!(
            host.relay,
            added.relay.as_str(),
            "{}: {host:?}",
            member.handle
        );
        assert!(
            host.ts.is_some_and(|ts| ts >= first_record_ts),
            "{} accepted a record of ts {:?} before the first the tail printed, of ts {first_record_ts}, so that record's arrival does not time the host's first",
            member.handle,
            host.ts
        );
        assert!(
            *read_at <= deadline,
            "{} named the added host {:?} after the deadline",
            member.handle,
            read_at.duration_since(deadline)
        );
    }
}
