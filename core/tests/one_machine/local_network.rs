use estafette::config::NetworkConfig;
use estafette::crypto::{NetworkKeys, NetworkSecret};
use estafette::keys::parse_network_secret;
use estafette::net::endpoint::GateFailure;
use iroh::SecretKey;
use iroh_gossip::TopicId;

use crate::machine::{Card, Machine};
use crate::member::Member;
use crate::scripted_node::ScriptedNode;

const NETWORK: &str = "acme";

/// A network the binary created on this machine, with the keys its members derive from its secret.
pub struct LocalNetwork {
    machine: Machine,
    keys: NetworkKeys,
}

impl LocalNetwork {
    /// Creates the network by the binary's own command on a fresh machine named for the test.
    pub fn create(test_tag: &str) -> Self {
        let machine = Machine::fresh(test_tag, NETWORK);
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
        self.machine.start_member()
    }

    /// The keys every member derives from the network secret.
    pub const fn keys(&self) -> &NetworkKeys {
        &self.keys
    }

    /// The gossip topic of the network.
    pub fn topic(&self) -> TopicId {
        TopicId::from_bytes(*self.keys.topic_id().as_bytes())
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
