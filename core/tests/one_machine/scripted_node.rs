use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use estafette::crypto::GateKey;
use estafette::net::endpoint::DIRECT_ALPN;
use estafette::net::endpoint::{GateFailure, NodeEndpoint};
use estafette::wire::WIRE_LIMIT_BYTES;
use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler, Router, RouterBuilder};
use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey};
use iroh_gossip::api::{GossipReceiver, GossipSender};
use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip, TopicId};
use n0_future::StreamExt;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

type Admitted = Arc<Mutex<BTreeSet<EndpointId>>>;

/// How long a direct request may take to be answered: the server read timeout of spec 3.14 (10000 ms) with slack.
const DIRECT_ANSWER_WAIT: Duration = Duration::from_secs(15);

/// An iroh node the test drives on the network topic: it links the nodes it admits, and plays a host, a forger or an outsider.
pub struct ScriptedNode {
    runtime: Handle,
    endpoint: NodeEndpoint,
    router: Router,
    topic: GossipSender,
    admitted: Admitted,
    stop_driver: Option<oneshot::Sender<()>>,
    driver: Option<JoinHandle<()>>,
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
        let (runtime, stop_driver, driver) = start_driver();
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
        runtime.spawn(drain(receiver));
        Self {
            runtime: runtime.clone(),
            endpoint,
            router,
            topic,
            admitted,
            stop_driver: Some(stop_driver),
            driver: Some(driver),
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
        &self.runtime
    }
}

impl Drop for ScriptedNode {
    fn drop(&mut self) {
        drop(self.runtime.block_on(self.router.shutdown()));
        drop(self.stop_driver.take());
        if let Some(driver) = self.driver.take() {
            drop(driver.join());
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

/// A thread driving a runtime of its own until its stop sender is dropped, so the test thread may block on it while its tasks run.
fn start_driver() -> (Handle, oneshot::Sender<()>, JoinHandle<()>) {
    let (handle_sender, handle_receiver) = std::sync::mpsc::channel();
    let (stop_driver, stopped) = oneshot::channel::<()>();
    let driver = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        handle_sender.send(runtime.handle().clone()).unwrap();
        runtime.block_on(async {
            drop(stopped.await);
        });
    });
    (handle_receiver.recv().unwrap(), stop_driver, driver)
}

/// Reads every event of the subscription, so the topic never drops it for lagging.
async fn drain(mut receiver: GossipReceiver) {
    while let Some(Ok(_event)) = receiver.next().await {}
}
