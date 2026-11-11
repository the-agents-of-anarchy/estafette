use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

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
