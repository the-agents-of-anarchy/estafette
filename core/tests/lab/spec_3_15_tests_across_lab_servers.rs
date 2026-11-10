use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::inbox_client::InboxClient;
use crate::lab_machine::LabMachine;
use crate::lab_server::{KillableHostServer, LabServer};
use crate::live_host::live_host_to_itself;
use crate::live_network::LiveNetwork;
use crate::server_member::ServerMember;
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
