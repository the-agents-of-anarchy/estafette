//! The rebind of spec 3.8 and D46: when a member falls deaf or its join gains no neighbor, the rebind itself, and the member join loop after it.

use std::collections::BTreeSet;
use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use n0_future::join_all;
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};

use crate::net::direct::DirectHost;
use crate::net::gossip::NetworkTopic;
use crate::node::presence::{
    NodeRecord, PresenceLoop, RelayLossWatch, endpoint_id, home_relay_changes, own_address_changes,
    seed_lookup_with_known_hosts, serve,
};
use crate::peers::Status;

/// Silence after which a member that knows another node is deaf and rebinds (spec:781, spec:1016, D46).
const DEAF_TIMEOUT: Duration = Duration::from_secs(15);
/// The least time between any two rebinds (spec:781, spec:786, spec:1017).
const REBIND_INTERVAL: Duration = Duration::from_secs(20);
/// How long a rebind waits for a known host's relay to answer its TCP probe (spec:786, spec:1019).
const RELAY_PROBE_TIMEOUT: Duration = Duration::from_millis(1000);
/// How long after a rebind's join the member waits for a member neighbor before asking to join the members (spec:782, spec:1020).
const MEMBER_JOIN_WAIT: Duration = Duration::from_millis(1500);
/// The time between two asks to join the members (spec:782, spec:1021).
const MEMBER_JOIN_INTERVAL: Duration = Duration::from_millis(3000);
/// The most asks to join the members after one rebind (spec:782, spec:1012).
const MEMBER_JOIN_TRIES: u32 = 10;

/// Whether a relay answers the probe of spec 3.8, as the composition root hands it to the node.
pub type RelayProbe =
    Arc<dyn Fn(RelayUrl) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Whether a TCP connect to the relay's host and port succeeds, the relay probe the composition root hands the node.
pub async fn relay_answers(relay: RelayUrl) -> bool {
    let (Some(host), Some(port)) = (relay.host_str(), relay.port_or_known_default()) else {
        return false;
    };
    TcpStream::connect(format!("{host}:{port}")).await.is_ok()
}

/// When this member is due to rebind: after the deaf timeout of silence or at a join that gained no neighbor, never sooner than the rebind interval after the last rebind (spec 3.8, D46).
pub(super) struct RebindSchedule {
    silent_since: Instant,
    unjoined_since: Option<Instant>,
    last_rebind: Option<Instant>,
}

impl RebindSchedule {
    /// The schedule of a member that has heard nothing since it subscribed.
    pub(super) const fn new(subscribed: Instant) -> Self {
        Self {
            silent_since: subscribed,
            unjoined_since: None,
            last_rebind: None,
        }
    }

    /// Notes a gossip payload heard.
    pub(super) const fn heard(&mut self, now: Instant) {
        self.silent_since = now;
    }

    /// Notes that the topic's join gained no neighbor within the join timeout.
    pub(super) const fn join_timed_out(&mut self, now: Instant) {
        self.unjoined_since = Some(now);
    }

    /// Notes that the topic reported joined.
    pub(super) const fn joined(&mut self) {
        self.unjoined_since = None;
    }

    /// Notes a rebind, whose fresh topic has not joined yet.
    pub(super) const fn rebound(&mut self, now: Instant) {
        self.last_rebind = Some(now);
        self.unjoined_since = None;
    }

    /// When the next rebind is due, for a member that may rebind.
    pub(super) fn due(&self) -> Instant {
        let deaf_at = later_by(self.silent_since, DEAF_TIMEOUT);
        let triggered_at = self
            .unjoined_since
            .map_or(deaf_at, |unjoined| unjoined.min(deaf_at));
        self.last_rebind.map_or(triggered_at, |last| {
            triggered_at.max(later_by(last, REBIND_INTERVAL))
        })
    }
}

/// The member join loop of spec 3.8 after a rebind: when the next ask to join the members is due, and how many asks are left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MemberJoins {
    Idle,
    AwaitingRebindJoin,
    Asking { next: Instant, asks_left: u32 },
}

impl MemberJoins {
    /// Starts the member join wait when the topic of a rebind reports joined.
    pub(super) fn joined(&mut self, now: Instant) {
        if *self == Self::AwaitingRebindJoin {
            *self = Self::Asking {
                next: later_by(now, MEMBER_JOIN_WAIT),
                asks_left: MEMBER_JOIN_TRIES,
            };
        }
    }

    /// When the next ask is due, if one is.
    pub(super) const fn next_ask(&self) -> Option<Instant> {
        match self {
            Self::Asking { next, .. } => Some(*next),
            Self::Idle | Self::AwaitingRebindJoin => None,
        }
    }

    /// Counts an ask and schedules the next one the member join interval later, or stops after the member join tries.
    pub(super) fn asked(&mut self, now: Instant) {
        *self = match *self {
            Self::Asking { asks_left, .. } => asks_left
                .checked_sub(1)
                .filter(|left| *left > 0)
                .map_or(Self::Idle, |left| Self::Asking {
                    next: later_by(now, MEMBER_JOIN_INTERVAL),
                    asks_left: left,
                }),
            Self::Idle | Self::AwaitingRebindJoin => Self::Idle,
        };
    }
}

impl PresenceLoop {
    /// Whether this member may rebind: it knows another node, a known host or an Online peer, and is not stopping.
    pub(super) fn may_rebind(&self) -> bool {
        !self.state.is_stopping()
            && (!self.known_hosts.is_empty() || !self.peers_with_status(Status::Online).is_empty())
    }

    /// Rebinds by spec 3.8, stopping the loop when no endpoint or topic comes of it.
    pub(super) async fn rebind(&mut self) -> ControlFlow<()> {
        (self.report)(NodeRecord::Rebinding);
        self.schedule.rebound((self.monotonic)());
        self.member_joins = MemberJoins::AwaitingRebindJoin;
        self.neighbors.clear();
        self.close_endpoint().await;
        let down = self.relays_found_down().await;
        self.strip_relays_from_members(&down);
        if let Err(failure) = self.endpoint.rebind().await {
            (self.report)(NodeRecord::RebindFailed(failure));
            return ControlFlow::Break(());
        }
        self.client.follow(self.endpoint.endpoint().clone()).await;
        self.subscribe_through_known_nodes().await
    }

    /// Asks the gossip layer to join the Online and Unreachable members, stopping the asks when there are none or one of them is a gossip neighbor already.
    pub(super) async fn ask_members_to_join(&mut self) {
        let mut members = self.peers_with_status(Status::Online);
        members.extend(self.peers_with_status(Status::Unreachable));
        if members.is_empty() || members.iter().any(|member| self.neighbors.contains(member)) {
            self.member_joins = MemberJoins::Idle;
            return;
        }
        let now = (self.monotonic)();
        if let Err(failure) =
            NetworkTopic::join(&self.gossip, self.keys.clone(), self.owner, members, now).await
        {
            (self.report)(NodeRecord::TopicFailed(failure));
        }
        self.member_joins.asked(now);
    }

    /// The node ids of the peers whose status is `wanted`.
    fn peers_with_status(&self, wanted: Status) -> Vec<EndpointId> {
        self.state
            .table()
            .peers(self.state.now_ms())
            .filter(|peer| peer.status == wanted)
            .filter_map(|peer| endpoint_id(&peer.card))
            .collect()
    }

    /// Closes the endpoint with the direct listener and the gossip instance on it, and with them every connection, recording a shutdown that failed.
    async fn close_endpoint(&self) {
        if let Err(failure) = self.router.shutdown().await {
            (self.report)(NodeRecord::RouterShutdownFailed(failure));
        }
    }

    /// The relays of the known hosts that do not answer the probe within the relay probe timeout.
    async fn relays_found_down(&self) -> BTreeSet<RelayUrl> {
        let relays: BTreeSet<RelayUrl> = self
            .known_hosts
            .addresses()
            .into_iter()
            .map(|host| host.relay)
            .collect();
        let deadline = later_by((self.monotonic)(), RELAY_PROBE_TIMEOUT);
        let answers = join_all(
            relays
                .iter()
                .map(|relay| timeout_at(deadline, (self.probe_relay)(relay.clone()))),
        )
        .await;
        relays
            .into_iter()
            .zip(answers)
            .filter(|(_, answer)| !matches!(answer, Ok(true)))
            .map(|(relay, _)| relay)
            .collect()
    }

    /// Strips the relays found down from the lookup entry of every other member.
    fn strip_relays_from_members(&self, down: &BTreeSet<RelayUrl>) {
        let members: Vec<EndpointId> = self
            .state
            .table()
            .peers(self.state.now_ms())
            .filter_map(|peer| endpoint_id(&peer.card))
            .collect();
        for member in members {
            if let Some(address) = self.endpoint.address_of(member) {
                self.endpoint.replace_address(without_relays(address, down));
            }
        }
    }

    /// Serves the fresh endpoint and subscribes to the topic through every known host and every Online peer whose address the lookup holds.
    async fn subscribe_through_known_nodes(&mut self) -> ControlFlow<()> {
        let hosts = self.known_hosts.addresses();
        seed_lookup_with_known_hosts(&self.endpoint, &hosts);
        let mut bootstrap: Vec<EndpointId> = hosts.iter().map(|host| host.id).collect();
        bootstrap.extend(
            self.peers_with_status(Status::Online)
                .into_iter()
                .filter(|peer| self.endpoint.address_of(*peer).is_some()),
        );
        let (router, gossip) = serve(&self.endpoint, &self.keys, &self.state);
        self.router = router;
        self.gossip = gossip;
        self.own_addresses = own_address_changes(self.endpoint.endpoint());
        self.relay_statuses = home_relay_changes(self.endpoint.endpoint());
        self.relay_loss = RelayLossWatch::default();
        match NetworkTopic::join(
            &self.gossip,
            self.keys.clone(),
            self.owner,
            bootstrap,
            (self.monotonic)(),
        )
        .await
        {
            Ok(topic) => {
                self.topic = topic;
                ControlFlow::Continue(())
            }
            Err(failure) => {
                (self.report)(NodeRecord::TopicFailed(failure));
                ControlFlow::Break(())
            }
        }
    }
}

/// The address less every relay found down.
fn without_relays(address: EndpointAddr, down: &BTreeSet<RelayUrl>) -> EndpointAddr {
    EndpointAddr::from_parts(
        address.id,
        address
            .addrs
            .into_iter()
            .filter(|path| !matches!(path, TransportAddr::Relay(relay) if down.contains(relay))),
    )
}

/// The instant a span after `at`, or `at` itself past the clock's range.
fn later_by(at: Instant, span: Duration) -> Instant {
    at.checked_add(span).unwrap_or(at)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use iroh::protocol::Router;
    use iroh::{EndpointAddr, EndpointId, RelayUrl};
    use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip};
    use n0_future::StreamExt;
    use tokio::task::JoinHandle;
    use tokio::time::Instant;

    use super::{MemberJoins, RebindSchedule, RelayProbe, relay_answers};
    use crate::net::direct::{ClientTimeouts, DirectClient, DirectHost, DirectServer};
    use crate::net::endpoint::{DIRECT_ALPN, NodeEndpoint};
    use crate::net::gossip::{GossipEvent, NetworkTopic};
    use crate::node::NodeRecord;
    use crate::node::presence::tests::{
        Draws, FIRST_HANDLE, FIRST_PUBLIC, FIRST_SEED, HOST_SEED, MARKER_SEED, Member, MemberPlan,
        OWNER_SEED, PAYLOAD_WAIT, Scratch, WITNESS_SEED, Witness, bind, card, frozen_clock, keys,
        secret_key, spawn_member, start_members, start_members_reaching, status_within,
        unreachable_about, wall_clock_ms,
    };
    use crate::payload::ZERO_MAC;
    use crate::peers::Status;
    use crate::wire::{Addr, Introduce, IntroduceAck, Message, Payload, Version};

    const SPEC_DEAF_TIMEOUT: Duration = Duration::from_secs(15);
    const SPEC_REBIND_INTERVAL: Duration = Duration::from_secs(20);
    const SPEC_JOIN_TIMEOUT: Duration = Duration::from_millis(5000);
    const SPEC_RELAY_PROBE_TIMEOUT: Duration = Duration::from_millis(1000);
    const IROH_DOCUMENTED_CLOSE_LATENCY: Duration = Duration::from_secs(3);
    const SPEC_MEMBER_JOIN_WAIT: Duration = Duration::from_millis(1500);
    const SPEC_MEMBER_JOIN_INTERVAL: Duration = Duration::from_millis(3000);
    const SPEC_MEMBER_JOIN_TRIES: u32 = 10;
    const SPEC_TEST_24_RUN: Duration = Duration::from_secs(180);
    const LONGEST_PAYLOAD_GAP_MS: u64 = 5000;
    const LONGEST_DRAWN_MS: u64 = 60_000;
    const PROPERTY_CASES: u64 = 500;
    const STEPS_PER_CASE: u64 = 40;
    const SCHEDULE_EVENT_KINDS: u64 = 3;
    const FROZEN_CLOCK_MS: u64 = 1_800_000_000_000;
    const LIVE_HOST_SEED: u8 = 93;
    const DEAD_HOST_SEED: u8 = 94;
    const PEER_SEED: u8 = 95;
    const PEER_HANDLE: &str = "pat";
    const PEER_PUBLIC: &[u8] = br#"{"role":"peer"}"#;
    const SENT_BODY: &str = "a send that reaches the peer";
    const LIVE_RELAY: &str = "https://relay-live.estafette.invalid/";
    const DEAD_RELAY: &str = "https://relay-dead.estafette.invalid/";
    const PEER_ONLY_RELAY: &str = "https://relay-peer.estafette.invalid/";
    const KEPT_RELAY: &str = "https://relay-answers-in-time.estafette.invalid/";
    const STRIPPED_RELAY: &str = "https://relay-answers-late.estafette.invalid/";
    const ANSWER_WITHIN_THE_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
    const ANSWER_AFTER_THE_PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
    const SPEC_INFRASTRUCTURE_RECORD_INTERVAL: Duration = Duration::from_secs(5);
    const LOOPBACK_ANY_PORT: &str = "127.0.0.1:0";
    const REBIND_SLACK: Duration = Duration::from_secs(2);
    const REBIND_POLL: Duration = Duration::from_millis(20);

    #[derive(Clone, Default)]
    struct RebindLog {
        started: Arc<Mutex<Vec<Instant>>>,
    }

    impl RebindLog {
        fn sink(&self) -> Arc<dyn Fn(NodeRecord) + Send + Sync> {
            let started = Arc::clone(&self.started);
            Arc::new(move |record| {
                if matches!(record, NodeRecord::Rebinding) {
                    started.lock().unwrap().push(Instant::now());
                }
            })
        }

        async fn nth_within(&self, index: usize, within: Duration) -> Option<Instant> {
            tokio::time::timeout(within, async {
                loop {
                    if let Some(at) = self.started.lock().unwrap().get(index) {
                        return *at;
                    }
                    tokio::time::sleep(REBIND_POLL).await;
                }
            })
            .await
            .ok()
        }
    }

    struct QuietNode {
        endpoint: NodeEndpoint,
        introduction: Addr,
        presences: Arc<Mutex<Vec<(Instant, u64)>>>,
        neighbors_up: Arc<Mutex<Vec<(Instant, EndpointId)>>>,
        _router: Router,
        _listening: JoinHandle<()>,
    }

    impl QuietNode {
        async fn start(seed: u8) -> Self {
            Self::serving(seed, None, None).await
        }

        async fn answering(seed: u8, relays: &[&str]) -> Self {
            Self::serving(seed, Some(relays), None).await
        }

        async fn broadcasting(seed: u8, every: Duration) -> Self {
            Self::serving(seed, None, Some(every)).await
        }

        async fn serving(
            seed: u8,
            introduced_with_relays: Option<&[&str]>,
            broadcast_every: Option<Duration>,
        ) -> Self {
            let endpoint = bind(seed).await;
            let introduction = Addr::from_endpoint_report(
                introduced_with_relays
                    .unwrap_or_default()
                    .iter()
                    .map(|relay| (*relay).to_owned()),
                endpoint.endpoint().addr().ip_addrs().copied(),
            );
            let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());
            let mut router =
                Router::builder(endpoint.endpoint().clone()).accept(GOSSIP_ALPN, gossip.clone());
            if introduced_with_relays.is_some() {
                router = router.accept(
                    DIRECT_ALPN,
                    DirectServer::new(
                        keys(),
                        AnsweringPeer {
                            addr: introduction.clone(),
                        },
                    ),
                );
            }
            let router = router.spawn();
            let mut topic = NetworkTopic::join(
                &gossip,
                keys(),
                secret_key(OWNER_SEED).public(),
                Vec::new(),
                Instant::now(),
            )
            .await
            .unwrap();
            let presences = Arc::new(Mutex::new(Vec::new()));
            let neighbors_up = Arc::new(Mutex::new(Vec::new()));
            let heard = Arc::clone(&presences);
            let met = Arc::clone(&neighbors_up);
            let mut broadcasts = broadcast_every.map(tokio::time::interval);
            let listening = tokio::spawn(async move {
                loop {
                    let event = tokio::select! {
                        event = topic.next_event() => event,
                        () = next_broadcast(&mut broadcasts) => {
                            drop(topic.broadcast(
                                &secret_key(seed),
                                unreachable_about(MARKER_SEED, seed, wall_clock_ms()),
                            ).await);
                            continue;
                        }
                    };
                    let Some(event) = event else {
                        break;
                    };
                    match event {
                        Ok(GossipEvent::Payload(payload)) => {
                            if let Payload::Presence(presence) = *payload {
                                heard.lock().unwrap().push((Instant::now(), presence.seq));
                            }
                        }
                        Ok(GossipEvent::NeighborUp(neighbor)) => {
                            met.lock().unwrap().push((Instant::now(), neighbor));
                        }
                        Ok(
                            GossipEvent::Joined
                            | GossipEvent::JoinTimedOut
                            | GossipEvent::NeighborDown(_),
                        )
                        | Err(_) => {}
                    }
                }
            });
            Self {
                endpoint,
                introduction,
                presences,
                neighbors_up,
                _router: router,
                _listening: listening,
            }
        }

        fn addr(&self) -> EndpointAddr {
            self.endpoint.endpoint().addr()
        }

        async fn introduce_to(&self, member: &iroh::Endpoint) {
            self.endpoint.replace_address(member.addr());
            DirectClient::new(
                self.endpoint.endpoint().clone(),
                keys(),
                ClientTimeouts::SPEC,
            )
            .introduce(member.id(), peer_introduction(self.introduction.clone()))
            .await
            .unwrap();
        }

        async fn first_neighbor_up_of(&self, seed: u8, within: Duration) -> Option<Instant> {
            let neighbor = secret_key(seed).public();
            tokio::time::timeout(within, async {
                loop {
                    let found = self
                        .neighbors_up
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|(_, up)| *up == neighbor)
                        .map(|(at, _)| *at);
                    if let Some(at) = found {
                        return at;
                    }
                    tokio::time::sleep(REBIND_POLL).await;
                }
            })
            .await
            .ok()
        }

        fn last_seq_before(&self, at: Instant) -> Option<u64> {
            self.presences
                .lock()
                .unwrap()
                .iter()
                .rfind(|(heard, _)| *heard < at)
                .map(|(_, seq)| *seq)
        }

        async fn first_seq_after(&self, at: Instant, within: Duration) -> Option<u64> {
            tokio::time::timeout(within, async {
                loop {
                    let found = self
                        .presences
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|(heard, _)| *heard > at)
                        .map(|(_, seq)| *seq);
                    if let Some(seq) = found {
                        return seq;
                    }
                    tokio::time::sleep(REBIND_POLL).await;
                }
            })
            .await
            .ok()
        }
    }

    struct AnsweringPeer {
        addr: Addr,
    }

    impl DirectHost for AnsweringPeer {
        fn is_stopping(&self) -> bool {
            false
        }

        fn own_card(&self) -> crate::wire::PublicKey {
            card(PEER_SEED)
        }

        fn is_attached(&self) -> bool {
            false
        }

        fn receive_message(&self, _sender: crate::wire::PublicKey, _message: Message) {}

        fn receive_introduce(
            &self,
            _sender: crate::wire::PublicKey,
            _introduce: Introduce,
        ) -> IntroduceAck {
            let introduction = peer_introduction(self.addr.clone());
            IntroduceAck {
                v: introduction.v,
                handle: introduction.handle,
                public: introduction.public,
                attached: introduction.attached,
                addr: introduction.addr,
                mac: introduction.mac,
            }
        }
    }

    async fn next_broadcast(broadcasts: &mut Option<tokio::time::Interval>) {
        match broadcasts {
            Some(interval) => drop(interval.tick().await),
            None => std::future::pending().await,
        }
    }

    fn peer_introduction(addr: Addr) -> Introduce {
        Introduce {
            v: Version::CURRENT,
            handle: Some(PEER_HANDLE.to_owned()),
            public: PEER_PUBLIC.to_vec(),
            attached: false,
            addr,
            mac: ZERO_MAC,
        }
    }

    async fn lookup_entry(endpoint: &iroh::Endpoint, seed: u8) -> Option<EndpointAddr> {
        let lookup = endpoint.address_lookup().ok()?;
        match lookup.resolve(secret_key(seed).public()).next().await {
            Some(Ok(Ok(item))) => Some(item.into_endpoint_addr()),
            _ => None,
        }
    }

    fn relays_of(address: &EndpointAddr) -> Vec<String> {
        address.relay_urls().map(ToString::to_string).collect()
    }

    async fn entry_within(
        member: &Member,
        seed: u8,
        wanted: impl Fn(&EndpointAddr) -> bool + Send + Sync,
        within: Duration,
    ) -> Option<EndpointAddr> {
        tokio::time::timeout(within, async {
            loop {
                if let Some(entry) = lookup_entry(&member.node.client.endpoint(), seed).await
                    && wanted(&entry)
                {
                    return entry;
                }
                tokio::time::sleep(REBIND_POLL).await;
            }
        })
        .await
        .ok()
    }

    fn answering_only(live: &'static str) -> RelayProbe {
        Arc::new(move |relay: RelayUrl| {
            let answers = relay.to_string() == live;
            Box::pin(async move {
                if !answers {
                    std::future::pending::<()>().await;
                }
                answers
            })
        })
    }

    fn answering_after(delays: &'static [(&'static str, Duration)]) -> RelayProbe {
        Arc::new(move |relay: RelayUrl| {
            let delay = delays
                .iter()
                .find(|(answering, _)| relay.to_string() == *answering)
                .map(|(_, delay)| *delay);
            Box::pin(async move {
                match delay {
                    Some(delay) => {
                        tokio::time::sleep(delay).await;
                        true
                    }
                    None => false,
                }
            })
        })
    }

    #[tokio::test]
    async fn spec_3_15_test_24_logic_a_member_whose_host_broadcasts_every_5_s_records_no_rebind_past_the_deaf_timeout()
     {
        let scratch = Scratch::new("hearing").await;
        let host = QuietNode::broadcasting(HOST_SEED, SPEC_INFRASTRUCTURE_RECORD_INTERVAL).await;
        let rebinds = RebindLog::default();

        let _member = start_members_reaching(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_hosts(&[HOST_SEED])
                    .with_report(rebinds.sink()),
            ],
            &[(HOST_SEED, host.addr())],
        )
        .await;

        assert_eq!(
            rebinds
                .nth_within(0, SPEC_DEAF_TIMEOUT + REBIND_SLACK)
                .await,
            None
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_14_a_relay_answering_within_the_1000_ms_probe_timeout_is_kept_and_one_answering_after_it_is_stripped()
     {
        let scratch = Scratch::new("probe-timeout").await;
        let rebinds = RebindLog::default();
        let peer = QuietNode::answering(PEER_SEED, &[KEPT_RELAY, STRIPPED_RELAY]).await;
        let (member_endpoint, starting) = spawn_member(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_host_relays(&[
                    (LIVE_HOST_SEED, KEPT_RELAY),
                    (DEAD_HOST_SEED, STRIPPED_RELAY),
                ])
                .with_relay_probe(answering_after(&[
                    (KEPT_RELAY, ANSWER_WITHIN_THE_PROBE_TIMEOUT),
                    (STRIPPED_RELAY, ANSWER_AFTER_THE_PROBE_TIMEOUT),
                ]))
                .with_report(rebinds.sink()),
        )
        .await;
        peer.introduce_to(&member_endpoint).await;
        let member = starting.await.unwrap();

        let rebound = rebinds
            .nth_within(0, SPEC_JOIN_TIMEOUT + REBIND_SLACK)
            .await;
        let stripped = entry_within(
            &member,
            PEER_SEED,
            |entry| !relays_of(entry).contains(&STRIPPED_RELAY.to_owned()),
            IROH_DOCUMENTED_CLOSE_LATENCY + ANSWER_AFTER_THE_PROBE_TIMEOUT + REBIND_SLACK,
        )
        .await;

        assert!(rebound.is_some());
        assert_eq!(
            stripped.as_ref().map(relays_of),
            Some(vec![KEPT_RELAY.to_owned()])
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn d46_a_member_silent_for_the_deaf_timeout_rebinds_once_and_not_again_within_the_rebind_interval()
     {
        let scratch = Scratch::new("deaf").await;
        let host = QuietNode::start(HOST_SEED).await;
        let rebinds = RebindLog::default();
        let started = Instant::now();

        let _member = start_members_reaching(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_hosts(&[HOST_SEED])
                    .with_report(rebinds.sink()),
            ],
            &[(HOST_SEED, host.addr())],
        )
        .await;
        let first = rebinds
            .nth_within(0, SPEC_DEAF_TIMEOUT + REBIND_SLACK)
            .await
            .unwrap();
        let second = rebinds
            .nth_within(1, SPEC_REBIND_INTERVAL + REBIND_SLACK)
            .await
            .unwrap();

        assert!(first.duration_since(started) >= SPEC_DEAF_TIMEOUT);
        assert!(second.duration_since(first) >= SPEC_REBIND_INTERVAL);
        assert!(second.duration_since(first) < SPEC_REBIND_INTERVAL + REBIND_SLACK);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_after_a_rebind_the_member_rejoins_through_its_known_host_and_announces_the_next_seq()
     {
        let scratch = Scratch::new("rejoin").await;
        let host = QuietNode::start(HOST_SEED).await;
        let rebinds = RebindLog::default();

        let _member = start_members_reaching(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_hosts(&[HOST_SEED])
                    .with_clock(frozen_clock(FROZEN_CLOCK_MS))
                    .with_report(rebinds.sink()),
            ],
            &[(HOST_SEED, host.addr())],
        )
        .await;
        let rebound = rebinds
            .nth_within(0, SPEC_DEAF_TIMEOUT + REBIND_SLACK)
            .await
            .unwrap();
        let announced_after = host.first_seq_after(rebound, SPEC_JOIN_TIMEOUT).await;

        assert_eq!(host.last_seq_before(rebound), Some(FROZEN_CLOCK_MS));
        assert_eq!(announced_after, Some(FROZEN_CLOCK_MS + 1));
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_member_that_knows_no_other_node_never_rebinds_on_silence_or_at_its_join_timeout()
     {
        let scratch = Scratch::new("alone").await;
        let rebinds = RebindLog::default();

        let _member = start_members(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(rebinds.sink()),
            ],
        )
        .await;

        assert_eq!(
            rebinds
                .nth_within(0, SPEC_DEAF_TIMEOUT + REBIND_SLACK)
                .await,
            None
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn d46_an_online_peer_is_another_node_so_a_join_without_a_neighbor_ends_in_a_rebind_that_joins_through_it()
     {
        let scratch = Scratch::new("online").await;
        let rebinds = RebindLog::default();
        let started = Instant::now();
        let peer = QuietNode::answering(PEER_SEED, &[]).await;

        let (member_endpoint, starting) = spawn_member(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(rebinds.sink()),
        )
        .await;
        peer.introduce_to(&member_endpoint).await;
        let _member = starting.await.unwrap();
        let rebound = rebinds
            .nth_within(0, SPEC_JOIN_TIMEOUT + REBIND_SLACK)
            .await
            .unwrap();
        let joined_through_the_peer = peer
            .first_neighbor_up_of(FIRST_SEED, IROH_DOCUMENTED_CLOSE_LATENCY + REBIND_SLACK)
            .await
            .unwrap();

        assert!(rebound.duration_since(started) >= SPEC_JOIN_TIMEOUT);
        assert!(joined_through_the_peer > rebound);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_rebind_strips_the_relays_found_down_from_other_members_entries_only_and_keeps_the_table()
     {
        let scratch = Scratch::new("strip").await;
        let rebinds = RebindLog::default();
        let peer =
            QuietNode::answering(PEER_SEED, &[LIVE_RELAY, DEAD_RELAY, PEER_ONLY_RELAY]).await;
        let (member_endpoint, starting) = spawn_member(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_host_relays(&[(LIVE_HOST_SEED, LIVE_RELAY), (DEAD_HOST_SEED, DEAD_RELAY)])
                .with_relay_probe(answering_only(LIVE_RELAY))
                .with_report(rebinds.sink()),
        )
        .await;
        peer.introduce_to(&member_endpoint).await;
        let held_before = lookup_entry(&member_endpoint, PEER_SEED).await.unwrap();
        let member = starting.await.unwrap();

        let rebound = rebinds
            .nth_within(0, SPEC_JOIN_TIMEOUT + REBIND_SLACK)
            .await;
        let stripped = entry_within(
            &member,
            PEER_SEED,
            |entry| !relays_of(entry).contains(&DEAD_RELAY.to_owned()),
            IROH_DOCUMENTED_CLOSE_LATENCY + SPEC_RELAY_PROBE_TIMEOUT + REBIND_SLACK,
        )
        .await
        .unwrap();
        let dead_host = lookup_entry(&member.node.client.endpoint(), DEAD_HOST_SEED)
            .await
            .unwrap();

        assert!(rebound.is_some());
        assert_eq!(
            relays_of(&held_before),
            vec![DEAD_RELAY, LIVE_RELAY, PEER_ONLY_RELAY]
        );
        assert_eq!(relays_of(&stripped), vec![LIVE_RELAY, PEER_ONLY_RELAY]);
        assert_eq!(
            stripped.ip_addrs().collect::<Vec<_>>(),
            held_before.ip_addrs().collect::<Vec<_>>()
        );
        assert_eq!(relays_of(&dead_host), vec![DEAD_RELAY]);
        assert!(
            member
                .node
                .peers()
                .iter()
                .any(|listed| listed.card == card(PEER_SEED))
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_after_a_rebinds_join_without_a_member_neighbor_the_member_asks_to_join_an_unreachable_member()
     {
        let scratch = Scratch::new("member-joins").await;
        let host = QuietNode::start(HOST_SEED).await;
        let unreachable_member = QuietNode::start(PEER_SEED).await;
        let rebinds = RebindLog::default();

        let member = start_members_reaching(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_hosts(&[HOST_SEED])
                    .with_report(rebinds.sink()),
            ],
            &[(HOST_SEED, host.addr())],
        )
        .await
        .pop()
        .unwrap();
        unreachable_member.introduce_to(&member.endpoint).await;
        let rebound = rebinds
            .nth_within(0, SPEC_DEAF_TIMEOUT + REBIND_SLACK)
            .await
            .unwrap();
        let met = unreachable_member
            .first_neighbor_up_of(
                FIRST_SEED,
                IROH_DOCUMENTED_CLOSE_LATENCY
                    + SPEC_MEMBER_JOIN_WAIT
                    + SPEC_MEMBER_JOIN_INTERVAL
                    + REBIND_SLACK,
            )
            .await
            .unwrap();

        assert!(met.duration_since(rebound) >= SPEC_MEMBER_JOIN_WAIT);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_send_that_turns_its_unreachable_peer_online_lets_a_long_deaf_member_rebind_at_once()
     {
        let scratch = Scratch::new("reached").await;
        let rebinds = RebindLog::default();
        let member = start_members(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(rebinds.sink()),
            ],
        )
        .await
        .pop()
        .unwrap();
        let peer = QuietNode::answering(PEER_SEED, &[]).await;
        peer.introduce_to(&member.endpoint).await;
        let witness = Witness::joined_through(WITNESS_SEED, None, &member.endpoint).await;
        witness
            .broadcast_as(
                WITNESS_SEED,
                unreachable_about(PEER_SEED, WITNESS_SEED, wall_clock_ms()),
            )
            .await;
        status_within(
            &member,
            PEER_SEED,
            |status| status == Status::Unreachable,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap();
        let rebound_while_no_peer_was_online = rebinds
            .nth_within(0, SPEC_DEAF_TIMEOUT + REBIND_SLACK)
            .await;

        member.node.send(PEER_HANDLE, SENT_BODY).await.unwrap();
        let reached = Instant::now();
        let rebound = rebinds.nth_within(0, REBIND_SLACK).await;

        assert_eq!(rebound_while_no_peer_was_online, None);
        assert!(rebound.unwrap().duration_since(reached) < REBIND_SLACK);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_the_relay_probe_answers_for_a_listening_relay_and_not_for_a_closed_port() {
        let listening = tokio::net::TcpListener::bind(LOOPBACK_ANY_PORT)
            .await
            .unwrap();
        let listening_relay = relay_on(listening.local_addr().unwrap().port());
        let closed_port = tokio::net::TcpListener::bind(LOOPBACK_ANY_PORT)
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        assert!(relay_answers(listening_relay).await);
        assert!(!relay_answers(relay_on(closed_port)).await);
    }

    fn relay_on(port: u16) -> RelayUrl {
        format!("http://127.0.0.1:{port}/").parse().unwrap()
    }

    #[test]
    fn spec_3_15_test_24_logic_a_member_hearing_a_payload_at_least_every_5_s_never_rebinds_in_180_s()
     {
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let started = Instant::now();
            let mut schedule = RebindSchedule::new(started);
            schedule.joined();
            let mut now = started;
            while now.duration_since(started) < SPEC_TEST_24_RUN {
                let next_payload =
                    now + Duration::from_millis(1 + draws.below(LONGEST_PAYLOAD_GAP_MS));
                assert!(schedule.due() > next_payload, "case {case}");
                schedule.heard(next_payload);
                now = next_payload;
            }
        }
    }

    #[test]
    fn d46_silence_of_the_deaf_timeout_is_due_then_and_no_rebind_comes_within_20_s_of_the_last() {
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let started = Instant::now();
            let mut schedule = RebindSchedule::new(started);
            let heard = started + Duration::from_millis(draws.below(LONGEST_DRAWN_MS));
            schedule.heard(heard);
            assert_eq!(schedule.due(), heard + SPEC_DEAF_TIMEOUT, "case {case}");

            let rebound = heard + Duration::from_millis(draws.below(LONGEST_DRAWN_MS));
            schedule.rebound(rebound);
            for _ in 0..STEPS_PER_CASE {
                let at = rebound + Duration::from_millis(draws.below(LONGEST_DRAWN_MS));
                match draws.below(SCHEDULE_EVENT_KINDS) {
                    0 => schedule.heard(at),
                    1 => schedule.join_timed_out(at),
                    _ => schedule.joined(),
                }
                assert!(
                    schedule.due() >= rebound + SPEC_REBIND_INTERVAL,
                    "case {case}"
                );
            }
        }
    }

    #[test]
    fn spec_3_8_a_join_without_a_neighbor_is_due_at_once_and_after_a_rebind_no_sooner_than_the_interval()
     {
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let started = Instant::now();
            let mut schedule = RebindSchedule::new(started);
            let timed_out = started + Duration::from_millis(draws.below(LONGEST_DRAWN_MS));
            schedule.join_timed_out(timed_out);
            assert_eq!(
                schedule.due(),
                timed_out.min(started + SPEC_DEAF_TIMEOUT),
                "case {case}"
            );

            schedule.rebound(timed_out);
            let timed_out_again = timed_out + SPEC_JOIN_TIMEOUT;
            schedule.join_timed_out(timed_out_again);
            assert_eq!(
                schedule.due(),
                timed_out + SPEC_REBIND_INTERVAL,
                "case {case}"
            );
        }
    }

    #[test]
    fn spec_3_8_member_joins_start_1_5_s_after_a_rebinds_join_repeat_every_3_s_and_stop_after_10() {
        let joined = Instant::now();
        let mut joins = MemberJoins::AwaitingRebindJoin;
        joins.joined(joined);
        let mut asks = Vec::new();
        while let Some(due) = joins.next_ask() {
            asks.push(due);
            joins.asked(due);
        }

        let expected: Vec<Instant> = (0..SPEC_MEMBER_JOIN_TRIES)
            .map(|ask| joined + SPEC_MEMBER_JOIN_WAIT + SPEC_MEMBER_JOIN_INTERVAL * ask)
            .collect();
        assert_eq!(asks, expected);
    }

    #[test]
    fn spec_3_8_the_first_join_starts_no_member_joins() {
        let mut joins = MemberJoins::Idle;
        joins.joined(Instant::now());

        assert_eq!(joins.next_ask(), None);
    }
}
