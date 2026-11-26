use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use estafette::crypto::GateKey;
use estafette::net::endpoint::DIRECT_ALPN;
use estafette::net::endpoint::{GateFailure, NodeEndpoint};
use estafette::stream_items::StreamExt;
use estafette::wire::WIRE_LIMIT_BYTES;
use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{ConnectError, Connection, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router, RouterBuilder};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey};
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip, TopicId};
use tokio::runtime::Handle;
use tokio::sync::watch;

#[path = "../support/gated_endpoint.rs"]
mod gated_endpoint;

use gated_endpoint::{bind_gated_endpoint, unrecorded_connections};

use crate::driven_runtime::DrivenRuntime;

type Admitted = Arc<Mutex<BTreeSet<EndpointId>>>;
type Events = Arc<watch::Sender<Vec<Event>>>;

/// The longest a server of a direct request reads it for (spec 3.14, 10000 ms).
const SPEC_SERVER_READ_TIMEOUT: Duration = Duration::from_millis(10_000);
/// The longest a sender of a direct request waits for its ack (spec 3.14, 10000 ms).
const SPEC_ACK_TIMEOUT: Duration = Duration::from_millis(10_000);
/// What a test allows beyond a spec timeout before it judges the timeout broken.
const SPEC_TIMEOUT_SLACK: Duration = Duration::from_secs(5);
/// The longest a direct request waits for its answer and a send for its ack: the longer of the two spec timeouts, with slack.
pub const SPEC_DIRECT_TIMEOUT_WITH_SLACK: Duration =
    longer(SPEC_SERVER_READ_TIMEOUT, SPEC_ACK_TIMEOUT).saturating_add(SPEC_TIMEOUT_SLACK);
/// The longer of the two durations.
const fn longer(first: Duration, second: Duration) -> Duration {
    if first.as_millis() >= second.as_millis() {
        first
    } else {
        second
    }
}

/// How long a node joined to another may take to report it up as a gossip neighbor.
const GOSSIP_NEIGHBOR_WAIT: Duration = Duration::from_secs(10);

/// How a scripted node reaches iroh: behind the network secret gate, or with no gate at all.
enum Reach {
    Gated(Box<NodeEndpoint>),
    Ungated {
        endpoint: Endpoint,
        address_lookup: MemoryLookup,
    },
}

impl Reach {
    /// The id the node is reached by.
    fn id(&self) -> EndpointId {
        match self {
            Self::Gated(node_endpoint) => node_endpoint.endpoint().id(),
            Self::Ungated { endpoint, .. } => endpoint.id(),
        }
    }

    /// The address the node is reached at.
    fn addr(&self) -> EndpointAddr {
        match self {
            Self::Gated(node_endpoint) => node_endpoint.endpoint().addr(),
            Self::Ungated { endpoint, .. } => endpoint.addr(),
        }
    }

    /// Dials the id under the direct protocol's ALPN.
    async fn connect_directly(&self, to: EndpointId) -> Result<Connection, ConnectError> {
        match self {
            Self::Gated(node_endpoint) => node_endpoint.endpoint().connect(to, DIRECT_ALPN).await,
            Self::Ungated { endpoint, .. } => endpoint.connect(to, DIRECT_ALPN).await,
        }
    }

    /// Spawns the gossip the node's topic runs on.
    fn spawn_gossip(&self) -> Gossip {
        match self {
            Self::Gated(node_endpoint) => node_endpoint.endpoint().spawn_gossip(),
            Self::Ungated { endpoint, .. } => Gossip::builder().spawn(endpoint.clone()),
        }
    }

    /// Starts a router of the node's protocol handlers.
    fn router_builder(&self) -> RouterBuilder {
        match self {
            Self::Gated(node_endpoint) => node_endpoint.endpoint().router_builder(),
            Self::Ungated { endpoint, .. } => Router::builder(endpoint.clone()),
        }
    }

    /// Takes the address as the only way to its node.
    fn replace_address(&self, address: EndpointAddr) {
        match self {
            Self::Gated(node_endpoint) => node_endpoint.replace_address(address),
            Self::Ungated { address_lookup, .. } => {
                address_lookup.set_endpoint_info(address);
            }
        }
    }
}

/// An iroh node the test drives on the network topic: it links the nodes it admits, and plays a host, a forger or an outsider.
pub struct ScriptedNode {
    runtime: DrivenRuntime,
    reach: Reach,
    router: Option<Router>,
    topic: GossipSender,
    admitted: Admitted,
    events: Events,
}

impl ScriptedNode {
    /// Starts the node under the key on the topic, behind the gate of the gate key, accepting gossip only from the nodes it admits.
    pub fn start(key: SecretKey, gate_key: GateKey, topic: TopicId) -> Self {
        Self::start_gated(key, gate_key, topic, |_endpoint| |router| router)
    }

    /// Starts the node as `start` does, also answering the direct protocol with the handler it builds on the node's endpoint.
    pub fn start_answering_direct<P: ProtocolHandler>(
        key: SecretKey,
        gate_key: GateKey,
        topic: TopicId,
        direct: impl FnOnce(&NodeEndpoint) -> P,
    ) -> Self {
        Self::start_gated(key, gate_key, topic, |endpoint| {
            let handler = direct(endpoint);
            move |router: RouterBuilder| router.accept(DIRECT_ALPN, handler)
        })
    }

    /// Starts the node under the key on the topic with no gate of its own, so it holds the topic id and no secret, accepting gossip only from the nodes it admits.
    pub fn start_without_gate(key: SecretKey, topic: TopicId) -> Self {
        let driven = DrivenRuntime::start();
        let address_lookup = MemoryLookup::new();
        let endpoint = driven
            .handle()
            .block_on(
                Endpoint::builder(presets::Minimal)
                    .secret_key(key)
                    .alpns(vec![DIRECT_ALPN.to_vec(), GOSSIP_ALPN.to_vec()])
                    .relay_mode(RelayMode::Disabled)
                    .address_lookup(address_lookup.clone())
                    .bind(),
            )
            .unwrap();
        Self::assemble(
            driven,
            Reach::Ungated {
                endpoint,
                address_lookup,
            },
            topic,
            |router| router,
        )
    }

    /// Binds the node behind the gate and starts it with the protocols the extension, built on its endpoint, adds beside gossip.
    fn start_gated<Extension: FnOnce(RouterBuilder) -> RouterBuilder>(
        key: SecretKey,
        gate_key: GateKey,
        topic: TopicId,
        extension_on: impl FnOnce(&NodeEndpoint) -> Extension,
    ) -> Self {
        let driven = DrivenRuntime::start();
        let runtime = driven.handle().clone();
        let endpoint = bind_gated_endpoint(
            &runtime,
            key,
            gate_key,
            Vec::<RelayUrl>::new(),
            |_closing: GateFailure| {},
            unrecorded_connections(),
        );
        let extension = {
            let _inside_runtime = runtime.enter();
            extension_on(&endpoint)
        };
        Self::assemble(driven, Reach::Gated(Box::new(endpoint)), topic, extension)
    }

    /// Starts gossip and the router on the node's endpoint, subscribes to the topic and records what it gives.
    fn assemble(
        driven: DrivenRuntime,
        reach: Reach,
        topic: TopicId,
        extend: impl FnOnce(RouterBuilder) -> RouterBuilder,
    ) -> Self {
        let runtime = driven.handle().clone();
        let admitted = Admitted::default();
        let events = Arc::new(watch::Sender::new(Vec::new()));
        let _inside_runtime = runtime.enter();
        let gossip = reach.spawn_gossip();
        let router = extend(reach.router_builder().accept(
            GOSSIP_ALPN,
            AdmittedOnly {
                gossip: gossip.clone(),
                admitted: Arc::clone(&admitted),
            },
        ))
        .spawn();
        let (topic, receiver) = runtime
            .block_on(gossip.subscribe(topic, Vec::new()))
            .unwrap()
            .split();
        runtime.spawn(record(receiver, Arc::clone(&events)));
        Self {
            runtime: driven,
            reach,
            router: Some(router),
            topic,
            admitted,
            events,
        }
    }

    /// Takes the address as the node's only way to the other node and joins it on the topic.
    pub fn admit(&self, address: EndpointAddr) {
        let other = address.id;
        self.reach.replace_address(address);
        self.admitted.lock().unwrap().insert(other);
        self.runtime()
            .block_on(self.topic.join_peers(vec![other]))
            .unwrap();
    }

    /// Broadcasts the bytes on the topic as they are.
    pub fn broadcast(&self, payload: Vec<u8>) {
        self.runtime()
            .block_on(self.topic.broadcast(payload.into()))
            .unwrap();
    }

    /// Every event the topic gave this node so far, in order.
    pub fn events(&self) -> Vec<Event> {
        self.events.borrow().clone()
    }

    /// Waits until the node's topic reports the neighbor up, requiring that within the gossip neighbor wait.
    #[track_caller]
    pub fn wait_for_neighbor_up(&self, neighbor: EndpointId) {
        self.wait_for_events(
            &format!("{neighbor} as a gossip neighbor"),
            GOSSIP_NEIGHBOR_WAIT,
            |events| events.contains(&Event::NeighborUp(neighbor)),
        );
    }

    /// Waits until the events the node's topic gave it satisfy `holds`, woken by each event, failing with what was awaited when the bound runs out first.
    #[track_caller]
    pub fn wait_for_events(
        &self,
        awaited: &str,
        within: Duration,
        holds: impl FnMut(&Vec<Event>) -> bool,
    ) {
        let mut events = self.events.subscribe();
        let held = self.runtime().block_on(async {
            tokio::time::timeout(within, events.wait_for(holds))
                .await
                .is_ok_and(|seen| seen.is_ok())
        });
        assert!(held, "no {awaited} within {within:?}");
    }

    /// Sends the bytes as one direct request to the node over a fresh connection and returns the answer, none when the connection, the stream or the wait fails.
    pub fn request_directly(&self, to: EndpointId, request: &[u8]) -> Option<Vec<u8>> {
        self.runtime().block_on(async {
            tokio::time::timeout(SPEC_DIRECT_TIMEOUT_WITH_SLACK, async {
                let connection = self.reach.connect_directly(to).await.ok()?;
                let (mut send, mut receive) = connection.open_bi().await.ok()?;
                send.write_all(request).await.ok()?;
                send.finish().ok()?;
                receive.read_to_end(WIRE_LIMIT_BYTES).await.ok()
            })
            .await
            .ok()
            .flatten()
        })
    }

    /// The id the node is reached by.
    pub fn id(&self) -> EndpointId {
        self.reach.id()
    }

    /// The address the node is reached at.
    pub fn addr(&self) -> EndpointAddr {
        self.reach.addr()
    }

    /// Dials the id under the direct protocol's ALPN from the node's endpoint.
    pub async fn connect_directly(&self, to: EndpointId) -> Result<Connection, ConnectError> {
        self.reach.connect_directly(to).await
    }

    /// The runtime the node's tasks run on, which the test thread may block on.
    pub const fn runtime(&self) -> &Handle {
        self.runtime.handle()
    }

    /// Ends the node as a killed process ends: its runtime stops before any connection is closed, so no peer hears a close.
    pub fn vanish(mut self) {
        self.runtime.stop();
        drop(self.router.take());
    }
}

impl Drop for ScriptedNode {
    fn drop(&mut self) {
        if let Some(router) = self.router.take() {
            drop(self.runtime().block_on(router.shutdown()));
        }
    }
}

/// The gossip handler of a node that refuses every gossip connection from a node it has not admitted.
#[derive(Debug)]
struct AdmittedOnly {
    gossip: Gossip,
    admitted: Admitted,
}

impl ProtocolHandler for AdmittedOnly {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let is_admitted = self
            .admitted
            .lock()
            .unwrap()
            .contains(&connection.remote_id());
        if is_admitted {
            self.gossip.accept(connection).await
        } else {
            Ok(())
        }
    }

    async fn shutdown(&self) {
        ProtocolHandler::shutdown(&self.gossip).await;
    }
}

/// Records every event of the subscription, reading each so the topic never drops it for lagging.
async fn record(mut receiver: GossipReceiver, events: Events) {
    while let Some(Ok(event)) = receiver.next().await {
        events.send_modify(|events| events.push(event));
    }
}
