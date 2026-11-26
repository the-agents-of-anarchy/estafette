use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use estafette::config::NetworkConfig;
use estafette::crypto::{NetworkKeys, NetworkSecret};
use estafette::invite::render_host_line;
use estafette::keys::{parse_network_secret, parse_secret_key};
use estafette::net::direct::{DirectHost, DirectServer};
use estafette::net::endpoint::NodeEndpoint;
use estafette::wire::{self, Addr, HostAddr, Introduce, IntroduceAck, Message, RelayUrl, Version};
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{EndpointId, SecretKey};
use iroh_gossip::TopicId;
use serde_json::Value;
use tokio::sync::watch;

use crate::mcp_server::{PRESENCE_WAIT, online};
use crate::member::Member;
use crate::payload_placeholders::UNSET_MAC;
use crate::scratch_machine::{Card, KeptFilesPolicy, ScratchMachine};
use crate::scripted_node::ScriptedNode;

/// The name of the network every local test creates.
pub const NETWORK: &str = "acme";
/// The card name of the member amy.
pub const AMY_CARD: &str = "amy-card";
/// The handle amy's card requests.
pub const AMY_HANDLE: &str = "amy";
/// The card name of the member bob.
pub const BOB_CARD: &str = "bob-card";
/// The handle bob's card requests.
pub const BOB_HANDLE: &str = "bob";
/// The handle the third member, carol, announces.
pub const CAROL_HANDLE: &str = "carol";
/// A relay URL no resolver answers, for a host that is never reached.
const SILENT_RELAY: &str = "https://relay.estafette.invalid/";

/// A network the binary created on this machine, with the keys its members derive from its secret.
pub struct LocalNetwork {
    machine: ScratchMachine,
    keys: NetworkKeys,
}

/// Two members, amy and bob, joined through a host, each listing the other online, with the envelope of bob's claim.
pub struct MetPair {
    pub amy: Member,
    pub amy_card: Card,
    pub bob: Member,
    pub bob_card: Card,
    pub bob_claim: Value,
}

impl LocalNetwork {
    /// Creates the network by the binary's own command on a fresh machine named for the test, its members' run records kept as the test asks.
    pub fn create(test_tag: &str, kept_files_policy: &KeptFilesPolicy) -> Self {
        let machine = ScratchMachine::fresh(test_tag, NETWORK, kept_files_policy);
        machine.run(&["network", "create", NETWORK]);
        let config_file = machine
            .directories()
            .network_config_file(&NETWORK.parse().unwrap());
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
        let (process, events) = self.machine.spawn_recorded_mcp_server();
        Member::start(process, events)
    }

    /// Every file under the base directories of the machine the members run on.
    pub fn base_directory_files(&self) -> Vec<PathBuf> {
        self.machine.base_directory_files().unwrap()
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
        self.add_host_named(wire::PublicKey::from(SecretKey::generate().public()));
    }

    /// Adds by the binary's own command the stand-in host as a configured host whose relay never answers, so every member sends it its join.
    pub fn add_stand_in_host_to_the_configuration(&self, host: &ScriptedNode) {
        self.add_host_named(wire::PublicKey::from(host.id()));
    }

    /// Adds by the binary's own command a configured host of that node id whose relay never answers.
    fn add_host_named(&self, id: wire::PublicKey) {
        let host_line = render_host_line(&HostAddr {
            v: Version::CURRENT,
            id,
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
            .card_sidecar_file(&NETWORK.parse().unwrap(), &card.name.parse().unwrap());
        parse_secret_key(&fs::read_to_string(sidecar).unwrap()).unwrap()
    }

    /// Rewrites the card file's public key in upper-case hexadecimal.
    pub fn uppercase_public_key(&self, card: &Card) {
        let card_file = self
            .machine
            .directories()
            .card_file(&NETWORK.parse().unwrap(), &card.name.parse().unwrap());
        let lower = card.id.to_string();
        let text = fs::read_to_string(&card_file).unwrap();
        assert!(text.contains(&lower), "the card file does not hold {lower}");
        fs::write(card_file, text.replace(&lower, &lower.to_uppercase())).unwrap();
    }

    /// Sets the permission bits of the card's sidecar.
    pub fn set_sidecar_mode(&self, card: &Card, mode: u32) {
        let sidecar = self
            .machine
            .directories()
            .card_sidecar_file(&NETWORK.parse().unwrap(), &card.name.parse().unwrap());
        fs::set_permissions(sidecar, Permissions::from_mode(mode)).unwrap();
    }

    /// Starts a node standing in for a bootstrap host: it holds the network secret and links the members it admits.
    pub fn stand_in_host(&self) -> ScriptedNode {
        ScriptedNode::start(SecretKey::generate(), *self.keys.gate_key(), self.topic())
    }

    /// Starts amy and bob on cards requesting those handles, each claiming through the host, and waits until each lists the other online.
    #[track_caller]
    pub fn amy_and_bob_met(&self, host: &ScriptedNode) -> MetPair {
        let amy_card = self.create_card(AMY_CARD, AMY_HANDLE);
        let bob_card = self.create_card(BOB_CARD, BOB_HANDLE);
        let mut amy = self.start_member();
        amy.init_through(&amy_card, host);
        let mut bob = self.start_member();
        let bob_claim = bob.init_through(&bob_card, host);
        amy.server_mut()
            .peer_within(BOB_HANDLE, PRESENCE_WAIT, online);
        bob.server_mut()
            .peer_within(AMY_HANDLE, PRESENCE_WAIT, online);
        MetPair {
            amy,
            amy_card,
            bob,
            bob_card,
            bob_claim,
        }
    }

    /// Starts a node standing in for a member holding the key's card under the handle: it accepts every Message and answers every Introduce with that handle.
    pub fn scripted_member(&self, key: &SecretKey, handle: &str) -> ScriptedNode {
        ScriptedNode::start_answering_direct(
            key.clone(),
            *self.keys.gate_key(),
            self.topic(),
            |endpoint| self.scripted_member_server(key, handle, endpoint),
        )
    }

    /// Starts a scripted member as `scripted_member` does, except that it leaves each connection of the dialer unserved until the returned handle releases them, so a request the dialer writes stays unanswered.
    pub fn scripted_member_holding_connections_of(
        &self,
        key: &SecretKey,
        handle: &str,
        dialer: EndpointId,
    ) -> (ScriptedNode, HeldConnections) {
        let (arrived, arrivals) = watch::channel(false);
        let (release, released) = watch::channel(false);
        let node = ScriptedNode::start_answering_direct(
            key.clone(),
            *self.keys.gate_key(),
            self.topic(),
            |endpoint| HoldingConnectionsOf {
                dialer,
                arrived,
                released,
                served: self.scripted_member_server(key, handle, endpoint),
            },
        );
        (node, HeldConnections { arrivals, release })
    }

    /// The direct server of a scripted member holding the key's card under the handle.
    fn scripted_member_server(
        &self,
        key: &SecretKey,
        handle: &str,
        endpoint: &NodeEndpoint,
    ) -> DirectServer<ScriptedMemberHost> {
        DirectServer::new(
            NetworkKeys::clone(&self.keys),
            ScriptedMemberHost {
                card: wire::PublicKey::from(key.public()),
                handle: handle.to_owned(),
            },
            endpoint,
        )
    }
}

/// The connections of one dialer that a scripted member leaves unserved, and the way to serve them.
pub struct HeldConnections {
    arrivals: watch::Receiver<bool>,
    release: watch::Sender<bool>,
}

impl HeldConnections {
    /// Waits until the node holds a connection of the dialer, failing when none arrives within the bound.
    #[track_caller]
    pub fn wait_for_one(&self, node: &ScriptedNode, within: Duration) {
        let arrived = node.runtime().block_on(async {
            let mut arrivals = self.arrivals.clone();
            tokio::time::timeout(within, arrivals.wait_for(|arrived| *arrived))
                .await
                .is_ok_and(|seen| seen.is_ok())
        });
        assert!(
            arrived,
            "no connection of the dialer reached the holding member within {within:?}"
        );
    }

    /// Has the node serve every connection of the dialer it holds, and each later one at once.
    pub fn release(&self) {
        self.release.send_replace(true);
    }
}

/// The direct protocol of a scripted member that serves a connection of one dialer only once released, and every other connection at once.
#[derive(Debug)]
struct HoldingConnectionsOf {
    dialer: EndpointId,
    arrived: watch::Sender<bool>,
    released: watch::Receiver<bool>,
    served: DirectServer<ScriptedMemberHost>,
}

impl ProtocolHandler for HoldingConnectionsOf {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if connection.remote_id() == self.dialer {
            self.arrived.send_replace(true);
            drop(self.released.clone().wait_for(|released| *released).await);
        }
        self.served.accept(connection).await
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
