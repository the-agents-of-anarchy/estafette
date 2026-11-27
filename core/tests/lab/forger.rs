use std::sync::Arc;
use std::time::Duration;

use estafette::crypto::GateKey;
use estafette::net::endpoint::{DIRECT_ALPN, NodeEndpoint};
use estafette::stream_items::StreamExt;
use iroh::protocol::{ProtocolHandler, Router, RouterBuilder};
use iroh::{EndpointAddr, SecretKey};
use iroh_gossip::api::{Event, GossipReceiver, GossipSender};
use iroh_gossip::{ALPN as GOSSIP_ALPN, TopicId};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::driven_runtime::DrivenRuntime;
use crate::gated_endpoint::{bind_gated_endpoint, unrecorded_connections};

/// How long the forger may take to gain the host as a gossip neighbor.
const JOIN_WAIT: Duration = Duration::from_secs(30);

/// A node joined to the live network's topic through its host, broadcasting whatever bytes the test hands it and keeping every payload the topic gives it.
pub struct Forger {
    runtime: DrivenRuntime,
    router: Router,
    topic: GossipSender,
    payloads_given: Arc<watch::Sender<Vec<Vec<u8>>>>,
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
        let payloads_given = Arc::new(watch::Sender::new(Vec::new()));
        runtime.spawn(keep_received(receiver, Arc::clone(&payloads_given)));
        Self {
            runtime: driven,
            router,
            topic,
            payloads_given,
        }
    }

    /// The runtime the forger's tasks run on, which the test thread may block on.
    pub const fn runtime(&self) -> &Handle {
        self.runtime.handle()
    }

    /// The first payload the topic gave this node that satisfies `is_awaited`, woken by each payload, failing with what was awaited when the bound runs out first.
    #[track_caller]
    pub fn wait_for_received(
        &self,
        awaited: &str,
        within: Duration,
        is_awaited: impl Fn(&[u8]) -> bool,
    ) -> Vec<u8> {
        let mut payloads_awaited = self.payloads_given.subscribe();
        let found = self.runtime.handle().block_on(async {
            let seen = tokio::time::timeout(
                within,
                payloads_awaited
                    .wait_for(|payloads| payloads.iter().any(|payload| is_awaited(payload))),
            )
            .await
            .ok()?
            .ok()?;
            seen.iter().find(|payload| is_awaited(payload)).cloned()
        });
        found.unwrap_or_else(|| panic!("no {awaited} within {within:?}"))
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

/// Keeps the content of every message the subscription gives and reads every event, so the topic never drops it for lagging.
async fn keep_received(
    mut receiver: GossipReceiver,
    payloads_given: Arc<watch::Sender<Vec<Vec<u8>>>>,
) {
    while let Some(Ok(event)) = receiver.next().await {
        if let Event::Received(message) = event {
            payloads_given.send_modify(|payloads| payloads.push(message.content.to_vec()));
        }
    }
}
