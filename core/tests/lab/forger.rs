use std::thread::JoinHandle;
use std::time::Duration;

use estafette::crypto::GateKey;
use estafette::net::endpoint::NodeEndpoint;
use estafette::stream_items::StreamExt;
use iroh::protocol::Router;
use iroh::{EndpointAddr, SecretKey};
use iroh_gossip::api::{GossipReceiver, GossipSender};
use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip, TopicId};
use tokio::runtime::Handle;
use tokio::sync::oneshot;

/// How long the forger may take to gain the host as a gossip neighbor.
const JOIN_WAIT: Duration = Duration::from_secs(30);

/// A node joined to the live network's topic through its host, broadcasting whatever bytes the test hands it.
pub struct Forger {
    runtime: Handle,
    router: Router,
    topic: GossipSender,
    stop_driver: Option<oneshot::Sender<()>>,
    driver: Option<JoinHandle<()>>,
}

impl Forger {
    /// Binds under the key behind the network gate, homed on the host's relays, and joins the topic through the host, waiting until the host is a neighbor.
    pub fn join_through(
        key: SecretKey,
        gate_key: GateKey,
        topic: TopicId,
        host: EndpointAddr,
    ) -> Self {
        let (runtime, stop_driver, driver) = start_driver();
        let endpoint = runtime
            .block_on(NodeEndpoint::bind(
                key,
                gate_key,
                host.relay_urls().cloned(),
                |nonce| *nonce = SecretKey::generate().to_bytes(),
                |_closing| {},
            ))
            .unwrap();
        let host_id = host.id;
        endpoint.replace_address(host);
        let _inside_runtime = runtime.enter();
        let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(GOSSIP_ALPN, gossip.clone())
            .spawn();
        let joined = runtime
            .block_on(tokio::time::timeout(
                JOIN_WAIT,
                gossip.subscribe_and_join(topic, vec![host_id]),
            ))
            .unwrap_or_else(|_elapsed| panic!("the host was no neighbor within {JOIN_WAIT:?}"))
            .unwrap();
        let (topic, receiver) = joined.split();
        runtime.spawn(drain(receiver));
        Self {
            runtime,
            router,
            topic,
            stop_driver: Some(stop_driver),
            driver: Some(driver),
        }
    }

    /// Broadcasts the bytes on the topic as they are.
    pub fn broadcast(&self, payload: Vec<u8>) {
        self.runtime
            .block_on(self.topic.broadcast(payload.into()))
            .unwrap();
    }
}

impl Drop for Forger {
    fn drop(&mut self) {
        drop(self.runtime.block_on(self.router.shutdown()));
        drop(self.stop_driver.take());
        if let Some(driver) = self.driver.take() {
            drop(driver.join());
        }
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
