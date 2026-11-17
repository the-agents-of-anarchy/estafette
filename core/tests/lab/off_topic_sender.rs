use estafette::crypto::NetworkKeys;
use estafette::net::direct::{ClientTimeouts, DirectClient, DirectError, DirectHost, DirectServer};
use estafette::net::endpoint::{DIRECT_ALPN, NodeEndpoint};
use estafette::wire::{self, Addr, Introduce, IntroduceAck, Message, MessageAck, Version};
use iroh::protocol::Router;
use iroh::{EndpointAddr, RelayUrl, SecretKey};

use crate::driven_runtime::DrivenRuntime;
use crate::gated_endpoint::{bind_gated_endpoint, unrecorded_connections};
use crate::payload_placeholders::UNSET_MAC;

/// The public mapping the card introduces itself with, an empty one.
const EMPTY_PUBLIC: &[u8] = b"{}";

/// A card whose node never joins the network's topic: it holds no gossip at all, serves the direct protocol alone, sends Messages sealed under the network keys and answers every Introduce under its handle.
pub struct OffTopicSender {
    runtime: DrivenRuntime,
    endpoint: NodeEndpoint,
    router: Router,
    client: DirectClient,
}

impl OffTopicSender {
    /// Binds under the card key behind the network gate, homed on the relays, and serves the direct protocol alone, answering every Introduce with the handle.
    pub fn start(
        keys: &NetworkKeys,
        relays: impl IntoIterator<Item = RelayUrl>,
        key: SecretKey,
        handle: &str,
    ) -> Self {
        let runtime = DrivenRuntime::start();
        let card = *key.public().as_bytes();
        let connections = unrecorded_connections();
        let endpoint = bind_gated_endpoint(
            runtime.handle(),
            key,
            *keys.gate_key(),
            relays,
            |_closing| {},
            connections.clone(),
        );
        let _inside_runtime = runtime.handle().enter();
        let introduced = IntroducedUnderItsHandle {
            card,
            handle: handle.to_owned(),
        };
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(
                DIRECT_ALPN,
                DirectServer::new(NetworkKeys::clone(keys), introduced, &endpoint),
            )
            .spawn();
        let client = DirectClient::new(
            endpoint.endpoint().clone(),
            NetworkKeys::clone(keys),
            ClientTimeouts::SPEC,
            connections,
        );
        Self {
            runtime,
            endpoint,
            router,
            client,
        }
    }

    /// Sends the message over the direct protocol to the node at the address and returns its acknowledgement.
    pub fn send(&self, to: EndpointAddr, message: Message) -> Result<MessageAck, DirectError> {
        let recipient = to.id;
        self.endpoint.replace_address(to);
        self.runtime
            .handle()
            .block_on(self.client.send_message(recipient, message))
    }
}

impl Drop for OffTopicSender {
    fn drop(&mut self) {
        drop(self.runtime.handle().block_on(self.router.shutdown()));
    }
}

/// The direct side of a card that receives no Message and introduces itself under its handle.
struct IntroducedUnderItsHandle {
    card: wire::PublicKey,
    handle: String,
}

impl DirectHost for IntroducedUnderItsHandle {
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
            public: EMPTY_PUBLIC.to_vec(),
            attached: false,
            addr: Addr::from_endpoint_report(Vec::<String>::new(), []),
            mac: UNSET_MAC,
        }
    }
}
