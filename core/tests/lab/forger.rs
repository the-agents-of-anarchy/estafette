use std::time::Duration;

use estafette::crypto::GateKey;
use estafette::net::endpoint::{DIRECT_ALPN, NodeEndpoint};
use estafette::stream_items::StreamExt;
use iroh::protocol::{ProtocolHandler, Router, RouterBuilder};
use iroh::{EndpointAddr, SecretKey};
use iroh_gossip::api::{GossipReceiver, GossipSender};
use iroh_gossip::{ALPN as GOSSIP_ALPN, TopicId};
use tokio::runtime::Handle;

use crate::driven_runtime::DrivenRuntime;
use crate::gated_endpoint::{bind_gated_endpoint, unrecorded_connections};

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
        Self::join(key, gate_key, topic, host, |_endpoint| |router| router)
    }

    /// Joins as `join_through` does, also answering the direct protocol with the handler it builds on the node's endpoint.
    #[track_caller]
    pub fn join_through_answering_direct<Direct: ProtocolHandler>(
        key: SecretKey,
        gate_key: GateKey,
        topic: TopicId,
        host: EndpointAddr,
        direct: impl FnOnce(&NodeEndpoint) -> Direct,
    ) -> Self {
        Self::join(key, gate_key, topic, host, |endpoint| {
            let handler = direct(endpoint);
            move |router: RouterBuilder| router.accept(DIRECT_ALPN, handler)
        })
    }

    /// Binds and joins as `join_through` does, serving beside gossip what the extension, built on the node's endpoint, adds to the router.
    #[track_caller]
    fn join<Extension: FnOnce(RouterBuilder) -> RouterBuilder>(
        key: SecretKey,
        gate_key: GateKey,
        topic: TopicId,
        host: EndpointAddr,
        extension_on: impl FnOnce(&NodeEndpoint) -> Extension,
    ) -> Self {
        let driven = DrivenRuntime::start();
        let runtime = driven.handle().clone();
        let endpoint = bind_gated_endpoint(
            &runtime,
            key,
            gate_key,
            host.relay_urls().cloned(),
            |_closing| {},
            unrecorded_connections(),
        );
        let host_id = host.id;
        endpoint.replace_address(host);
        let _inside_runtime = runtime.enter();
        let gossip = endpoint.endpoint().spawn_gossip();
        let extension = extension_on(&endpoint);
        let router = extension(
            endpoint
                .endpoint()
                .router_builder()
                .accept(GOSSIP_ALPN, gossip.clone()),
        )
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

    /// The runtime the forger's tasks run on, which the test thread may block on.
    pub const fn runtime(&self) -> &Handle {
        self.runtime.handle()
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
