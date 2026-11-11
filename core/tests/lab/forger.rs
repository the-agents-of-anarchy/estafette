use std::time::Duration;

use estafette::crypto::GateKey;
use estafette::net::endpoint::NodeEndpoint;
use iroh::protocol::Router;
use iroh::{EndpointAddr, SecretKey};
use iroh_gossip::api::{GossipReceiver, GossipSender};
use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip, TopicId};
use n0_future::StreamExt;

use crate::driven_runtime::DrivenRuntime;

/// How long the forger may take to gain the host as a gossip neighbor.
const JOIN_WAIT: Duration = Duration::from_secs(30);

/// A node joined to the live network's topic through its host, broadcasting whatever bytes the test hands it.
pub struct Forger {
    runtime: DrivenRuntime,
    router: Router,
    topic: GossipSender,
}

impl Forger {
    /// Binds under the key behind the network gate, homed on the host's relays, and joins the topic through the host, waiting until the host is a neighbor.
    #[track_caller]
    pub fn join_through(
        key: SecretKey,
        gate_key: GateKey,
        topic: TopicId,
        host: EndpointAddr,
    ) -> Self {
        let driven = DrivenRuntime::start();
        let runtime = driven.handle().clone();
        let endpoint = runtime
            .block_on(NodeEndpoint::bind(
                key,
                gate_key,
                host.relay_urls().cloned(),
                |nonce| *nonce = SecretKey::generate().to_bytes(),
                |_closing| {},
                estafette::net::direct::HeldConnections::new(std::sync::Arc::new(
                    tokio::time::Instant::now,
                )),
            ))
            .unwrap();
        let host_id = host.id;
        endpoint.replace_address(host);
        let _inside_runtime = runtime.enter();
        let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(GOSSIP_ALPN, gossip.clone())
            .spawn();
        let Ok(joined) = runtime.block_on(tokio::time::timeout(
            JOIN_WAIT,
            gossip.subscribe_and_join(topic, vec![host_id]),
        )) else {
            panic!("the host was no neighbor within {JOIN_WAIT:?}")
        };
        let joined = joined.unwrap();
        let (topic, receiver) = joined.split();
        runtime.spawn(drain(receiver));
        Self {
            runtime: driven,
            router,
            topic,
        }
    }

    /// Broadcasts the bytes on the topic as they are.
    pub fn broadcast(&self, payload: Vec<u8>) {
        self.runtime
            .handle()
            .block_on(self.topic.broadcast(payload.into()))
            .unwrap();
    }
}

impl Drop for Forger {
    fn drop(&mut self) {
        drop(self.runtime.handle().block_on(self.router.shutdown()));
    }
}

/// Reads every event of the subscription, so the topic never drops it for lagging.
async fn drain(mut receiver: GossipReceiver) {
    while let Some(Ok(_event)) = receiver.next().await {}
}
