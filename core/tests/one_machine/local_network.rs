use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use estafette::config::NetworkConfig;
use estafette::crypto::{NetworkKeys, NetworkSecret};
use estafette::invite::render_host_line;
use estafette::keys::{parse_network_secret, parse_secret_key};
use estafette::net::direct::{DirectHost, DirectServer};
use estafette::net::endpoint::GateFailure;
use estafette::wire::{self, Addr, HostAddr, Introduce, IntroduceAck, Message, RelayUrl, Version};
use iroh::SecretKey;
use iroh_gossip::TopicId;

use crate::mcp_server::{PRESENCE_WAIT, online};
use crate::member::Member;
use crate::member_events::MemberEvents;
use crate::payload_placeholders::UNSET_MAC;
use crate::scratch_machine::{Card, RunRecordKeeping, ScratchMachine};
use crate::scripted_node::ScriptedNode;

/// The name of the network every local test creates.
pub const NETWORK: &str = "acme";
/// A relay URL no resolver answers, for a host that is never reached.
const SILENT_RELAY: &str = "https://relay.estafette.invalid/";

/// A network the binary created on this machine, with the keys its members derive from its secret.
pub struct LocalNetwork {
    machine: ScratchMachine,
    keys: NetworkKeys,
}

/// Two members, amy and bob, joined through a host, each listing the other online.
pub struct MetPair {
    pub amy: Member,
    pub amy_card: Card,
    pub bob: Member,
    pub bob_card: Card,
}

impl LocalNetwork {
    /// Creates the network by the binary's own command on a fresh machine named for the test, its members' run records kept as the test asks.
    pub fn create(test_tag: &str, run_records: &RunRecordKeeping) -> Self {
        let machine = ScratchMachine::fresh(test_tag, NETWORK, run_records);
        machine.run(&["network", "create", NETWORK]);
        let config_file = machine.directories().network_config_file(NETWORK).unwrap();
        let secret =
            parse_network_secret(&NetworkConfig::load(&config_file).unwrap().secret).unwrap();
        Self {
            machine,
            keys: NetworkKeys::derive(&NetworkSecret::from_bytes(*secret.as_bytes())),
        }
    }

    /// Creates a card requesting the handle by the binary's own command.
    pub fn create_card(&self, name: &str, handle: &str) -> Card {
        self.machine.create_card(name, handle)
    }

    /// Starts the binary's MCP server for this network, handshaken and with no card claimed, its events awaited in the trace directory it writes in.
    #[track_caller]
    pub fn start_member(&self) -> Member {
        let number = self.machine.members_started();
        let process = self.machine.spawn_mcp_server();
        let events = MemberEvents::new(
            &format!("member {number}"),
            &self.machine.traces_directory_of_member(number),
        );
        Member::start(process, events)
    }

    /// Every file under the base directories of the machine the members run on.
    pub fn base_directory_files(&self) -> Vec<PathBuf> {
        self.machine.base_directory_files()
    }

    /// The machine the members run on.
    pub const fn machine(&self) -> &ScratchMachine {
        &self.machine
    }

    /// The keys every member derives from the network secret.
    pub const fn keys(&self) -> &NetworkKeys {
        &self.keys
    }

    /// The gossip topic of the network.
    pub fn topic(&self) -> TopicId {
        TopicId::from_bytes(*self.keys.topic_id().as_bytes())
    }

    /// Adds by the binary's own command a configured host whose relay never answers, so a member always knows a host and may rebind.
    pub fn add_host_that_never_answers(&self) {
        let host_line = render_host_line(&HostAddr {
            v: Version::CURRENT,
            id: *SecretKey::generate().public().as_bytes(),
            relay: RelayUrl::try_from(SILENT_RELAY.to_owned()).unwrap(),
        })
        .unwrap();
        self.machine
            .run(&["network", "add-host", NETWORK, &host_line]);
    }

    /// The secret key the card's sidecar holds, so a scripted node can run under the card a member claims.
    pub fn card_key(&self, card: &Card) -> SecretKey {
        let sidecar = self
            .machine
            .directories()
            .card_sidecar_file(NETWORK, &card.name)
            .unwrap();
        parse_secret_key(&fs::read_to_string(sidecar).unwrap()).unwrap()
    }

    /// Sets the permission bits of the card's sidecar.
    pub fn set_sidecar_mode(&self, card: &Card, mode: u32) {
        let sidecar = self
            .machine
            .directories()
            .card_sidecar_file(NETWORK, &card.name)
            .unwrap();
        fs::set_permissions(sidecar, Permissions::from_mode(mode)).unwrap();
    }

    /// Starts a node standing in for a bootstrap host: it holds the network secret and links the members it admits.
    pub fn stand_in_host(&self) -> ScriptedNode {
        ScriptedNode::start(
            SecretKey::generate(),
            *self.keys.gate_key(),
            self.topic(),
            |_closing: GateFailure| {},
        )
    }

    /// Starts amy and bob on cards requesting those handles, each claiming through the host, and waits until each lists the other online.
    #[track_caller]
    pub fn amy_and_bob_met(&self, host: &ScriptedNode) -> MetPair {
        let amy_card = self.create_card("amy-card", "amy");
        let bob_card = self.create_card("bob-card", "bob");
        let mut amy = self.start_member();
        amy.init_through(&amy_card, host);
        let mut bob = self.start_member();
        bob.init_through(&bob_card, host);
        amy.peer_within("bob", PRESENCE_WAIT, online);
        bob.peer_within("amy", PRESENCE_WAIT, online);
        MetPair {
            amy,
            amy_card,
            bob,
            bob_card,
        }
    }

    /// Starts a node standing in for a member holding the key's card under the handle: it accepts every Message and answers every Introduce with that handle.
    pub fn scripted_member(&self, key: &SecretKey, handle: &str) -> ScriptedNode {
        ScriptedNode::start_answering_direct(
            key.clone(),
            *self.keys.gate_key(),
            self.topic(),
            |endpoint| {
                DirectServer::new(
                    NetworkKeys::clone(&self.keys),
                    ScriptedMemberHost {
                        card: *key.public().as_bytes(),
                        handle: handle.to_owned(),
                    },
                    endpoint,
                )
            },
        )
    }
}

/// The direct side of a scripted member holding a card under a handle: it accepts every Message and answers every Introduce with the handle.
#[derive(Clone)]
struct ScriptedMemberHost {
    card: wire::PublicKey,
    handle: String,
}

impl DirectHost for ScriptedMemberHost {
    fn is_stopping(&self) -> bool {
        false
    }

    fn own_card(&self) -> Option<wire::PublicKey> {
        Some(self.card)
    }

    fn is_attached(&self) -> bool {
        false
    }

    fn receive_message(&self, _sender: wire::PublicKey, _message: Message) -> bool {
        false
    }

    fn receive_introduce(&self, _sender: wire::PublicKey, _introduce: Introduce) -> IntroduceAck {
        IntroduceAck {
            v: Version::CURRENT,
            handle: Some(self.handle.clone()),
            public: b"{}".to_vec(),
            attached: false,
            addr: Addr::from_endpoint_report(Vec::<String>::new(), []),
            mac: UNSET_MAC,
        }
    }
}
