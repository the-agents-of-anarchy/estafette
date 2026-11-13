//! The rebind of spec 3.8 and D46: when a member falls deaf or its join gains no neighbor, the rebind itself, and the member join loop after it.

use std::collections::BTreeSet;
use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::stream_items::join_all;
use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};

use crate::event::{GossipEvent, TransportEvent};
use crate::net::direct::DirectHost;
use crate::net::endpoint::{RebindCause, SnapshotMoment};
use crate::net::gossip::NetworkTopic;
use crate::node::addresses::endpoint_id;
use crate::node::known_hosts::seed_lookup_with_known_hosts;
use crate::node::presence::{PresenceLoop, home_relay_changes, own_address_changes, serve};
use crate::node::relay_loss::RelayLossWatch;
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
struct RebindSchedule {
    silent_since: Instant,
    unjoined_since: Option<Instant>,
    last_rebind: Option<Instant>,
}

impl RebindSchedule {
    /// The schedule of a member that has heard nothing since it subscribed.
    const fn new(subscribed: Instant) -> Self {
        Self {
            silent_since: subscribed,
            unjoined_since: None,
            last_rebind: None,
        }
    }

    /// Notes a gossip payload heard.
    const fn heard(&mut self, now: Instant) {
        self.silent_since = now;
    }

    /// Notes that the topic's join gained no neighbor within the join timeout.
    const fn join_timed_out(&mut self, now: Instant) {
        self.unjoined_since = Some(now);
    }

    /// Notes that the topic reported joined.
    const fn joined(&mut self) {
        self.unjoined_since = None;
    }

    /// Notes a rebind, whose fresh topic has not joined yet.
    const fn rebound(&mut self, now: Instant) {
        self.last_rebind = Some(now);
        self.unjoined_since = None;
    }

    /// When the next rebind is due, for a member that may rebind.
    #[cfg(test)]
    fn due(&self) -> Instant {
        self.next_rebind().0
    }

    /// When the next rebind is due and why: after the deaf timeout of silence or at a join that gained no neighbor, never sooner than the rebind interval after the last rebind.
    fn next_rebind(&self) -> (Instant, RebindCause) {
        let deaf_at = later_by(self.silent_since, DEAF_TIMEOUT);
        let (triggered_at, cause) = match self.unjoined_since {
            Some(unjoined) if unjoined < deaf_at => (unjoined, RebindCause::JoinGainedNoNeighbor),
            Some(_) | None => (deaf_at, RebindCause::Deaf),
        };
        let due = self.last_rebind.map_or(triggered_at, |last| {
            triggered_at.max(later_by(last, REBIND_INTERVAL))
        });
        (due, cause)
    }
}

/// The member join loop of spec 3.8 after a rebind: when the next ask to join the members is due, and how many asks are left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberJoins {
    Idle,
    AwaitingRebindJoin,
    Asking { next: Instant, asks_left: u32 },
}

impl MemberJoins {
    /// Starts the member join wait when the topic of a rebind reports joined.
    fn joined(&mut self, now: Instant) {
        if *self == Self::AwaitingRebindJoin {
            *self = Self::Asking {
                next: later_by(now, MEMBER_JOIN_WAIT),
                asks_left: MEMBER_JOIN_TRIES,
            };
        }
    }

    /// When the next ask is due, if one is.
    const fn next_ask(&self) -> Option<Instant> {
        match self {
            Self::Asking { next, .. } => Some(*next),
            Self::Idle | Self::AwaitingRebindJoin => None,
        }
    }

    /// Counts an ask and schedules the next one the member join interval later, or stops after the member join tries.
    fn asked(&mut self, now: Instant) {
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

/// What the rebind of spec 3.8 keeps between wakes: when it is due, the member join loop after it, the gossip neighbors and the relay probe.
pub(super) struct Rebinding {
    schedule: RebindSchedule,
    member_joins: MemberJoins,
    neighbors: BTreeSet<EndpointId>,
    probe_relay: RelayProbe,
}

impl Rebinding {
    /// The rebind state of a member that has heard nothing since it subscribed.
    pub(super) const fn new(subscribed: Instant, probe_relay: RelayProbe) -> Self {
        Self {
            schedule: RebindSchedule::new(subscribed),
            member_joins: MemberJoins::Idle,
            neighbors: BTreeSet::new(),
            probe_relay,
        }
    }

    /// Notes a gossip payload heard.
    pub(super) const fn heard(&mut self, now: Instant) {
        self.schedule.heard(now);
    }

    /// Notes a gossip neighbor that came up.
    pub(super) fn neighbor_up(&mut self, node: EndpointId) {
        self.neighbors.insert(node);
    }

    /// Notes a gossip neighbor that went down.
    pub(super) fn neighbor_down(&mut self, node: EndpointId) {
        self.neighbors.remove(&node);
    }

    /// Notes that the topic reported joined, starting the member join wait when the join was a rebind's.
    pub(super) fn joined(&mut self, now: Instant) {
        self.schedule.joined();
        self.member_joins.joined(now);
    }

    /// Notes that the topic's join gained no neighbor within the join timeout.
    pub(super) const fn join_timed_out(&mut self, now: Instant) {
        self.schedule.join_timed_out(now);
    }

    /// When the next rebind is due and why, for a member that may rebind.
    pub(super) fn next_rebind(&self) -> (Instant, RebindCause) {
        self.schedule.next_rebind()
    }

    /// When the next ask to join the members is due, if one is.
    pub(super) const fn next_ask(&self) -> Option<Instant> {
        self.member_joins.next_ask()
    }

    /// Notes a rebind: the next one waits the rebind interval, the fresh topic's join is awaited, and no neighbor is held.
    pub(super) fn rebound(&mut self, now: Instant) {
        self.schedule.rebound(now);
        self.member_joins = MemberJoins::AwaitingRebindJoin;
        self.neighbors.clear();
    }

    /// Whether any of the members is a gossip neighbor.
    pub(super) fn has_neighbor_among(&self, members: &[EndpointId]) -> bool {
        members.iter().any(|member| self.neighbors.contains(member))
    }

    /// Stops the asks to join the members.
    pub(super) const fn stop_asking(&mut self) {
        self.member_joins = MemberJoins::Idle;
    }

    /// Counts an ask to join the members made at `now`.
    pub(super) fn asked(&mut self, now: Instant) {
        self.member_joins.asked(now);
    }

    /// Probes whether the relay answers.
    pub(super) fn probe(&self, relay: RelayUrl) -> impl Future<Output = bool> + Send {
        (self.probe_relay)(relay)
    }
}

impl PresenceLoop {
    /// Whether this member may rebind: it knows another node, a known host or an Online peer, and is not stopping.
    pub(super) fn may_rebind(&self) -> bool {
        !self.state.is_stopping()
            && (!self.known_hosts.is_empty() || !self.peers_with_status(Status::Online).is_empty())
    }

    /// Rebinds by spec 3.8, stopping the loop when no endpoint or topic comes of it.
    pub(super) async fn rebind(&mut self, cause: RebindCause) -> ControlFlow<()> {
        (self.report)(TransportEvent::Rebinding(cause).into());
        self.record_state(SnapshotMoment::Rebind);
        self.rebinding.rebound((self.monotonic)());
        self.close_endpoint().await;
        let down = self.relays_found_down().await;
        self.strip_relays_from_members(&down);
        if let Err(failure) = self.endpoint.rebind().await {
            (self.report)(TransportEvent::RebindFailed(failure).into());
            return ControlFlow::Break(());
        }
        self.client.follow(self.endpoint.endpoint().clone()).await;
        self.subscribe_through_known_nodes().await
    }

    /// Asks the gossip layer to join the Online and Unreachable members, stopping the asks when there are none or one of them is a gossip neighbor already.
    pub(super) async fn ask_members_to_join(&mut self) {
        let mut members = self.peers_with_status(Status::Online);
        members.extend(self.peers_with_status(Status::Unreachable));
        if members.is_empty() || self.rebinding.has_neighbor_among(&members) {
            self.rebinding.stop_asking();
            return;
        }
        let now = (self.monotonic)();
        if let Err(failure) =
            NetworkTopic::join(&self.gossip, self.keys.clone(), self.owner, members, now).await
        {
            (self.report)(GossipEvent::TopicFailed(failure).into());
        }
        self.rebinding.asked(now);
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
            (self.report)(TransportEvent::RouterShutdownFailed(failure).into());
        }
    }

    /// The relays of the known hosts that do not answer the probe within the relay probe timeout, recording each probe's answer.
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
                .map(|relay| timeout_at(deadline, self.rebinding.probe(relay.clone()))),
        )
        .await;
        let mut down = BTreeSet::new();
        for (relay, answer) in relays.into_iter().zip(answers) {
            let answered = matches!(answer, Ok(true));
            self.endpoint
                .record(|transport| TransportEvent::RelayProbed {
                    location: transport.locate(&TransportAddr::Relay(relay.clone())),
                    answered,
                });
            if !answered {
                down.insert(relay);
            }
        }
        down
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
                (self.report)(GossipEvent::TopicFailed(failure).into());
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
    use std::sync::Arc;
    use std::time::Duration;

    use crate::stream_items::StreamExt;
    use iroh::protocol::Router;
    use iroh::{EndpointAddr, EndpointId, RelayUrl};
    use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip};
    use tokio::sync::watch;
    use tokio::task::JoinHandle;
    use tokio::time::Instant;

    use super::{MemberJoins, RebindSchedule, RelayProbe, relay_answers};
    use crate::event::{Event, RunRecord, TransportEvent};
    use crate::net::direct::tests::{next_recorded, recorded_events, test_salt};
    use crate::net::direct::{
        ClientTimeouts, DirectClient, DirectHost, DirectServer, MonotonicClock,
    };
    use crate::net::endpoint::{
        DIRECT_ALPN, LocationHash, NodeEndpoint, RebindCause, TransportRecord,
    };
    use crate::net::gossip::{GossipEvent, NetworkTopic};
    use crate::node::presence::acknowledgement_of;
    use crate::node::presence::tests::{
        FIRST_HANDLE, FIRST_PUBLIC, FIRST_SEED, HOST_SEED, MARKER_SEED, Member, MemberPlan,
        OWNER_SEED, PAYLOAD_WAIT, SECOND_HANDLE, SECOND_PUBLIC, SECOND_SEED, Scratch, WITNESS_SEED,
        Witness, bind, card, entry_meeting_within, frozen_clock, keys, presence_from,
        presence_request_by, relay_url, secret_key, spawn_member, start_members,
        start_members_reaching, status_within, unreachable_about, wall_clock_ms,
    };
    use crate::payload::ZERO_MAC;
    use crate::peers::Status;
    use crate::seeded_draws::Draws;
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
    const LONGEST_ASK_LATENESS_MS: u64 = 700;
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
    const ATTACHED_ONLY_IN_THE_PEERS_ACK: bool = true;
    const ANSWERING_MEMBER_DEAF_LATER_BY: Duration = Duration::from_secs(20);
    const SPEC_REBINDS_JOIN_WITHIN: Duration = SPEC_DEAF_TIMEOUT
        .saturating_add(IROH_DOCUMENTED_CLOSE_LATENCY)
        .saturating_add(SPEC_RELAY_PROBE_TIMEOUT)
        .saturating_add(REBIND_SLACK);

    /// What a test heard, in order, waking each waiter at every new entry.
    struct Heard<T>(Arc<watch::Sender<Vec<T>>>);

    impl<T> Clone for Heard<T> {
        fn clone(&self) -> Self {
            Self(Arc::clone(&self.0))
        }
    }

    impl<T> Default for Heard<T> {
        fn default() -> Self {
            Self(Arc::new(watch::Sender::new(Vec::new())))
        }
    }

    impl<T: Send + Sync> Heard<T> {
        /// Records the entry after every earlier one and wakes each waiter.
        fn push(&self, entry: T) {
            self.0.send_modify(|entries| entries.push(entry));
        }

        /// Every entry heard so far, in order.
        fn entries(&self) -> watch::Ref<'_, Vec<T>> {
            self.0.borrow()
        }

        /// What `found` first finds in the entries heard within `within`, none when it finds nothing by then.
        async fn first_within<Found: Send>(
            &self,
            within: Duration,
            found: impl Fn(&[T]) -> Option<Found> + Send + Sync,
        ) -> Option<Found> {
            let mut entries = self.0.subscribe();
            let entries =
                tokio::time::timeout(within, entries.wait_for(|entries| found(entries).is_some()))
                    .await
                    .ok()?
                    .ok()?;
            found(&entries)
        }
    }

    #[derive(Clone, Default)]
    struct RebindLog {
        started: Heard<Instant>,
    }

    impl RebindLog {
        /// A run record that notes the instant of each rebind it is given.
        fn sink(&self) -> RunRecord {
            let started = self.started.clone();
            Arc::new(move |event| {
                if matches!(event, Event::Transport(TransportEvent::Rebinding(_))) {
                    started.push(Instant::now());
                }
            })
        }

        async fn nth_within(&self, index: usize, within: Duration) -> Option<Instant> {
            self.started
                .first_within(within, |started| started.get(index).copied())
                .await
        }
    }

    struct QuietNode {
        endpoint: NodeEndpoint,
        introduction: Addr,
        presences: Heard<(Instant, u64)>,
        neighbors_up: Heard<(Instant, EndpointId)>,
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
                        &endpoint,
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
            let presences = Heard::default();
            let neighbors_up = Heard::default();
            let heard = presences.clone();
            let met = neighbors_up.clone();
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
                            if let Payload::Presence(presence) = Payload::from(*payload) {
                                heard.push((Instant::now(), presence.seq));
                            }
                        }
                        Ok(GossipEvent::NeighborUp(neighbor)) => {
                            met.push((Instant::now(), neighbor));
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
            self.neighbors_up
                .first_within(within, |ups| {
                    ups.iter()
                        .find(|(_, up)| *up == neighbor)
                        .map(|(at, _)| *at)
                })
                .await
        }

        fn last_seq_before(&self, at: Instant) -> Option<u64> {
            self.presences
                .entries()
                .iter()
                .rfind(|(heard, _)| *heard < at)
                .map(|(_, seq)| *seq)
        }

        async fn first_seq_after(&self, at: Instant, within: Duration) -> Option<u64> {
            self.presences
                .first_within(within, |presences| {
                    presences
                        .iter()
                        .find(|(heard, _)| *heard > at)
                        .map(|(_, seq)| *seq)
                })
                .await
        }
    }

    struct AnsweringPeer {
        addr: Addr,
    }

    impl DirectHost for AnsweringPeer {
        fn is_stopping(&self) -> bool {
            false
        }

        fn own_card(&self) -> Option<crate::wire::PublicKey> {
            Some(card(PEER_SEED))
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
            IntroduceAck {
                attached: ATTACHED_ONLY_IN_THE_PEERS_ACK,
                ..acknowledgement_of(peer_introduction(self.addr.clone()))
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
    async fn observability_decision_9_a_rebind_records_its_cause_the_old_endpoints_counters_before_it_closes_and_each_relay_probes_answer_by_port_and_salted_hash()
     {
        let scratch = Scratch::new("probe-record").await;
        let (record, mut events) = recorded_events();
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
                .with_report(Arc::clone(&record))
                .with_register(
                    crate::net::direct::HeldConnections::new(Arc::new(Instant::now))
                        .recording_to(TransportRecord::new(record, test_salt())),
                ),
        )
        .await;
        peer.introduce_to(&member_endpoint).await;
        let _member = starting.await.unwrap();

        let rebinding = next_recorded(&mut events, "transport.rebinding").await;
        let (before_the_snapshot, at_the_rebind) =
            recorded_until(&mut events, "state.counters").await;
        assert!(
            !before_the_snapshot
                .iter()
                .any(|name| SHOWS_THE_OLD_ENDPOINT_CLOSED.contains(name)),
            "the counters are read before the old endpoint closes: {before_the_snapshot:?}"
        );
        let first_probe = next_recorded(&mut events, "transport.relay_probed").await;
        let second_probe = next_recorded(&mut events, "transport.relay_probed").await;

        assert_eq!(
            rebinding,
            vec![("cause", serde_json::Value::from("join_gained_no_neighbor"))]
        );
        let probe_of = |relay: &str, answered: bool| {
            let relay = relay_url(relay);
            vec![
                ("kind", serde_json::Value::from("relay")),
                (
                    "port",
                    serde_json::Value::from(relay.port_or_known_default().unwrap()),
                ),
                (
                    "location",
                    serde_json::Value::from(LocationHash::of(&test_salt(), relay.as_str()).hex()),
                ),
                ("answered", serde_json::Value::from(answered)),
            ]
        };
        assert_eq!(
            at_the_rebind.first(),
            Some(&("moment", serde_json::Value::from("rebind")))
        );
        assert!(
            at_the_rebind
                .iter()
                .any(|(name, count)| *name == "socket.recv_datagrams"
                    && count.as_u64().is_some_and(|count| count > 0)),
            "the old endpoint's datagrams are counted: {at_the_rebind:?}"
        );
        let mut probes = vec![first_probe, second_probe];
        probes.sort_by_key(|fields| fields.last().map(|(_, answered)| answered.to_string()));
        assert_eq!(
            probes,
            vec![probe_of(STRIPPED_RELAY, false), probe_of(KEPT_RELAY, true)]
        );
        drop(scratch);
    }

    /// The names of the events recorded before the next one of the name, within the bound, and that event's fields.
    async fn recorded_until(
        events: &mut tokio::sync::mpsc::UnboundedReceiver<crate::net::direct::tests::RecordedEvent>,
        name: &str,
    ) -> (Vec<&'static str>, crate::event::EventFields) {
        let mut before = Vec::new();
        let found = tokio::time::timeout(RECORDED_UNTIL_WITHIN, async {
            while let Some((recorded, fields)) = events.recv().await {
                if recorded == name {
                    return Some(fields);
                }
                before.push(recorded);
            }
            None
        })
        .await;
        match found {
            Ok(Some(fields)) => (before, fields),
            Ok(None) | Err(_) => {
                panic!("no {name} within {RECORDED_UNTIL_WITHIN:?}; seen {before:?}")
            }
        }
    }

    const RECORDED_UNTIL_WITHIN: Duration = Duration::from_secs(10);
    /// The events a rebind records only once its old endpoint has closed: that endpoint's connections and paths ending, then the relay probes.
    const SHOWS_THE_OLD_ENDPOINT_CLOSED: [&str; 3] = [
        "transport.connection_closed",
        "transport.path_closed",
        "transport.relay_probed",
    ];
    const SPEC_STATE_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(60);
    const SNAPSHOT_WAIT: Duration = Duration::from_secs(61);

    type StampedEvent = (Instant, &'static str, crate::event::EventFields);

    /// A run record that hands each event's name and fields to the receiver, stamped with the instant it was recorded.
    fn stamped_events() -> (
        RunRecord,
        tokio::sync::mpsc::UnboundedReceiver<StampedEvent>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let record: RunRecord = Arc::new(move |event: Event| {
            drop(sender.send((Instant::now(), event.name(), event.fields())));
        });
        (record, receiver)
    }

    /// The instant, on the paused test clock, of the next counters snapshot recorded, with its fields.
    async fn next_snapshot(
        events: &mut tokio::sync::mpsc::UnboundedReceiver<StampedEvent>,
    ) -> (Instant, crate::event::EventFields) {
        let found = tokio::time::timeout(SNAPSHOT_WAIT, async {
            loop {
                let (at, name, fields) = events.recv().await.unwrap();
                if name == "state.counters" {
                    return (at, fields);
                }
            }
        })
        .await;
        found.unwrap_or_else(|_elapsed| panic!("no state.counters within {SNAPSHOT_WAIT:?}"))
    }

    #[tokio::test(start_paused = true)]
    async fn observability_decision_9_a_quiet_member_records_its_socket_and_gossip_counters_at_start_and_every_60_s_on_the_clock_it_waits_on()
     {
        let scratch = Scratch::new("state-interval").await;
        let (record, mut events) = stamped_events();
        let _members = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(record)],
        )
        .await;

        let (started, at_start) = next_snapshot(&mut events).await;
        let (first_interval, at_first_interval) = next_snapshot(&mut events).await;
        let (second_interval, _) = next_snapshot(&mut events).await;

        assert_eq!(
            at_start.first(),
            Some(&("moment", serde_json::Value::from("start")))
        );
        assert_eq!(
            at_first_interval.first(),
            Some(&("moment", serde_json::Value::from("interval")))
        );
        assert_eq!(first_interval - started, SPEC_STATE_SNAPSHOT_INTERVAL);
        assert_eq!(
            second_interval - first_interval,
            SPEC_STATE_SNAPSHOT_INTERVAL
        );
        let names: Vec<&str> = at_start.iter().map(|(name, _)| *name).collect();
        assert!(names.contains(&"socket.recv_datagrams"), "{names:?}");
        assert!(names.contains(&"gossip.neighbor_up"), "{names:?}");
        assert_eq!(names.len(), 1 + 35 + 18, "{names:?}");
        drop(scratch);
    }

    const LIVE_RUN_BYTES: [u8; crate::trace::RUN_ID_BYTES] = [0x1a; crate::trace::RUN_ID_BYTES];
    const PRUNING_RUN_BYTES: [u8; crate::trace::RUN_ID_BYTES] = [0x2b; crate::trace::RUN_ID_BYTES];
    const STALE_EVENTS_FILE: &str = "20000101T000000Z-0000000000000003-estafette.events.jsonl";
    const STALE_RAW_FILE: &str = "20000101T000000Z-0000000000000003-estafette.raw.jsonl";
    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const ONE_MILLISECOND: Duration = Duration::from_millis(1);
    const STALE_RUN_AGE: Duration = Duration::from_hours(24);

    /// The trace directory under a fresh data home for the test.
    fn trace_directory_for(test_name: &str) -> (std::path::PathBuf, crate::dirs::TraceDirectory) {
        let root = std::env::temp_dir().join(format!(
            "estafette-rebind-{test_name}-{}",
            std::process::id()
        ));
        drop(std::fs::remove_dir_all(&root));
        let data_home = root.join("data");
        let directory = crate::dirs::TraceDirectory::resolve(&move |name: &str| {
            (name == DATA_HOME_VARIABLE).then(|| std::ffi::OsString::from(&data_home))
        })
        .unwrap();
        (root, directory)
    }

    /// The trace clocks reading the milliseconds since the Unix epoch of the machine's clock.
    fn machine_trace_clocks(
        wall_ms: impl Fn() -> u64 + Send + Sync + 'static,
    ) -> crate::trace::TraceClocks {
        crate::trace::TraceClocks {
            monotonic_ms: Arc::new(|| 0),
            wall_ms: Arc::new(wall_ms),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn observability_decisions_9_and_13_a_quiet_live_member_writes_its_events_file_every_60_s_so_a_second_start_spares_its_run()
     {
        use crate::trace::{RunId, Trace, TraceBounds, TraceEffects};

        let scratch = Scratch::new("state-file").await;
        let (root, directory) = trace_directory_for("state-file");
        let live = Arc::new(
            Trace::start(
                &directory,
                &crate::dirs::OperatingSystemFilesystem,
                RunId::from_random_bytes(LIVE_RUN_BYTES),
                &TraceEffects::of_the_operating_system(machine_trace_clocks(wall_clock_ms)),
                TraceBounds::SPEC,
            )
            .unwrap(),
        );
        let (stamped, mut events) = stamped_events();
        let tracing = Arc::clone(&live);
        let record: RunRecord = Arc::new(move |event: Event| {
            tracing.record(&event);
            stamped(event);
        });
        let _members = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(record)],
        )
        .await;
        let (started, _) = next_snapshot(&mut events).await;
        let (interval, _) = next_snapshot(&mut events).await;
        assert!(matches!(
            live.finish(),
            [
                crate::trace::TraceStop::Drained,
                crate::trace::TraceStop::Drained
            ]
        ));
        let traces = directory.path();
        for stale in [STALE_EVENTS_FILE, STALE_RAW_FILE] {
            std::fs::write(traces.join(stale), b"{}\n").unwrap();
            std::fs::File::options()
                .write(true)
                .open(traces.join(stale))
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + STALE_RUN_AGE)
                .unwrap();
        }
        let live_events = std::fs::read_to_string(&live.files().events).unwrap();
        let written_ms = u64::try_from(
            std::fs::metadata(&live.files().events)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap();
        let just_before_the_next_snapshot_ms = written_ms
            + u64::try_from(
                (interval - started)
                    .checked_sub(ONE_MILLISECOND)
                    .unwrap()
                    .as_millis(),
            )
            .unwrap();

        let pruning = Trace::start(
            &directory,
            &crate::dirs::OperatingSystemFilesystem,
            RunId::from_random_bytes(PRUNING_RUN_BYTES),
            &TraceEffects::of_the_operating_system(machine_trace_clocks(move || {
                just_before_the_next_snapshot_ms
            })),
            TraceBounds {
                directory_bytes: 1,
                ..TraceBounds::SPEC
            },
        )
        .unwrap();
        drop(pruning.finish());

        assert_eq!(interval - started, SPEC_STATE_SNAPSHOT_INTERVAL);
        assert!(
            live_events.contains("\"event\":\"state.counters\",\"moment\":\"interval\""),
            "{live_events}"
        );
        assert!(live.files().events.exists());
        assert!(live.files().raw.exists());
        assert!(!traces.join(STALE_EVENTS_FILE).exists());
        assert!(!traces.join(STALE_RAW_FILE).exists());
        drop(std::fs::remove_dir_all(&root));
        drop(scratch);
    }

    #[tokio::test]
    async fn observability_decision_9_a_stopping_member_records_its_counters_after_its_departure() {
        let scratch = Scratch::new("state-stop").await;
        let (record, mut events) = recorded_events();
        let members = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(record)],
        )
        .await;
        let at_start = next_recorded(&mut events, "state.counters").await;

        members.first().unwrap().node.start_stopping();
        let at_stop = next_recorded(&mut events, "state.counters").await;

        assert_eq!(
            at_start.first(),
            Some(&("moment", serde_json::Value::from("start")))
        );
        assert_eq!(
            at_stop.first(),
            Some(&("moment", serde_json::Value::from("stop")))
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
    async fn spec_3_8_a_member_whose_wall_clock_stands_still_asks_again_at_its_rebinds_join_and_a_member_answers()
     {
        let scratch = Scratch::new("ask-again").await;
        let leading: MonotonicClock = Arc::new(|| {
            Instant::now()
                .checked_add(ANSWERING_MEMBER_DEAF_LATER_BY)
                .unwrap()
        });
        let answering = start_members(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_clock(frozen_clock(FROZEN_CLOCK_MS))
                    .with_monotonic(leading),
            ],
        )
        .await
        .pop()
        .unwrap();
        let mut witness = Witness::joined_through(WITNESS_SEED, None, &answering.endpoint).await;

        let _asking = start_members_reaching(
            &scratch,
            vec![
                MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC)
                    .with_hosts(&[WITNESS_SEED])
                    .with_clock(frozen_clock(FROZEN_CLOCK_MS)),
            ],
            &[(WITNESS_SEED, witness.address())],
        )
        .await;
        let first_ask = witness
            .payload_where(PAYLOAD_WAIT, presence_request_by(SECOND_SEED))
            .await;
        let ask_at_the_rebinds_join = witness
            .payload_where(SPEC_REBINDS_JOIN_WITHIN, presence_request_by(SECOND_SEED))
            .await;
        let answer = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;

        assert!(request_seq(first_ask) < request_seq(ask_at_the_rebinds_join));
        assert!(answer.is_some());
        drop(scratch);
    }

    fn request_seq(payload: Option<Payload>) -> u64 {
        let Some(Payload::PresenceRequest(request)) = payload else {
            panic!("a presence request was expected");
        };
        request.seq
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
        assert!(
            entry_meeting_within(
                &member,
                PEER_SEED,
                |listed| listed.attached == ATTACHED_ONLY_IN_THE_PEERS_ACK,
                PAYLOAD_WAIT
            )
            .await
            .is_some(),
            "the member never applied the ack of its own Introduce to the peer"
        );
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
    fn observability_decision_9_a_rebind_is_caused_by_the_join_without_a_neighbor_only_when_it_came_before_the_deaf_timeout_ran_out()
     {
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let started = Instant::now();
            let mut schedule = RebindSchedule::new(started);
            assert_eq!(schedule.next_rebind().1, RebindCause::Deaf, "case {case}");
            let timed_out = started + Duration::from_millis(draws.below(LONGEST_DRAWN_MS));
            schedule.join_timed_out(timed_out);
            let expected = if timed_out < started + SPEC_DEAF_TIMEOUT {
                RebindCause::JoinGainedNoNeighbor
            } else {
                RebindCause::Deaf
            };
            assert_eq!(schedule.next_rebind().1, expected, "case {case}");
        }
    }

    #[test]
    fn spec_3_8_a_late_join_clears_the_join_timeout_mark_so_only_silence_makes_a_rebind_due() {
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let started = Instant::now();
            let mut schedule = RebindSchedule::new(started);
            let timed_out = started + Duration::from_millis(draws.below(LONGEST_DRAWN_MS));
            schedule.join_timed_out(timed_out);
            schedule.joined();

            assert_eq!(schedule.due(), started + SPEC_DEAF_TIMEOUT, "case {case}");
        }
    }

    #[test]
    fn spec_3_8_member_joins_start_1_5_s_after_a_rebinds_join_repeat_every_3_s_and_stop_after_10() {
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let joined = Instant::now() + Duration::from_millis(draws.below(LONGEST_DRAWN_MS));
            let mut joins = MemberJoins::AwaitingRebindJoin;
            joins.joined(joined);
            let mut expected_due = joined + SPEC_MEMBER_JOIN_WAIT;
            let mut asks: u32 = 0;
            while let Some(due) = joins.next_ask() {
                assert_eq!(due, expected_due, "case {case}");
                asks += 1;
                assert!(asks <= SPEC_MEMBER_JOIN_TRIES, "case {case}");
                let asked_at = due + Duration::from_millis(draws.below(LONGEST_ASK_LATENESS_MS));
                joins.asked(asked_at);
                expected_due = asked_at + SPEC_MEMBER_JOIN_INTERVAL;
            }

            assert_eq!(asks, SPEC_MEMBER_JOIN_TRIES, "case {case}");
        }
    }

    #[test]
    fn spec_3_8_the_first_join_starts_no_member_joins() {
        let mut joins = MemberJoins::Idle;
        joins.joined(Instant::now());

        assert_eq!(joins.next_ask(), None);
    }
}
