use std::fmt;
use std::time::Duration;

use iroh::{EndpointId, PublicKey, SecretKey};
use iroh_gossip::api::{ApiError, Event, GossipReceiver, GossipSender, JoinOptions};
use iroh_gossip::{Gossip, TopicId};
use n0_future::StreamExt;
use tokio::time::{Instant, sleep_until};

use crate::crypto::NetworkKeys;
use crate::payload::{PayloadError, open_payload, seal_payload};
use crate::wire::Payload;

const JOIN_TIMEOUT: Duration = Duration::from_millis(5000);

/// What the node learns from the gossip topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GossipEvent {
    Joined,
    JoinTimedOut,
    NeighborUp(EndpointId),
    NeighborDown(EndpointId),
    Payload(Box<Payload>),
}

#[derive(Debug)]
pub enum GossipError {
    Seal(PayloadError),
    Topic(ApiError),
    Lagged,
}

impl fmt::Display for GossipError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Seal(source) => source.fmt(formatter),
            Self::Topic(source) => source.fmt(formatter),
            Self::Lagged => {
                formatter.write_str("the topic dropped this node for reading its events too slowly")
            }
        }
    }
}

impl std::error::Error for GossipError {}

/// The network's gossip topic, joined, handing the node only payloads that passed verification.
pub struct NetworkTopic {
    sender: GossipSender,
    receiver: GossipReceiver,
    keys: NetworkKeys,
    owner: PublicKey,
    join_progress: JoinProgress,
    neighbor_up_after_joined: Option<EndpointId>,
}

#[derive(Clone, Copy)]
enum JoinProgress {
    AwaitingFirstNeighbor { deadline: Instant },
    TimedOut,
    Joined,
}

impl NetworkTopic {
    /// Subscribes to the network's topic with the known hosts as bootstrap, counting the join timeout from `now`.
    ///
    /// # Errors
    /// `GossipError::Topic` when the gossip actor is gone.
    pub async fn join(
        gossip: &Gossip,
        keys: NetworkKeys,
        owner: PublicKey,
        bootstrap: Vec<EndpointId>,
        now: Instant,
    ) -> Result<Self, GossipError> {
        let topic_id = TopicId::from_bytes(*keys.topic_id().as_bytes());
        let topic = gossip
            .subscribe_with_opts(topic_id, JoinOptions::with_bootstrap(bootstrap))
            .await
            .map_err(GossipError::Topic)?;
        let (sender, receiver) = topic.split();
        Ok(Self {
            sender,
            receiver,
            keys,
            owner,
            join_progress: JoinProgress::AwaitingFirstNeighbor {
                deadline: now.checked_add(JOIN_TIMEOUT).unwrap_or(now),
            },
            neighbor_up_after_joined: None,
        })
    }

    /// Signs, macs and broadcasts the payload, refusing one past the gossip frame limit.
    ///
    /// # Errors
    /// `GossipError::Seal` for an oversize or unsignable payload, `GossipError::Topic` when the gossip actor is gone.
    pub async fn broadcast(&self, signer: &SecretKey, payload: Payload) -> Result<(), GossipError> {
        let sealed = seal_payload(&self.keys, signer, payload).map_err(GossipError::Seal)?;
        self.sender
            .broadcast(sealed.into())
            .await
            .map_err(GossipError::Topic)
    }

    /// The next event of the topic, an error naming why the topic failed, or none once the topic has closed cleanly.
    pub async fn next_event(&mut self) -> Option<Result<GossipEvent, GossipError>> {
        loop {
            if let Some(neighbor) = self.neighbor_up_after_joined.take() {
                return Some(Ok(GossipEvent::NeighborUp(neighbor)));
            }
            let incoming = match self.join_progress {
                JoinProgress::AwaitingFirstNeighbor { deadline } => {
                    tokio::select! {
                        incoming = self.receiver.next() => incoming,
                        () = sleep_until(deadline) => {
                            self.join_progress = JoinProgress::TimedOut;
                            return Some(Ok(GossipEvent::JoinTimedOut));
                        }
                    }
                }
                JoinProgress::TimedOut | JoinProgress::Joined => self.receiver.next().await,
            };
            let event = match incoming? {
                Ok(event) => event,
                Err(source) => return Some(Err(GossipError::Topic(source))),
            };
            if let Some(surfaced) = self.surface(event) {
                return Some(surfaced);
            }
        }
    }

    fn surface(&mut self, event: Event) -> Option<Result<GossipEvent, GossipError>> {
        match event {
            Event::NeighborUp(neighbor) => Some(Ok(self.announce_neighbor_up(neighbor))),
            Event::NeighborDown(neighbor) => Some(Ok(GossipEvent::NeighborDown(neighbor))),
            Event::Received(message) => verified_payload(&self.keys, &self.owner, &message.content)
                .map(|payload| Ok(GossipEvent::Payload(Box::new(payload)))),
            Event::Lagged => Some(Err(GossipError::Lagged)),
        }
    }

    const fn announce_neighbor_up(&mut self, neighbor: EndpointId) -> GossipEvent {
        match self.join_progress {
            JoinProgress::AwaitingFirstNeighbor { .. } | JoinProgress::TimedOut => {
                self.join_progress = JoinProgress::Joined;
                self.neighbor_up_after_joined = Some(neighbor);
                GossipEvent::Joined
            }
            JoinProgress::Joined => GossipEvent::NeighborUp(neighbor),
        }
    }
}

fn verified_payload(keys: &NetworkKeys, owner: &PublicKey, bytes: &[u8]) -> Option<Payload> {
    open_payload(keys, owner, bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KEY_LEN, MAC_LEN, NetworkSecret, sign_host_certificate};
    use crate::net::endpoint::NodeEndpoint;
    use crate::wire::{
        Addr, Departure, Infrastructure, Presence, RelayUrl, SignatureBytes, Version, encode,
    };
    use iroh::Signature;
    use iroh_gossip::ALPN as GOSSIP_ALPN;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    const NETWORK_SECRET: NetworkSecret = NetworkSecret::from_bytes([9; KEY_LEN]);
    const OWNER_SEED: u8 = 40;
    const FIRST_SEED: u8 = 41;
    const SECOND_SEED: u8 = 42;
    const OUTSIDER_SEED: u8 = 43;
    const HOST_SEED: u8 = 44;
    const IMPOSTOR_OWNER_SEED: u8 = 45;
    const ABSENT_BOOTSTRAP_SEED: u8 = 46;
    const OUTSIDER_SECRET: NetworkSecret = NetworkSecret::from_bytes([10; KEY_LEN]);
    const HOST_RELAY: &str = "https://relay.example/";
    const EVENTS_PAST_THE_SUBSCRIPTION_CAPACITY: u64 = 4000;
    const LAG_WAIT: Duration = Duration::from_secs(30);
    const SPEC_JOIN_TIMEOUT: Duration = Duration::from_millis(5000);
    const JOIN_TIMEOUT_SLACK: Duration = Duration::from_millis(1000);
    const NOTHING_MORE_WAIT: Duration = Duration::from_millis(500);
    const OVERSIZE_PUBLIC_BYTES: usize = 4000;
    const EVENT_WAIT: Duration = Duration::from_millis(8000);

    fn keys() -> NetworkKeys {
        NetworkKeys::derive(&NETWORK_SECRET)
    }

    fn secret_key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; KEY_LEN])
    }

    struct Member {
        node: NodeEndpoint,
        gossip: Gossip,
        _accepting: JoinHandle<()>,
    }

    async fn bind_member(seed: u8) -> Member {
        bind_member_with_gate_of(seed, &NETWORK_SECRET).await
    }

    async fn bind_member_with_gate_of(seed: u8, secret: &NetworkSecret) -> Member {
        let node = NodeEndpoint::bind(
            secret_key(seed),
            *NetworkKeys::derive(secret).gate_key(),
            Vec::new(),
            move |nonce: &mut [u8; KEY_LEN]| *nonce = [seed; KEY_LEN],
            |_ignored| {},
        )
        .await
        .unwrap();
        let gossip = Gossip::builder().spawn(node.endpoint().clone());
        let accepting = tokio::spawn(accept_gossip(node.endpoint().clone(), gossip.clone()));
        Member {
            node,
            gossip,
            _accepting: accepting,
        }
    }

    async fn accept_gossip(endpoint: iroh::Endpoint, gossip: Gossip) {
        while let Some(incoming) = endpoint.accept().await {
            let Ok(connection) = incoming.await else {
                continue;
            };
            if connection.alpn() == GOSSIP_ALPN {
                gossip.handle_connection(connection).await.unwrap();
            }
        }
    }

    fn teach_address(learner: &Member, known: &Member) {
        learner.node.replace_address(known.node.endpoint().addr());
    }

    async fn join_topic(member: &Member, bootstrap: Vec<EndpointId>) -> NetworkTopic {
        NetworkTopic::join(
            &member.gossip,
            keys(),
            secret_key(OWNER_SEED).public(),
            bootstrap,
            Instant::now(),
        )
        .await
        .unwrap()
    }

    async fn event_where<T>(
        topic: &mut NetworkTopic,
        mut pick: impl FnMut(GossipEvent) -> Option<T>,
    ) -> T {
        timeout(EVENT_WAIT, async {
            loop {
                let event = topic.next_event().await.unwrap().unwrap();
                if let Some(picked) = pick(event) {
                    return picked;
                }
            }
        })
        .await
        .unwrap()
    }

    async fn joined(topic: &mut NetworkTopic) {
        event_where(topic, |event| (event == GossipEvent::Joined).then_some(())).await;
    }

    async fn next_payload(topic: &mut NetworkTopic) -> Payload {
        event_where(topic, |event| match event {
            GossipEvent::Payload(payload) => Some(*payload),
            GossipEvent::Joined
            | GossipEvent::JoinTimedOut
            | GossipEvent::NeighborUp(_)
            | GossipEvent::NeighborDown(_) => None,
        })
        .await
    }

    fn presence_of(seed: u8, handle: &str, seq: u64) -> Payload {
        Payload::Presence(Presence {
            v: Version::CURRENT,
            card: *secret_key(seed).public().as_bytes(),
            handle: handle.to_owned(),
            public: b"{}".to_vec(),
            attached: true,
            addr: Addr::from_endpoint_report(
                ["https://relay.example/".to_owned()],
                std::iter::empty(),
            ),
            seq,
            mac: [0; 32],
            sig: SignatureBytes([0; 64]),
        })
    }

    fn presence_card(payload: Payload) -> [u8; 32] {
        let Payload::Presence(presence) = payload else {
            panic!("a presence was expected");
        };
        presence.card
    }

    fn forged_departure(signature: SignatureBytes, mac_of: impl Fn(&[u8]) -> [u8; 32]) -> Vec<u8> {
        let unsealed = Departure {
            v: Version::CURRENT,
            card: *secret_key(FIRST_SEED).public().as_bytes(),
            seq: 3,
            mac: [0; 32],
            sig: signature,
        };
        let mac = mac_of(&encode(&Payload::Departure(unsealed.clone())).unwrap());
        encode(&Payload::Departure(Departure { mac, ..unsealed })).unwrap()
    }

    fn uncertified_infrastructure() -> Payload {
        Payload::Infrastructure(Infrastructure {
            v: Version::CURRENT,
            host: *secret_key(HOST_SEED).public().as_bytes(),
            relay: RelayUrl::try_from(HOST_RELAY.to_owned()).unwrap(),
            addr: Addr::from_endpoint_report([HOST_RELAY.to_owned()], std::iter::empty()),
            ts: 1_700_000_000,
            cert: SignatureBytes(
                sign_host_certificate(
                    &secret_key(IMPOSTOR_OWNER_SEED),
                    &secret_key(HOST_SEED).public(),
                )
                .to_bytes(),
            ),
            mac: [0; 32],
            sig: SignatureBytes([0; 64]),
        })
    }

    struct JoinedPair {
        first_topic: NetworkTopic,
        second_topic: NetworkTopic,
        _first: Member,
        _second: Member,
    }

    async fn joined_pair() -> JoinedPair {
        let first = bind_member(FIRST_SEED).await;
        let second = bind_member(SECOND_SEED).await;
        teach_address(&second, &first);
        let mut first_topic = join_topic(&first, Vec::new()).await;
        let mut second_topic = join_topic(&second, vec![first.node.endpoint().id()]).await;
        joined(&mut first_topic).await;
        joined(&mut second_topic).await;
        JoinedPair {
            first_topic,
            second_topic,
            _first: first,
            _second: second,
        }
    }

    async fn nothing_more_arrives(topic: &mut NetworkTopic) {
        let more = timeout(NOTHING_MORE_WAIT, next_payload(topic)).await;
        more.unwrap_err();
    }

    async fn dropped_while_the_next_good_presence_arrives(forged: Vec<u8>) {
        let JoinedPair {
            mut first_topic,
            second_topic,
            ..
        } = joined_pair().await;
        second_topic.sender.broadcast(forged.into()).await.unwrap();
        second_topic
            .broadcast(&secret_key(SECOND_SEED), presence_of(SECOND_SEED, "bob", 8))
            .await
            .unwrap();

        let arrived = next_payload(&mut first_topic).await;

        assert_eq!(
            presence_card(arrived),
            *secret_key(SECOND_SEED).public().as_bytes()
        );
        nothing_more_arrives(&mut first_topic).await;
    }

    #[tokio::test]
    async fn test_10_payload_with_a_valid_mac_and_a_signature_that_does_not_verify_never_reaches_the_node()
     {
        let keys = keys();
        let forged = forged_departure(SignatureBytes([5; 64]), |zeroed| {
            *keys.mac(zeroed).as_bytes()
        });

        dropped_while_the_next_good_presence_arrives(forged).await;
    }

    #[tokio::test]
    async fn spec_3_7_payload_with_a_wrong_mac_never_reaches_the_node() {
        let mut wrong_mac = seal_payload(
            &keys(),
            &secret_key(FIRST_SEED),
            Payload::Departure(Departure {
                v: Version::CURRENT,
                card: *secret_key(FIRST_SEED).public().as_bytes(),
                seq: 3,
                mac: [0; 32],
                sig: SignatureBytes([0; 64]),
            }),
        )
        .unwrap();
        let mac_first_byte = wrong_mac.len() - Signature::LENGTH - MAC_LEN;
        wrong_mac[mac_first_byte] ^= 1;

        dropped_while_the_next_good_presence_arrives(wrong_mac).await;
    }

    #[tokio::test]
    async fn test_27_infrastructure_of_a_host_the_owner_never_certified_never_reaches_the_node() {
        let sealed = seal_payload(
            &keys(),
            &secret_key(HOST_SEED),
            uncertified_infrastructure(),
        )
        .unwrap();

        dropped_while_the_next_good_presence_arrives(sealed).await;
    }

    #[tokio::test]
    async fn spec_3_7_broadcast_past_the_gossip_frame_fails_and_leaves_the_link_alive() {
        let JoinedPair {
            mut first_topic,
            second_topic,
            ..
        } = joined_pair().await;
        let Payload::Presence(presence) = presence_of(SECOND_SEED, "bob", 9) else {
            panic!("presence_of builds a presence");
        };
        let oversize = Payload::Presence(Presence {
            public: vec![1; OVERSIZE_PUBLIC_BYTES],
            ..presence
        });

        let refused = second_topic
            .broadcast(&secret_key(SECOND_SEED), oversize)
            .await;
        second_topic
            .broadcast(
                &secret_key(SECOND_SEED),
                presence_of(SECOND_SEED, "bob", 10),
            )
            .await
            .unwrap();

        assert!(matches!(
            refused,
            Err(GossipError::Seal(PayloadError::OverGossipFrame { .. }))
        ));
        let Payload::Presence(arrived) = next_payload(&mut first_topic).await else {
            panic!("the link must carry the next presence");
        };
        assert_eq!(arrived.seq, 10);
    }

    #[tokio::test]
    async fn spec_3_14_join_without_a_neighbor_reports_a_timeout_at_5_seconds() {
        let lonely = bind_member(FIRST_SEED).await;
        let absent = secret_key(ABSENT_BOOTSTRAP_SEED).public();
        let started = Instant::now();
        let mut topic = NetworkTopic::join(
            &lonely.gossip,
            keys(),
            secret_key(OWNER_SEED).public(),
            vec![absent],
            started,
        )
        .await
        .unwrap();

        let event = topic.next_event().await.unwrap().unwrap();

        assert_eq!(event, GossipEvent::JoinTimedOut);
        assert!(started.elapsed() >= SPEC_JOIN_TIMEOUT);
        assert!(started.elapsed() < SPEC_JOIN_TIMEOUT + JOIN_TIMEOUT_SLACK);
    }

    #[tokio::test]
    async fn spec_3_7_neighbor_up_follows_joined_and_neighbor_down_follows_a_shutdown() {
        let first = bind_member(FIRST_SEED).await;
        let second = bind_member(SECOND_SEED).await;
        teach_address(&second, &first);
        let mut first_topic = join_topic(&first, Vec::new()).await;
        let _second_topic = join_topic(&second, vec![first.node.endpoint().id()]).await;
        let second_id = second.node.endpoint().id();

        let first_event = event_where(&mut first_topic, Some).await;
        let second_event = event_where(&mut first_topic, Some).await;
        second.gossip.shutdown().await.unwrap();
        let down = event_where(&mut first_topic, |event| {
            matches!(event, GossipEvent::NeighborDown(_)).then_some(event)
        })
        .await;

        assert_eq!(first_event, GossipEvent::Joined);
        assert_eq!(second_event, GossipEvent::NeighborUp(second_id));
        assert_eq!(down, GossipEvent::NeighborDown(second_id));
    }

    #[tokio::test]
    async fn test_21_node_holding_the_topic_id_but_not_the_secret_gets_no_neighbor_and_no_payload()
    {
        let member = bind_member(FIRST_SEED).await;
        let outsider = bind_member_with_gate_of(OUTSIDER_SEED, &OUTSIDER_SECRET).await;
        teach_address(&outsider, &member);
        teach_address(&member, &outsider);
        let member_topic = join_topic(&member, vec![outsider.node.endpoint().id()]).await;
        let mut outsider_topic = join_topic(&outsider, vec![member.node.endpoint().id()]).await;
        member_topic
            .broadcast(&secret_key(FIRST_SEED), presence_of(FIRST_SEED, "amy", 1))
            .await
            .unwrap();

        let first_outsider_event = timeout(EVENT_WAIT, outsider_topic.next_event())
            .await
            .unwrap()
            .unwrap()
            .unwrap();

        assert_eq!(first_outsider_event, GossipEvent::JoinTimedOut);
        nothing_more_arrives(&mut outsider_topic).await;
        assert!(member_topic.receiver.neighbors().next().is_none());
    }

    #[tokio::test]
    async fn spec_3_7_topic_closed_by_a_gossip_shutdown_ends_with_none() {
        let member = bind_member(FIRST_SEED).await;
        let mut topic = join_topic(&member, Vec::new()).await;
        member.gossip.shutdown().await.unwrap();

        let ended = timeout(EVENT_WAIT, topic.next_event()).await.unwrap();

        assert!(ended.is_none());
    }

    #[tokio::test]
    async fn spec_3_7_node_that_falls_behind_the_topic_gets_a_lagged_error_not_a_silent_end() {
        let JoinedPair {
            mut first_topic,
            second_topic,
            ..
        } = joined_pair().await;
        for seq in 0..EVENTS_PAST_THE_SUBSCRIPTION_CAPACITY {
            second_topic
                .broadcast(
                    &secret_key(SECOND_SEED),
                    presence_of(SECOND_SEED, "bob", seq),
                )
                .await
                .unwrap();
        }

        let first_error = timeout(LAG_WAIT, async {
            loop {
                if let Err(error) = first_topic.next_event().await.unwrap() {
                    return error;
                }
            }
        })
        .await
        .unwrap();

        assert!(matches!(first_error, GossipError::Lagged));
    }

    #[tokio::test]
    async fn spec_3_7_two_nodes_exchange_a_signed_presence() {
        let JoinedPair {
            mut first_topic,
            second_topic,
            ..
        } = joined_pair().await;

        second_topic
            .broadcast(&secret_key(SECOND_SEED), presence_of(SECOND_SEED, "bob", 7))
            .await
            .unwrap();
        let received = next_payload(&mut first_topic).await;

        let Payload::Presence(presence) = received else {
            panic!("a presence was broadcast");
        };
        assert_eq!(presence.card, *secret_key(SECOND_SEED).public().as_bytes());
        assert_eq!(presence.handle, "bob");
        assert_eq!(presence.seq, 7);
    }
}
