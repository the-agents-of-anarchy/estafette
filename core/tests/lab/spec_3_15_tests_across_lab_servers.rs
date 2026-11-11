use std::collections::BTreeMap;
use std::fs;
use std::time::{Duration, Instant};

use estafette::config::{Host, HostOrigin, NetworkConfig};
use estafette::keys::render_public_key;
use estafette::wire::HostAddr;
use iroh::PublicKey;
use serde_json::{Value, json};

use crate::handles::unique_handle;
use crate::inbox_client::InboxClient;
use crate::killable_host::KillableHost;
use crate::lab_machine::LabMachine;
use crate::lab_server::LabServer;
use crate::live_host::live_host_to_itself;
use crate::live_network::LiveNetwork;
use crate::mcp_server::{McpServer, ONLINE, online};
use crate::payload_placeholders::wall_clock_ms;
use crate::scratch_machine::{Card, ScratchMachine};
use crate::server_member::ServerMember;
use crate::tail::Tail;
use crate::timed_step::timed;

/// How long a member may take to list a peer that just started as online.
const PRESENCE_WAIT: Duration = Duration::from_secs(30);
/// The column separator of a `show-peers` line.
const COLUMN_SEPARATOR: char = '\t';
/// The column of a `show-peers` line holding the peer's status, after its handle (spec 3.13).
const PEER_STATUS_COLUMN: usize = 1;
/// The body every send of this module carries.
const SENT_BODY: &str = "the relay carried this";
/// How long an inbox socket may take to emit the frame of a delivered message.
const FRAME_WAIT: Duration = Duration::from_secs(10);
/// How long an inbox socket is watched for a frame that must not come.
const NO_FURTHER_FRAME_WAIT: Duration = Duration::from_millis(1000);
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
/// How `tail` begins the line of a presence (spec 3.13).
const PRESENCE_KIND: &str = "presence";
/// The column of a presence line holding the card's id.
const PRESENCE_CARD_COLUMN: usize = 2;
/// The column of a presence line holding the announced address as JSON.
const PRESENCE_ADDR_COLUMN: usize = 6;
/// The key of an announced address listing its relays.
const RELAYS_KEY: &str = "relays";
/// How long every member may take to answer a fresh `tail`'s presence request, its join included.
const ANNOUNCEMENT_WAIT: Duration = Duration::from_secs(60);
/// How long the `tail` is read at a time while answers are collected.
const TAIL_READ_SPAN: Duration = Duration::from_millis(1000);
/// How many times three fresh members are started to have each homed on the doomed host's relay alone.
const HOMING_ATTEMPTS: usize = 3;
/// How soon after the kill every member's send to every other must be delivered (spec 3.15 test 23).
const SPEC_DELIVERED_WITHIN_OF_THE_KILL: Duration = Duration::from_secs(60);
/// How many rebinds a member may record after the kill: none twice (spec 3.15 test 23).
const SPEC_MOST_REBINDS_AFTER_THE_KILL: usize = 1;
/// The column of a presence line holding its seq.
const PRESENCE_SEQ_COLUMN: usize = 7;
/// How soon after its own first Presence a new member must list every present member, in milliseconds (spec 3.15 test 25, the observer wait of spec 3.14).
const SPEC_OBSERVER_WAIT_MS: u64 = 3000;

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
    let killable = KillableHost::from_environment();
    let amy = timed("amy serving on the live host's server", || {
        ServerMember::started(&live_host_server, &network, &unique_handle("amy"))
    });
    let bob = timed("bob serving on the killable host's server", || {
        ServerMember::started(killable.server(), &network, &unique_handle("bob"))
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
    let killable = KillableHost::from_environment();
    let _relay_only = timed(
        "UDP between the killable host's server and this machine dropped",
        || killable.server().relay_only_to_here(),
    );
    let amy = timed("amy serving on the killable host's server", || {
        ServerMember::started(killable.server(), &network, &unique_handle("amy"))
    });
    let bob_machine = LabMachine::joined("t3-bob", &network);
    let bob_handle = unique_handle("bob");
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
    card: Card,
    handle: String,
}

impl Member {
    /// A member requesting a fresh handle from the base, on a machine joined to the network by the invitation.
    #[track_caller]
    fn joined_by_invitation(network: &LiveNetwork, base: &str) -> Self {
        let handle = unique_handle(base);
        let machine = ScratchMachine::fresh(&handle, network.name());
        network.join(&machine.directories());
        Self::claimed_on(machine, handle)
    }

    /// A member requesting a fresh handle from the base, on a machine whose configuration names the hosts given alone.
    #[track_caller]
    fn joined_through(network: &LiveNetwork, base: &str, hosts: &[HostAddr]) -> Self {
        let handle = unique_handle(base);
        let machine = ScratchMachine::fresh(&handle, network.name());
        network.join_through(&machine.directories(), hosts);
        Self::claimed_on(machine, handle)
    }

    /// Creates a card named for the handle and requesting it on the machine, then serves and claims it, requiring the claim to succeed.
    #[track_caller]
    fn claimed_on(machine: ScratchMachine, handle: String) -> Self {
        let card = machine.create_card(&handle, &handle);
        Self::serving(machine, card, handle)
    }

    /// Serves the card on the machine and claims it, requiring the claim to succeed.
    #[track_caller]
    fn serving(machine: ScratchMachine, card: Card, handle: String) -> Self {
        let mut server = McpServer::handshaken(machine.spawn_mcp_server());
        let claim = server.call("init", &json!({ "card": card.name }));
        assert_eq!(claim["ok"], json!(true), "{claim}");
        Self {
            server,
            machine,
            card,
            handle,
        }
    }

    /// Closes the member's standard input and requires a clean exit, keeping its machine and card for a restart.
    #[track_caller]
    fn stopped(self) -> StoppedMember {
        let Self {
            mut server,
            machine,
            card,
            handle,
        } = self;
        server.close_standard_input();
        let exit = server.wait_for_exit();
        assert!(exit.success(), "{handle} stopped with {exit:?}");
        StoppedMember {
            machine,
            card,
            handle,
        }
    }

    /// The member's card id as `tail` writes it.
    fn card_key(&self) -> String {
        render_public_key(&self.card.id)
    }

    /// How many rebinds the member's run records name so far.
    fn rebinds_recorded(&self) -> usize {
        self.machine
            .run_records()
            .unwrap()
            .iter()
            .map(|record| {
                fs::read_to_string(record)
                    .unwrap()
                    .lines()
                    .filter(|line| line.starts_with(REBIND_RECORD_START))
                    .count()
            })
            .sum()
    }

    /// The hosts the member's network configuration names now.
    fn configured_hosts(&self) -> Vec<Host> {
        hosts_configured_on(&self.machine)
    }
}

/// A member whose process exited cleanly, its machine and card kept for a restart.
struct StoppedMember {
    machine: ScratchMachine,
    card: Card,
    handle: String,
}

impl StoppedMember {
    /// The hosts the stopped member's network configuration names.
    fn configured_hosts(&self) -> Vec<Host> {
        hosts_configured_on(&self.machine)
    }

    /// Serves and claims the member's card again on its machine, requiring the claim to succeed.
    #[track_caller]
    fn restarted(self) -> Member {
        Member::serving(self.machine, self.card, self.handle)
    }
}

/// The hosts the machine's network configuration names now.
fn hosts_configured_on(machine: &ScratchMachine) -> Vec<Host> {
    let config_file = machine
        .directories()
        .network_config_file(machine.network())
        .unwrap();
    NetworkConfig::load(&config_file).unwrap().hosts
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
    render_public_key(&PublicKey::from_bytes(&host.id).unwrap())
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
fn killable_host_serving(killable: &KillableHost, tail: &Tail) {
    let starting = Instant::now();
    killable.start();
    tail.first_line_after(starting, HOST_START_WAIT, |line| {
        record_ts_of(line, &host_id(killable.address())).is_some()
    });
}

/// Spec 3.15 test 24: three members joined by the invitation run 180 s with both hosts serving and none killed; by the end each still lists the other two online and no member's run record names a rebind.
#[test]
fn spec_3_15_test_24_three_members_running_180_s_with_no_host_killed_never_rebind() {
    let _live_host = live_host_to_itself();
    let network = LiveNetwork::from_environment();
    let killable = KillableHost::from_environment();
    let observer = LabMachine::joined("t24-tail", &network);
    let tail = timed("tail joined through the live host", || {
        tail_of(&observer, &network)
    });
    timed("the killable host serving", || {
        killable_host_serving(&killable, &tail);
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
    let killable = KillableHost::from_environment();
    let _host_back = killable.started_when_dropped();
    let added = killable.address();
    let added_id = host_id(added);
    timed("the killable host stopped before any member starts", || {
        killable.stop();
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
        killable.start();
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

/// The card and the announced relays of a `tail` line when it is a presence.
fn presence_of(line: &str) -> Option<(String, Vec<String>)> {
    let columns: Vec<&str> = line.split(COLUMN_SEPARATOR).collect();
    if columns.first() != Some(&PRESENCE_KIND) {
        return None;
    }
    let addr: Value = serde_json::from_str(columns.get(PRESENCE_ADDR_COLUMN)?).ok()?;
    let relays = addr
        .get(RELAYS_KEY)?
        .as_array()?
        .iter()
        .map(|relay| relay.as_str().map(str::to_owned))
        .collect::<Option<Vec<String>>>()?;
    Some(((*columns.get(PRESENCE_CARD_COLUMN)?).to_owned(), relays))
}

/// The relays each member announces in answer to the presence request of a fresh `tail` joined by the invitation, by card, requiring every member's answer within the wait.
#[track_caller]
fn relays_announced_now(
    network: &LiveNetwork,
    members: &[Member],
) -> BTreeMap<String, Vec<String>> {
    let observer = LabMachine::joined("t-homing-tail", network);
    let tail = tail_of(&observer, network);
    let cards: Vec<String> = members.iter().map(Member::card_key).collect();
    let deadline = Instant::now().checked_add(ANNOUNCEMENT_WAIT).unwrap();
    let mut announced = BTreeMap::new();
    while !cards.iter().all(|card| announced.contains_key(card)) {
        assert!(
            Instant::now() < deadline,
            "not every member answered the tail's presence request within {ANNOUNCEMENT_WAIT:?}: {announced:?}"
        );
        for line in tail.lines_during(TAIL_READ_SPAN) {
            if let Some((card, relays)) = presence_of(&line)
                && cards.contains(&card)
            {
                announced.insert(card, relays);
            }
        }
    }
    announced
}

/// Three members whose configuration names the doomed host alone, started until each announces the doomed host's relay alone, with what each announced, giving up after the homing attempts.
#[track_caller]
fn trio_homed_on(
    network: &LiveNetwork,
    doomed: &HostAddr,
) -> (Vec<Member>, BTreeMap<String, Vec<String>>) {
    let homed_alone = vec![doomed.relay.as_str().to_owned()];
    let mut attempts = Vec::new();
    for _ in 0..HOMING_ATTEMPTS {
        let trio: Vec<Member> = TRIO
            .iter()
            .map(|base| Member::joined_through(network, base, std::slice::from_ref(doomed)))
            .collect();
        let announced = relays_announced_now(network, &trio);
        if announced.values().all(|relays| *relays == homed_alone) {
            return (trio, announced);
        }
        attempts.push(announced);
    }
    panic!(
        "iroh homed a member on another relay than {} in each of {HOMING_ATTEMPTS} attempts: {attempts:#?}",
        doomed.relay.as_str()
    );
}

/// One send from every member to every other, in a fixed order, each timed, with its sender, receiver and envelope.
fn sweep_of_sends(members: &mut [Member]) -> Vec<(String, String, Value)> {
    let handles: Vec<String> = members.iter().map(|member| member.handle.clone()).collect();
    let mut sends = Vec::new();
    for member in members.iter_mut() {
        for to in handles.iter().filter(|to| **to != member.handle) {
            let sent = timed(&format!("{}'s send to {to}", member.handle), || {
                member
                    .server
                    .call("send", &json!({ "to": to, "body": SENT_BODY }))
            });
            sends.push((member.handle.clone(), to.clone(), sent));
        }
    }
    sends
}

/// Spec 3.15 test 23: three members on this machine whose configuration names the killable host alone, each announcing its relay alone to a fresh `tail` just before the kill, see that host's relay and bootstrap node killed while the live host lives, and a sweep of sends from every member to every other, sweeps following each other, is delivered whole and ends within 60 s of the kill, while no member's run record names a rebind twice by the end of those 60 s.
#[test]
fn spec_3_15_test_23_with_their_home_host_killed_every_member_reaches_every_other_within_60_s_and_none_rebinds_twice()
 {
    let _live_host = live_host_to_itself();
    let network = LiveNetwork::from_environment();
    let doomed = KillableHost::from_environment();
    let _host_back = doomed.started_when_dropped();
    let observer = LabMachine::joined("t23-tail", &network);
    let tail = timed("tail joined through the live host", || {
        tail_of(&observer, &network)
    });
    timed("the killable host serving", || {
        killable_host_serving(&doomed, &tail);
    });
    let (mut trio, announced) = timed("three members homed on the killable host", || {
        trio_homed_on(&network, doomed.address())
    });

    let kill = timed("the killable host killed", || doomed.kill());
    let window_ends = kill
        .sent
        .checked_add(SPEC_DELIVERED_WITHIN_OF_THE_KILL)
        .unwrap();
    let mut sweeps = Vec::new();
    let delivered_whole_at = loop {
        let sweep = timed("a sweep of sends from every member to every other", || {
            sweep_of_sends(&mut trio)
        });
        let ended = Instant::now();
        let delivered_whole = sweep
            .iter()
            .all(|(_from, _to, sent)| sent["result"]["delivered"] == json!(true));
        sweeps.push((ended.duration_since(kill.sent), sweep));
        if delivered_whole {
            break Some(ended);
        }
        if ended >= window_ends {
            break None;
        }
    };
    timed("the 60 s after the kill run out", || {
        std::thread::sleep(window_ends.saturating_duration_since(Instant::now()));
    });
    let rebinds: Vec<(String, usize)> = trio
        .iter()
        .map(|member| (member.handle.clone(), member.rebinds_recorded()))
        .collect();

    assert!(
        delivered_whole_at.is_some_and(|ended| ended <= window_ends),
        "no sweep was delivered whole within {SPEC_DELIVERED_WITHIN_OF_THE_KILL:?} of the kill; members announced {announced:?}; sweeps after the kill: {sweeps:#?}"
    );
    assert!(
        rebinds
            .iter()
            .all(|(_handle, count)| *count <= SPEC_MOST_REBINDS_AFTER_THE_KILL),
        "{rebinds:?}"
    );
}

/// The live host's node id as the network configuration writes it.
fn live_host_id(network: &LiveNetwork) -> String {
    render_public_key(&network.host_address().id)
}

/// The seq of a `tail` line when it is a presence of the card, the sender's clock in milliseconds at the send for a card's first Presence (spec 3.8).
fn presence_seq_of(line: &str, card: &str) -> Option<u64> {
    let columns: Vec<&str> = line.split(COLUMN_SEPARATOR).collect();
    if columns.first() != Some(&PRESENCE_KIND) || columns.get(PRESENCE_CARD_COLUMN) != Some(&card) {
        return None;
    }
    columns.get(PRESENCE_SEQ_COLUMN)?.parse().ok()
}

/// Spec 3.15 test 25: with the killable host killed after two members joined, a member joined by the invitation, which names the live host alone, lists both online within 3000 ms of its first Presence, timed on this machine's clock from that Presence's seq.
#[test]
fn spec_3_15_test_25_with_one_host_dead_a_member_invited_through_the_live_host_lists_every_present_member_within_the_observer_wait()
 {
    let _live_host = live_host_to_itself();
    let network = LiveNetwork::from_environment();
    let doomed = KillableHost::from_environment();
    let _host_back = doomed.started_when_dropped();
    let observer = LabMachine::joined("t25-tail", &network);
    let tail = timed("tail joined through the live host", || {
        tail_of(&observer, &network)
    });
    timed("the killable host serving", || {
        killable_host_serving(&doomed, &tail);
    });
    let present = timed("amy and bob claimed through the live host", || {
        [
            Member::joined_by_invitation(&network, "amy"),
            Member::joined_by_invitation(&network, "bob"),
        ]
    });
    timed("the killable host killed", || doomed.kill());

    let joining = Instant::now();
    let mut newcomer = timed("cid joins by the invitation", || {
        Member::joined_by_invitation(&network, "cid")
    });
    let newcomer_card = newcomer.card_key();
    let (first_presence, _after) = timed("tail prints cid's first presence", || {
        tail.first_line_after(joining, ANNOUNCEMENT_WAIT, |line| {
            presence_seq_of(line, &newcomer_card).is_some()
        })
    });
    let first_presence_sent_ms = presence_seq_of(&first_presence, &newcomer_card).unwrap();
    let deadline_ms = first_presence_sent_ms
        .checked_add(SPEC_OBSERVER_WAIT_MS)
        .unwrap();
    let mut listed_at_ms = Vec::new();
    for member in &present {
        timed(&format!("cid lists {} online", member.handle), || {
            newcomer.server.peer_within(
                &member.handle,
                Duration::from_millis(deadline_ms.saturating_sub(wall_clock_ms())),
                online,
            )
        });
        listed_at_ms.push((member.handle.clone(), wall_clock_ms()));
    }

    let configured: Vec<String> = newcomer
        .configured_hosts()
        .into_iter()
        .map(|host| host.id)
        .collect();
    assert_eq!(configured, [live_host_id(&network)]);
    for (handle, at_ms) in listed_at_ms {
        assert!(
            at_ms <= deadline_ms,
            "cid listed {handle} {} ms after the observer wait of its first presence",
            at_ms.saturating_sub(deadline_ms)
        );
    }
}

/// Whether the saved host is one the member learned from a record, not one its configuration was written with.
const fn is_learned(host: &Host) -> bool {
    matches!(host.origin, HostOrigin::Learned { .. })
}

/// Spec 3.15 test 26: bob, configured with the killable host alone, learns the live host; the killable host is killed and bob stopped cleanly with both hosts in his configuration, the live one learned, then restarted with his card, and he joins through the learned host: amy, joined through the live host, lists him online as seen after the restart, and he lists her online.
#[test]
fn spec_3_15_test_26_a_member_restarted_after_its_configured_host_died_joins_through_its_learned_host()
 {
    let _live_host = live_host_to_itself();
    let network = LiveNetwork::from_environment();
    let doomed = KillableHost::from_environment();
    let _host_back = doomed.started_when_dropped();
    let live_id = live_host_id(&network);
    let doomed_id = host_id(doomed.address());
    let observer = LabMachine::joined("t26-tail", &network);
    let tail = timed("tail joined through the live host", || {
        tail_of(&observer, &network)
    });
    timed("the killable host serving", || {
        killable_host_serving(&doomed, &tail);
    });
    let mut amy = timed("amy claimed through the live host", || {
        Member::joined_by_invitation(&network, "amy")
    });
    let bob = timed("bob claimed through the killable host alone", || {
        Member::joined_through(&network, "bob", std::slice::from_ref(doomed.address()))
    });
    let learned = timed("bob's configuration names the live host", || {
        hosts_first_read(
            std::slice::from_ref(&bob),
            &live_id,
            Instant::now().checked_add(ANNOUNCEMENT_WAIT).unwrap(),
        )
    });
    assert!(
        learned.iter().all(Option::is_some),
        "bob did not learn the live host within {ANNOUNCEMENT_WAIT:?}: {:?}",
        bob.configured_hosts()
    );
    timed("the killable host killed", || doomed.kill());
    let stopped = timed("bob stopped cleanly", || bob.stopped());
    let configured_at_restart = stopped.configured_hosts();
    let restarting_ms = wall_clock_ms();
    let mut bob = timed("bob restarted with his card", || stopped.restarted());

    timed("amy lists the restarted bob online", || {
        amy.server.peer_within(&bob.handle, PRESENCE_WAIT, |peer| {
            online(peer)
                && peer["last_seen"]
                    .as_u64()
                    .is_some_and(|seen| seen > restarting_ms)
        })
    });
    timed("the restarted bob lists amy online", || {
        bob.server.peer_within(&amy.handle, PRESENCE_WAIT, online)
    });

    assert!(
        configured_at_restart
            .iter()
            .any(|host| host.id == live_id && is_learned(host)),
        "{configured_at_restart:?}"
    );
    assert!(
        configured_at_restart
            .iter()
            .any(|host| host.id == doomed_id && !is_learned(host)),
        "{configured_at_restart:?}"
    );
}
