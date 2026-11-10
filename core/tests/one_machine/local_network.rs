use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;

use estafette::config::NetworkConfig;
use estafette::crypto::{NetworkKeys, NetworkSecret};
use estafette::invite::render_host_line;
use estafette::keys::parse_network_secret;
use estafette::net::endpoint::GateFailure;
use estafette::wire::{HostAddr, RelayUrl, Version};
use iroh::SecretKey;
use iroh_gossip::TopicId;

use crate::member::Member;
use crate::scratch_machine::{Card, ScratchMachine};
use crate::scripted_node::ScriptedNode;

const NETWORK: &str = "acme";
/// A relay URL no resolver answers, for a host that is never reached.
const SILENT_RELAY: &str = "https://relay.estafette.invalid/";

/// A network the binary created on this machine, with the keys its members derive from its secret.
pub struct LocalNetwork {
    machine: ScratchMachine,
    keys: NetworkKeys,
}

impl LocalNetwork {
    /// Creates the network by the binary's own command on a fresh machine named for the test.
    pub fn create(test_tag: &str) -> Self {
        let machine = ScratchMachine::fresh(test_tag, NETWORK);
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

    /// Starts the binary's MCP server for this network, handshaken and with no card claimed.
    pub fn start_member(&self) -> Member {
        Member::start(self.machine.spawn_mcp_server())
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
}
