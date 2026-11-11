use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use estafette::crypto::GateKey;
use estafette::net::endpoint::DIRECT_ALPN;
use estafette::net::endpoint::{GateFailure, NodeEndpoint};
use estafette::wire::WIRE_LIMIT_BYTES;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router, RouterBuilder};
use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey};
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip, TopicId};
use n0_future::StreamExt;
use tokio::runtime::Handle;

use crate::driven_runtime::DrivenRuntime;

type Admitted = Arc<Mutex<BTreeSet<EndpointId>>>;
type Events = Arc<Mutex<Vec<Event>>>;

/// How long a direct request may take to be answered: the server read timeout of spec 3.14 (10000 ms) with slack.
const DIRECT_ANSWER_WAIT: Duration = Duration::from_secs(15);

/// An iroh node the test drives on the network topic: it links the nodes it admits, and plays a host, a forger or an outsider.
pub struct ScriptedNode {
    runtime: DrivenRuntime,
    endpoint: NodeEndpoint,
    router: Option<Router>,
    topic: GossipSender,
    admitted: Admitted,
    events: Events,
}

impl ScriptedNode {
    /// Starts the node under the key on the topic, behind the gate of that key whose every closing goes to the sink, accepting gossip only from the nodes it admits.
    pub fn start(
        key: SecretKey,
        gate_key: GateKey,
        topic: TopicId,
        report_gate_closing: impl Fn(GateFailure) + Send + Sync + 'static,
    ) -> Self {
        Self::start_with(key, gate_key, topic, report_gate_closing, |router| router)
    }

    /// Starts the node as `start` does, also answering the direct protocol with the handler.
    pub fn start_answering_direct(
        key: SecretKey,
        gate_key: GateKey,
        topic: TopicId,
        direct: impl ProtocolHandler,
    ) -> Self {
        Self::start_with(
            key,
            gate_key,
            topic,
            |_closing: GateFailure| {},
            |router| router.accept(DIRECT_ALPN, direct),
        )
    }

    /// Starts the node with the protocols the extension adds beside gossip.
    fn start_with(
        key: SecretKey,
        gate_key: GateKey,
        topic: TopicId,
        report_gate_closing: impl Fn(GateFailure) + Send + Sync + 'static,
        extend: impl FnOnce(RouterBuilder) -> RouterBuilder,
    ) -> Self {
        let driven = DrivenRuntime::start();
        let runtime = driven.handle().clone();
        let endpoint = runtime
            .block_on(NodeEndpoint::bind(
                key,
                gate_key,
                Vec::<RelayUrl>::new(),
                |nonce| *nonce = SecretKey::generate().to_bytes(),
                report_gate_closing,
            ))
            .unwrap();
        let admitted = Admitted::default();
        let events = Events::default();
        let _inside_runtime = runtime.enter();
        let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());
        let router = extend(Router::builder(endpoint.endpoint().clone()).accept(
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
            endpoint,
            router: Some(router),
            topic,
            admitted,
            events,
        }
    }

    /// Takes the address as the node's only way to the other node and joins it on the topic.
    pub fn admit(&self, address: EndpointAddr) {
        let other = address.id;
        self.endpoint().replace_address(address);
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
        self.events.lock().unwrap().clone()
    }

    /// Sends the bytes as one direct request to the node over a fresh connection and returns the answer, none when the connection, the stream or the wait fails.
    pub fn request_directly(&self, to: EndpointId, request: &[u8]) -> Option<Vec<u8>> {
        self.runtime().block_on(async {
            tokio::time::timeout(DIRECT_ANSWER_WAIT, async {
                let connection = self
                    .endpoint()
                    .endpoint()
                    .connect(to, DIRECT_ALPN)
                    .await
                    .ok()?;
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

    /// The node's endpoint.
    pub const fn endpoint(&self) -> &NodeEndpoint {
        &self.endpoint
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
        events.lock().unwrap().push(event);
    }
}
