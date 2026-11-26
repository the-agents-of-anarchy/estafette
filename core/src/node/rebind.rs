//! The rebind of spec 3.8 and D46: when a member falls deaf or its join gains no neighbor, the rebind itself, and the member join loop after it.

use std::collections::BTreeSet;
use std::future::Future;
use std::ops::ControlFlow;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use tokio::net::TcpStream;
use tokio::time::Instant;

use crate::clock::{before_deadline, later_by};
use crate::event::{GossipEvent, TransportEvent};
use crate::net::direct::DirectHost;
use crate::net::endpoint::{RebindCause, SnapshotMoment};
use crate::net::gossip::NetworkTopic;
use crate::node::addresses::endpoint_id;
use crate::node::known_hosts::{KnownHostAddress, seed_lookup_with_known_hosts};
use crate::node::presence::{
    PresenceLoop, home_relay_changes, own_address_changes, record_counters, serve,
};
use crate::node::relay_loss::RelayLossWatch;
use crate::peers::Status;
use crate::stream_items::join_all;
use crate::wire;

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
        self.record_skipped_known_hosts();
        self.record_state(SnapshotMoment::Rebind);
        self.rebinding.rebound(self.monotonic.now());
        self.close_endpoint().await;
        let down = self.relays_found_down().await;
        self.strip_relays_from_members(&down);
        if let Err(failure) = self.endpoint.rebind().await {
            (self.report)(TransportEvent::RebindFailed(failure).into());
            return ControlFlow::Break(());
        }
        self.client.follow(self.endpoint.endpoint().clone()).await;
        let hosts = self.known_hosts.addresses();
        self.seed_lookup_recording_each_entry(&hosts);
        let bootstrap = self.bootstrap_nodes(&hosts);
        self.serve_fresh_endpoint();
        record_counters(
            SnapshotMoment::Rebound,
            self.endpoint.endpoint(),
            &self.gossip,
            &*self.report,
        );
        self.watch_fresh_endpoint();
        self.subscribe_through(bootstrap).await
    }

    /// Records each configured host this rebind leaves out because its id or relay does not parse.
    fn record_skipped_known_hosts(&self) {
        for skipped in self.known_hosts.skipped() {
            (self.report)(TransportEvent::KnownHostSkipped(skipped).into());
        }
    }

    /// Asks the gossip layer to join the Online and Unreachable members, stopping the asks when there are none or one of them is a gossip neighbor already.
    pub(super) async fn ask_members_to_join(&mut self) {
        let mut members = self.peers_with_status(Status::Online);
        members.extend(self.peers_with_status(Status::Unreachable));
        if members.is_empty() || self.rebinding.has_neighbor_among(&members) {
            self.rebinding.stop_asking();
            return;
        }
        let now = self.monotonic.now();
        if let Err(failure) = NetworkTopic::join(
            &self.gossip,
            self.keys.clone(),
            self.owner,
            members,
            Arc::clone(&self.monotonic),
        )
        .await
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

    /// The relays of the known hosts that do not answer the probe before the node's clock reads the relay probe timeout past the probe, recording each probe's answer.
    async fn relays_found_down(&self) -> BTreeSet<RelayUrl> {
        let relays: BTreeSet<RelayUrl> = self
            .known_hosts
            .addresses()
            .into_iter()
            .map(|host| host.relay)
            .collect();
        let deadline = later_by(self.monotonic.now(), RELAY_PROBE_TIMEOUT);
        let answers = join_all(relays.iter().map(|relay| {
            before_deadline(
                &*self.monotonic,
                Some(deadline),
                self.rebinding.probe(relay.clone()),
            )
        }))
        .await;
        let mut down = BTreeSet::new();
        for (relay, answer) in relays.into_iter().zip(answers) {
            let answered = matches!(answer, Some(true));
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

    /// Strips the relays found down from the lookup entry of every other member, recording each entry it changes.
    fn strip_relays_from_members(&self, down: &BTreeSet<RelayUrl>) {
        let members: Vec<wire::PublicKey> = self
            .state
            .table()
            .peers(self.state.now_ms())
            .map(|peer| peer.card)
            .collect();
        for card in members {
            let Some(address) = endpoint_id(&card).and_then(|id| self.endpoint.address_of(id))
            else {
                continue;
            };
            let stripped = without_relays(address.clone(), down);
            if stripped != address {
                self.endpoint.replace_address(stripped);
                self.record_lookup_of(&card);
            }
        }
    }

    /// Gives each known host whose lookup entry holds no address its relay, recording each entry it seeds.
    fn seed_lookup_recording_each_entry(&self, hosts: &[KnownHostAddress]) {
        for card in seed_lookup_with_known_hosts(&self.endpoint, hosts) {
            self.record_lookup_of(&card);
        }
    }

    /// The nodes the join goes through: every known host, then every Online peer whose address the lookup holds.
    fn bootstrap_nodes(&self, hosts: &[KnownHostAddress]) -> Vec<EndpointId> {
        let mut bootstrap: Vec<EndpointId> = hosts.iter().map(|host| host.id).collect();
        bootstrap.extend(
            self.peers_with_status(Status::Online)
                .into_iter()
                .filter(|peer| self.endpoint.address_of(*peer).is_some()),
        );
        bootstrap
    }

    /// Serves the fresh endpoint with a fresh gossip instance.
    fn serve_fresh_endpoint(&mut self) {
        let (router, gossip) = serve(&self.endpoint, &self.keys, &self.state);
        self.router = router;
        self.gossip = gossip;
    }

    /// Follows the fresh endpoint's own addresses and home relays, and starts the relay loss watch afresh.
    fn watch_fresh_endpoint(&mut self) {
        self.own_addresses = own_address_changes(self.endpoint.endpoint());
        self.relay_statuses = home_relay_changes(self.endpoint.endpoint());
        self.relay_loss = RelayLossWatch::default();
    }

    /// Subscribes to the topic through the nodes, stopping the loop when the join fails.
    async fn subscribe_through(&mut self, bootstrap: Vec<EndpointId>) -> ControlFlow<()> {
        match NetworkTopic::join(
            &self.gossip,
            self.keys.clone(),
            self.owner,
            bootstrap,
            Arc::clone(&self.monotonic),
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::Duration;

    use iroh::protocol::Router;
    use iroh::{EndpointAddr, EndpointId, RelayUrl};
    use iroh_gossip::ALPN as GOSSIP_ALPN;
    use tokio::sync::watch;
    use tokio::task::JoinHandle;
    use tokio::time::Instant;

    use super::{MemberJoins, RebindSchedule, RelayProbe, relay_answers};
    use crate::clock::{MonotonicClock, MonotonicTime, RuntimeClock, later_by};
    use crate::config::{Host, HostOrigin};
    use crate::event::{Event, EventFields, RunRecord, TransportEvent};
    use crate::keys::render_public_key;
    use crate::net::direct::tests::{
        INTERVAL_MOMENT, INTERVAL_SNAPSHOT_WITHIN, IROH_DOCUMENTED_CLOSE_LATENCY, PAYLOAD_ACCEPTED,
        SPEC_DEAF_TIMEOUT, SPEC_STATE_SNAPSHOT_INTERVAL, START_MOMENT, STATE_COUNTERS,
        ShiftedClock, StandingClock, closed_within_the_documented_latency,
        counters_snapshot_one_interval_on, deadline_of_the_gossip_join_timeout, test_salt,
        was_taken_at,
    };
    use crate::net::direct::{ClientTimeouts, DirectClient, DirectHost, DirectServer};
    use crate::net::endpoint::{
        BoundEndpoint, DIRECT_ALPN, LocationHash, NodeEndpoint, RebindCause, TransportRecord,
    };
    use crate::net::gossip::{GossipEvent, NetworkTopic};
    use crate::node::known_hosts::KnownHosts;
    use crate::node::presence::tests::{
        FIRST_HANDLE, FIRST_PUBLIC, FIRST_SEED, HOST_SEED, KIND, MARKER_SEED, Member, MemberPlan,
        OWNER_SEED, PAYLOAD_WAIT, PEER, PRESENCE_KIND, PRESENCE_REQUEST_KIND, SECOND_HANDLE,
        SECOND_PUBLIC, SECOND_SEED, SEQ, SPEC_OBSERVER_WAIT, Scratch, WITNESS_SEED, Witness, bind,
        card, config_with_hosts, entry_meeting_within, frozen_clock, idle_presence_loop,
        joined_and_asked_for_presence, keys, presence_from, presence_request_by, relay_url,
        short_peer_id, spawn_member, start_members, start_members_reaching, status_within,
        unreachable_about,
    };
    use crate::payload::ZERO_MAC;
    use crate::peers::Status;
    use crate::seeded_draws::Draws;
    use crate::stream_items::StreamExt;
    use crate::test_waits::{
        RecordedEvent, WaitMissed, found_within, next_recorded, recorded_events, recorded_until,
        secret_key, wall_clock_ms,
    };
    use crate::wire::{Addr, Introduce, IntroduceAck, Message, Payload, Version};

    const SPEC_REBIND_INTERVAL: Duration = Duration::from_secs(20);
    const SPEC_JOIN_TIMEOUT: Duration = Duration::from_millis(5000);
    const SPEC_RELAY_PROBE_TIMEOUT: Duration = Duration::from_millis(1000);
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
    const KEPT_PEER_SEED: u8 = 96;
    const BARE_PEER_SEED: u8 = 97;
    const LOOKUP_REPLACED: &str = "peers.lookup_replaced";
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

    const ATTACHED_ONLY_IN_THE_PEERS_ACK: bool = true;
    const REBINDS_JOIN_ONCE_DEAF_WITHIN: Duration = IROH_DOCUMENTED_CLOSE_LATENCY
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

        /// What `found` first finds in the entries heard within `within`, or the awaited condition and the bound when it finds nothing by then.
        async fn first_within<Found: Send>(
            &self,
            within: Duration,
            awaited: &str,
            found: impl Fn(&[T]) -> Option<Found> + Send + Sync,
        ) -> Result<Found, WaitMissed> {
            let missed = || WaitMissed::new(awaited, within);
            let mut entries = self.0.subscribe();
            let entries =
                tokio::time::timeout(within, entries.wait_for(|entries| found(entries).is_some()))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .ok_or_else(missed)?;
            found(&entries).ok_or_else(missed)
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

        async fn nth_within(&self, index: usize, within: Duration) -> Result<Instant, WaitMissed> {
            let awaited = format!("rebind number {index} started");
            self.started
                .first_within(within, &awaited, |started| started.get(index).copied())
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

        async fn broadcasting(seed: u8, every: Duration, clock: MonotonicClock) -> Self {
            let first = clock.now().checked_add(every).unwrap();
            Self::serving(
                seed,
                None,
                Some(BroadcastSchedule {
                    clock,
                    every,
                    next: first,
                    sent: 0,
                }),
            )
            .await
        }

        async fn serving(
            seed: u8,
            introduced_with_relays: Option<&[&str]>,
            mut broadcasts: Option<BroadcastSchedule>,
        ) -> Self {
            let endpoint = bind(seed).await;
            let introduction = Addr::from_endpoint_report(
                introduced_with_relays
                    .unwrap_or_default()
                    .iter()
                    .map(|relay| (*relay).to_owned()),
                endpoint.endpoint().addr().ip_addrs().copied(),
            );
            let gossip = endpoint.endpoint().spawn_gossip();
            let mut router = endpoint
                .endpoint()
                .router_builder()
                .accept(GOSSIP_ALPN, gossip.clone());
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
                Arc::new(RuntimeClock),
            )
            .await
            .unwrap();
            let presences = Heard::default();
            let neighbors_up = Heard::default();
            let heard = presences.clone();
            let met = neighbors_up.clone();
            let listening = tokio::spawn(async move {
                loop {
                    let event = tokio::select! {
                        event = topic.next_event() => event,
                        ordinal = next_broadcast(&mut broadcasts) => {
                            drop(topic.broadcast(
                                &secret_key(seed),
                                unreachable_about(MARKER_SEED, seed, wall_clock_ms().saturating_add(ordinal)),
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

        async fn introduce_to(&self, member: &BoundEndpoint) {
            self.endpoint.replace_address(member.addr());
            DirectClient::new(
                self.endpoint.endpoint().clone(),
                keys(),
                ClientTimeouts::SPEC,
                crate::net::direct::tests::unrecorded_register(),
            )
            .introduce(member.id(), peer_introduction(self.introduction.clone()))
            .await
            .unwrap();
        }

        async fn first_neighbor_up_of(
            &self,
            seed: u8,
            within: Duration,
        ) -> Result<Instant, WaitMissed> {
            let neighbor = secret_key(seed).public();
            let awaited = format!("card of seed {seed} up as a neighbor");
            self.neighbors_up
                .first_within(within, &awaited, |ups| {
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

        async fn first_seq_after(&self, at: Instant, within: Duration) -> Result<u64, WaitMissed> {
            self.presences
                .first_within(within, "presence heard after the rebind", |presences| {
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

        fn receive_message(&self, _sender: crate::wire::PublicKey, _message: Message) -> bool {
            false
        }

        fn receive_introduce(
            &self,
            _sender: crate::wire::PublicKey,
            _introduce: Introduce,
        ) -> IntroduceAck {
            IntroduceAck {
                attached: ATTACHED_ONLY_IN_THE_PEERS_ACK,
                ..IntroduceAck::from(peer_introduction(self.addr.clone()))
            }
        }
    }

    /// When a stand-in host broadcasts its next report: every span on the clock, counting the reports sent.
    struct BroadcastSchedule {
        clock: MonotonicClock,
        every: Duration,
        next: Instant,
        sent: u64,
    }

    /// The ordinal of the next report, once the clock reads its instant, the one after it scheduled a span later; never, for a node that does not broadcast.
    async fn next_broadcast(broadcasts: &mut Option<BroadcastSchedule>) -> u64 {
        match broadcasts {
            Some(schedule) => {
                schedule.clock.reached(schedule.next).await;
                schedule.next = schedule.next.checked_add(schedule.every).unwrap();
                schedule.sent = schedule.sent.saturating_add(1);
                schedule.sent
            }
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

    async fn lookup_entry(endpoint: &BoundEndpoint, seed: u8) -> Option<EndpointAddr> {
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
    ) -> Result<EndpointAddr, WaitMissed> {
        let awaited = format!("lookup entry for the card of seed {seed} meeting the condition");
        found_within(within, &awaited, async || {
            lookup_entry(&member.node.client.endpoint(), seed)
                .await
                .filter(|entry| wanted(entry))
        })
        .await
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

    /// Stand-in relays that each answer the probe once the clock reads its delay past the probe, noting every relay asked and every one that answered; a relay without a delay answers no at once.
    #[derive(Clone)]
    struct RelaysAnsweringOnTheClock {
        clock: MonotonicClock,
        delays: &'static [(&'static str, Duration)],
        asked: Arc<std::sync::Mutex<Vec<String>>>,
        answered: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl RelaysAnsweringOnTheClock {
        fn new(clock: MonotonicClock, delays: &'static [(&'static str, Duration)]) -> Self {
            Self {
                clock,
                delays,
                asked: Arc::default(),
                answered: Arc::default(),
            }
        }

        /// The relay probe a member takes, answered by these relays.
        fn probe(&self) -> RelayProbe {
            let relays = self.clone();
            Arc::new(move |relay: RelayUrl| Box::pin(relays.clone().answer(relay.to_string())))
        }

        /// Notes the relay asked, then answers yes once the clock reads its delay past now, noting the answer, or no at once for a relay without a delay.
        async fn answer(self, relay: String) -> bool {
            self.asked.lock().unwrap().push(relay.clone());
            let Some(delay) = self
                .delays
                .iter()
                .find(|(answering, _)| relay == *answering)
                .map(|(_, delay)| *delay)
            else {
                return false;
            };
            let asked_at = self.clock.now();
            self.clock
                .reached(asked_at.checked_add(delay).unwrap())
                .await;
            self.answered.lock().unwrap().push(relay);
            true
        }

        /// How many relays were asked so far.
        fn asked(&self) -> usize {
            self.asked.lock().unwrap().len()
        }

        /// Whether the relay has answered.
        fn has_answered(&self, relay: &str) -> bool {
            self.answered
                .lock()
                .unwrap()
                .iter()
                .any(|answered| answered == relay)
        }
    }

    #[tokio::test]
    async fn spec_3_15_test_24_logic_a_member_whose_host_broadcasts_every_5_s_records_no_rebind_past_the_deaf_timeout()
     {
        let scratch = Scratch::new("hearing").await;
        let clock = StandingClock::standing_now();
        let host = QuietNode::broadcasting(
            HOST_SEED,
            SPEC_INFRASTRUCTURE_RECORD_INTERVAL,
            clock.monotonic(),
        )
        .await;
        let (record, mut events) = recorded_events();
        let running_host = [(HOST_SEED, host.addr())];

        let (_member, mut before_the_last_report) = tokio::join!(
            start_members_reaching(
                &scratch,
                vec![
                    MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                        .with_hosts(&[HOST_SEED])
                        .with_report(record)
                        .with_monotonic(clock.monotonic()),
                ],
                &running_host,
            ),
            async {
                let joined = joined_and_asked_for_presence(&mut events).await;
                clock.move_forward(SPEC_OBSERVER_WAIT);
                joined
            }
        );
        for _report in 0..REPORTS_PAST_THE_DEAF_TIMEOUT {
            clock.move_forward(SPEC_INFRASTRUCTURE_RECORD_INTERVAL);
            let (before_the_report, _report) = recorded_until(
                &mut events,
                DROPPED_PAYLOAD,
                |fields| fields.contains(&(PEER, short_peer_id(HOST_SEED))),
                REPORT_HEARD_WITHIN,
            )
            .await;
            before_the_last_report.extend(before_the_report);
        }

        assert_eq!(
            rebinds_among(&before_the_last_report),
            0,
            "the member rebound while hearing its host: {before_the_last_report:?}"
        );
        drop(scratch);
    }

    /// A member plan whose two known hosts hold the kept and the stripped relay, recording its events and the transport of its held connections to the record.
    fn plan_with_kept_and_stripped_host_relays(record: &RunRecord) -> MemberPlan {
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_host_relays(&[
                (LIVE_HOST_SEED, KEPT_RELAY),
                (DEAD_HOST_SEED, STRIPPED_RELAY),
            ])
            .with_report(Arc::clone(record))
            .with_register(crate::net::direct::HeldConnections::new(
                Arc::new(RuntimeClock),
                TransportRecord::new(Arc::clone(record), test_salt()),
            ))
    }

    #[tokio::test]
    async fn spec_3_14_a_relay_answering_within_the_1000_ms_probe_timeout_is_kept_and_one_answering_after_it_is_stripped()
     {
        let scratch = Scratch::new("probe-timeout").await;
        let clock = StandingClock::standing_now();
        let relays = RelaysAnsweringOnTheClock::new(
            clock.monotonic(),
            &[
                (KEPT_RELAY, ANSWER_WITHIN_THE_PROBE_TIMEOUT),
                (STRIPPED_RELAY, ANSWER_AFTER_THE_PROBE_TIMEOUT),
            ],
        );
        let (record, mut events) = recorded_events();
        let peer = QuietNode::answering(PEER_SEED, &KNOWN_HOST_RELAYS).await;
        let (member_endpoint, starting) = spawn_member(
            &scratch,
            plan_with_kept_and_stripped_host_relays(&record)
                .with_relay_probe(relays.probe())
                .with_monotonic(clock.monotonic()),
        )
        .await;
        peer.introduce_to(&member_endpoint).await;
        next_recorded(&mut events, STATE_COUNTERS).await;
        clock.move_forward(SPEC_JOIN_TIMEOUT);
        let member = starting.await.unwrap();

        recorded_until(
            &mut events,
            REBINDING,
            |_fields| true,
            SPEC_JOIN_TIMEOUT + REBIND_SLACK,
        )
        .await;
        found_within(
            RELAYS_ASKED_WITHIN,
            "rebind asking every known host relay",
            async || (relays.asked() == KNOWN_HOST_RELAYS.len()).then_some(()),
        )
        .await
        .unwrap_or_else(|missed| {
            panic!(
                "{missed}: it asked {} of the {} relays",
                relays.asked(),
                KNOWN_HOST_RELAYS.len()
            )
        });
        clock.move_forward(ANSWER_WITHIN_THE_PROBE_TIMEOUT);
        found_within(
            RELAY_ANSWER_TAKEN_WITHIN,
            "answer of the relay answering within the probe timeout, counted from the clock reading it",
            async || relays.has_answered(KEPT_RELAY).then_some(()),
        )
        .await
        .unwrap_or_else(WaitMissed::fail_the_test);
        let judged_early: Vec<RecordedEvent> = std::iter::from_fn(|| events.try_recv().ok())
            .filter(|(name, _fields)| *name == RELAY_PROBED)
            .collect();
        assert!(
            judged_early.is_empty(),
            "relays were judged before the clock read the probe timeout past the probe: {judged_early:?}"
        );
        clock.move_forward(PROBE_TIMEOUT_LEFT_AFTER_THE_ANSWER_WITHIN_IT);
        next_recorded(&mut events, RELAY_PROBED).await;
        next_recorded(&mut events, RELAY_PROBED).await;
        let stripped = entry_within(
            &member,
            PEER_SEED,
            |entry| !relays_of(entry).contains(&STRIPPED_RELAY.to_owned()),
            REBIND_SLACK,
        )
        .await
        .unwrap_or_else(WaitMissed::fail_the_test);

        assert_eq!(relays_of(&stripped), vec![KEPT_RELAY.to_owned()]);
        drop(scratch);
    }

    #[tokio::test]
    async fn observability_decision_9_a_rebind_records_its_cause_the_old_endpoints_counters_before_it_closes_and_each_relay_probes_answer_by_port_and_salted_hash()
     {
        let scratch = Scratch::new("probe-record").await;
        let (record, mut events) = recorded_events();
        let peer = QuietNode::answering(PEER_SEED, &KNOWN_HOST_RELAYS).await;
        let (member_endpoint, starting) = spawn_member(
            &scratch,
            plan_with_kept_and_stripped_host_relays(&record)
                .with_relay_probe(answering_only(KEPT_RELAY)),
        )
        .await;
        peer.introduce_to(&member_endpoint).await;
        let _member = starting.await.unwrap();

        let rebinding = next_recorded(&mut events, "transport.rebinding").await;
        let (before_the_snapshot, at_the_rebind) = recorded_until(
            &mut events,
            STATE_COUNTERS,
            |_fields| true,
            SNAPSHOT_AT_THE_REBIND_WITHIN,
        )
        .await;
        assert!(
            !before_the_snapshot
                .iter()
                .any(|(name, _fields)| SHOWS_THE_OLD_ENDPOINT_CLOSED.contains(name)),
            "the counters are read before the old endpoint closes: {before_the_snapshot:?}"
        );
        let first_probe = next_recorded(&mut events, RELAY_PROBED).await;
        let second_probe = next_recorded(&mut events, RELAY_PROBED).await;

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
        assert!(
            was_taken_at(&at_the_rebind, REBIND_MOMENT),
            "{at_the_rebind:?}"
        );
        assert!(
            at_the_rebind
                .iter()
                .any(|(name, count)| *name == RECEIVED_DATAGRAMS
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

    const NOT_A_NODE_ID: &str = "not a node id";
    const NOT_A_RELAY_URL: &str = "not a relay url";

    fn configured_host_of(id: String, relay: &str) -> Host {
        Host {
            id,
            relay: relay.to_owned(),
            ts: None,
            origin: HostOrigin::Configured,
        }
    }

    #[tokio::test]
    async fn spec_3_8_a_rebind_records_each_known_host_it_skips_by_its_place_and_the_part_that_did_not_parse_and_goes_on()
     {
        let scratch = Scratch::new("rb-skip").await;
        let (record, mut events) = recorded_events();
        let mut presence_loop = idle_presence_loop(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(record),
        )
        .await;
        let live_id = render_public_key(&secret_key(LIVE_HOST_SEED).public());
        presence_loop.known_hosts = Arc::new(KnownHosts::new(
            config_with_hosts(vec![
                configured_host_of(NOT_A_NODE_ID.to_owned(), LIVE_RELAY),
                configured_host_of(live_id.clone(), LIVE_RELAY),
                configured_host_of(live_id, NOT_A_RELAY_URL),
            ]),
            std::path::PathBuf::new(),
        ));

        let flow = presence_loop.rebind(RebindCause::Deaf).await;
        let first_skipped = next_recorded(&mut events, "transport.known_host_skipped").await;
        let second_skipped = next_recorded(&mut events, "transport.known_host_skipped").await;

        assert_eq!(flow, std::ops::ControlFlow::Continue(()));
        assert_eq!(
            first_skipped,
            vec![
                ("index", serde_json::Value::from(0)),
                ("reason", serde_json::Value::from("id")),
            ]
        );
        assert_eq!(
            second_skipped,
            vec![
                ("index", serde_json::Value::from(2)),
                ("reason", serde_json::Value::from("relay")),
            ]
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn observability_decision_9_a_rebind_records_the_fresh_endpoints_counters_once_it_is_bound_after_every_relay_probe()
     {
        let scratch = Scratch::new("rebound-record").await;
        let (record, mut events) = recorded_events();
        let peer = QuietNode::answering(PEER_SEED, &KNOWN_HOST_RELAYS).await;
        let (member_endpoint, starting) = spawn_member(
            &scratch,
            plan_with_kept_and_stripped_host_relays(&record)
                .with_relay_probe(answering_only(KEPT_RELAY)),
        )
        .await;
        peer.introduce_to(&member_endpoint).await;
        let _member = starting.await.unwrap();

        next_recorded(&mut events, REBINDING).await;
        let at_the_rebind = next_recorded(&mut events, STATE_COUNTERS).await;
        let (before_the_fresh_snapshot, at_the_fresh_bind) = recorded_until(
            &mut events,
            STATE_COUNTERS,
            |_fields| true,
            SNAPSHOT_AT_THE_REBIND_WITHIN,
        )
        .await;

        assert!(
            was_taken_at(&at_the_fresh_bind, REBOUND_MOMENT),
            "{at_the_fresh_bind:?}"
        );
        assert_eq!(
            before_the_fresh_snapshot
                .iter()
                .filter(|(name, _fields)| *name == RELAY_PROBED)
                .count(),
            KNOWN_HOST_RELAYS.len(),
            "the fresh endpoint's counters follow every relay probe: {before_the_fresh_snapshot:?}"
        );
        assert!(
            counted_in(&at_the_fresh_bind, RECEIVED_DATAGRAMS)
                < counted_in(&at_the_rebind, RECEIVED_DATAGRAMS),
            "the fresh endpoint counts its datagrams afresh, below the old endpoint's: {at_the_fresh_bind:?} after {at_the_rebind:?}"
        );
        assert!(
            counted_in(&at_the_fresh_bind, GOSSIP_REQUESTS_TAKEN)
                < counted_in(&at_the_rebind, GOSSIP_REQUESTS_TAKEN),
            "the fresh gossip instance counts its requests afresh, below the old instance's: {at_the_fresh_bind:?} after {at_the_rebind:?}"
        );
        drop(scratch);
    }

    /// The count the snapshot read for the counter.
    #[track_caller]
    fn counted_in(snapshot: &EventFields, counter: &str) -> u64 {
        snapshot
            .iter()
            .find(|(name, _count)| *name == counter)
            .and_then(|(_name, count)| count.as_u64())
            .unwrap_or_else(|| panic!("no {counter} count in {snapshot:?}"))
    }

    const SNAPSHOT_AT_THE_REBIND_WITHIN: Duration = Duration::from_secs(10);
    const REBOUND_MOMENT: &str = "rebound";
    const RECEIVED_DATAGRAMS: &str = "socket.recv_datagrams";
    /// The requests a gossip instance has taken, its topic joins among them, so a fresh instance counts none before its join.
    const GOSSIP_REQUESTS_TAKEN: &str = "gossip.actor_tick_rx";
    /// The events a rebind records only once its old endpoint has closed: that endpoint's connections and paths ending, then the relay probes.
    const SHOWS_THE_OLD_ENDPOINT_CLOSED: [&str; 3] = [
        "transport.connection_closed",
        "transport.path_closed",
        RELAY_PROBED,
    ];
    const REBINDING: &str = "transport.rebinding";
    const DROPPED_PAYLOAD: &str = "gossip.payload_dropped";
    const RELAY_PROBED: &str = "transport.relay_probed";
    /// The relays of the two known hosts a rebind probes.
    const KNOWN_HOST_RELAYS: [&str; 2] = [KEPT_RELAY, STRIPPED_RELAY];
    /// The longest a rebind takes from its start to ask every relay: the old endpoint's close, then the asks.
    const RELAYS_ASKED_WITHIN: Duration =
        IROH_DOCUMENTED_CLOSE_LATENCY.saturating_add(REBIND_SLACK);
    /// The longest a stand-in relay takes to answer once the clock reads its delay.
    const RELAY_ANSWER_TAKEN_WITHIN: Duration = Duration::from_secs(5);
    const PROBE_TIMEOUT_LEFT_AFTER_THE_ANSWER_WITHIN_IT: Duration =
        SPEC_RELAY_PROBE_TIMEOUT.saturating_sub(ANSWER_WITHIN_THE_PROBE_TIMEOUT);
    const SOCKET_COUNTERS: usize = 35;
    const GOSSIP_COUNTERS: usize = 18;
    /// The fields of a counters snapshot: its moment, then the counters iroh keeps for the socket and for the gossip.
    const SNAPSHOT_FIELDS: usize = 1 + SOCKET_COUNTERS + GOSSIP_COUNTERS;
    const REBIND_MOMENT: &str = "rebind";
    const STOP_MOMENT: &str = "stop";
    /// The longest a host's report takes to reach the member that hears it.
    const REPORT_HEARD_WITHIN: Duration = Duration::from_secs(10);
    /// How many reports, an interval apart on the clock, move it past the deaf timeout and its slack.
    const REPORTS_PAST_THE_DEAF_TIMEOUT: u32 = 4;
    const _: () = assert!(
        SPEC_INFRASTRUCTURE_RECORD_INTERVAL
            .saturating_mul(REPORTS_PAST_THE_DEAF_TIMEOUT)
            .as_millis()
            >= SPEC_DEAF_TIMEOUT.saturating_add(REBIND_SLACK).as_millis()
    );

    /// Whether the counters were recorded after the start: at the interval, at a rebind or at a stop.
    fn is_snapshot_after_the_start(fields: &EventFields) -> bool {
        !was_taken_at(fields, START_MOMENT)
    }

    impl RebindSchedule {
        /// When the next rebind is due, for a member that may rebind.
        fn due(&self) -> Instant {
            self.next_rebind().0
        }
    }

    /// How many rebinds the events hold.
    fn rebinds_among(events: &[RecordedEvent]) -> usize {
        events
            .iter()
            .filter(|(name, _fields)| *name == REBINDING)
            .count()
    }

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
        let found = tokio::time::timeout(INTERVAL_SNAPSHOT_WITHIN, async {
            loop {
                let (at, name, fields) = events.recv().await.unwrap();
                if name == STATE_COUNTERS {
                    return (at, fields);
                }
            }
        })
        .await;
        found.unwrap_or_else(|_elapsed| {
            panic!("no {STATE_COUNTERS} within {INTERVAL_SNAPSHOT_WITHIN:?}")
        })
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

        assert!(was_taken_at(&at_start, START_MOMENT), "{at_start:?}");
        assert!(
            was_taken_at(&at_first_interval, INTERVAL_MOMENT),
            "{at_first_interval:?}"
        );
        assert_eq!(first_interval - started, SPEC_STATE_SNAPSHOT_INTERVAL);
        assert_eq!(
            second_interval - first_interval,
            SPEC_STATE_SNAPSHOT_INTERVAL
        );
        let names: Vec<&str> = at_start.iter().map(|(name, _)| *name).collect();
        assert!(names.contains(&RECEIVED_DATAGRAMS), "{names:?}");
        assert!(names.contains(&"gossip.neighbor_up"), "{names:?}");
        assert_eq!(names.len(), SNAPSHOT_FIELDS, "{names:?}");
        drop(scratch);
    }

    #[tokio::test]
    async fn observability_decision_9_a_quiet_member_records_its_counters_once_its_handed_clock_reads_60_s_past_its_start_and_again_60_s_past_that_reading()
     {
        let scratch = Scratch::new("state-on-the-clock").await;
        let clock = StandingClock::standing_an_hour_ahead();
        let (record, mut events) = recorded_events();

        let (_members, at_the_intervals) = tokio::join!(
            start_members(
                &scratch,
                vec![
                    MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                        .with_report(record)
                        .with_monotonic(clock.monotonic()),
                ],
            ),
            async {
                next_recorded(&mut events, STATE_COUNTERS).await;
                let mut at_the_intervals = Vec::new();
                let mut asked = deadline_of_the_gossip_join_timeout(&clock);
                for interval in ["first", "second"] {
                    let (_, snapshot) = counters_snapshot_one_interval_on(
                        &clock,
                        &mut events,
                        &format!("the quiet member's {interval} interval counters"),
                        &mut asked,
                        is_snapshot_after_the_start,
                    )
                    .await;
                    at_the_intervals.push(snapshot);
                }
                at_the_intervals
            }
        );

        for at_the_interval in at_the_intervals {
            assert!(
                was_taken_at(&at_the_interval, INTERVAL_MOMENT),
                "{at_the_interval:?}"
            );
        }
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
        let root = crate::this_process_directory::fresh_directory_of_this_process_in(
            &std::env::temp_dir(),
            &format!("estafette-rebind-{test_name}"),
        )
        .unwrap();
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
                &crate::dirs::OperatingSystemFilesystem::of_this_process().unwrap(),
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
            &crate::dirs::OperatingSystemFilesystem::of_this_process().unwrap(),
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
        crate::remove_scratch::remove_scratch(&root).unwrap();
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
        let at_start = next_recorded(&mut events, STATE_COUNTERS).await;

        members.first().unwrap().node.start_stopping();
        let at_stop = next_recorded(&mut events, STATE_COUNTERS).await;

        assert!(was_taken_at(&at_start, START_MOMENT), "{at_start:?}");
        assert!(was_taken_at(&at_stop, STOP_MOMENT), "{at_stop:?}");
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
            .unwrap_or_else(WaitMissed::fail_the_test);
        let second = rebinds
            .nth_within(1, SPEC_REBIND_INTERVAL + REBIND_SLACK)
            .await
            .unwrap_or_else(WaitMissed::fail_the_test);

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
            .unwrap_or_else(WaitMissed::fail_the_test);
        let announced_after = host
            .first_seq_after(rebound, SPEC_JOIN_TIMEOUT)
            .await
            .unwrap_or_else(WaitMissed::fail_the_test);

        assert_eq!(host.last_seq_before(rebound), Some(FROZEN_CLOCK_MS));
        assert_eq!(announced_after, FROZEN_CLOCK_MS + 1);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_member_whose_wall_clock_stands_still_asks_again_at_its_rebinds_join_and_a_member_answers()
     {
        let scratch = Scratch::new("ask-again").await;
        let (answering_record, mut answering_events) = recorded_events();
        let answering = start_members(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_clock(frozen_clock(FROZEN_CLOCK_MS))
                    .with_report(answering_record),
            ],
        )
        .await
        .pop()
        .unwrap();
        let mut witness = Witness::joined_through(WITNESS_SEED, None, &answering.endpoint).await;
        let asking_clock = ShiftedClock::default();
        let (asking_record, mut asking_events) = recorded_events();

        let _asking = start_members_reaching(
            &scratch,
            vec![
                MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC)
                    .with_hosts(&[WITNESS_SEED])
                    .with_clock(frozen_clock(FROZEN_CLOCK_MS))
                    .with_monotonic(asking_clock.monotonic())
                    .with_report(asking_record),
            ],
            &[(WITNESS_SEED, witness.address())],
        )
        .await;
        let first_ask = witness
            .payload_where(PAYLOAD_WAIT, presence_request_by(SECOND_SEED))
            .await;
        recorded_until(
            &mut asking_events,
            PAYLOAD_ACCEPTED,
            |fields| {
                fields.contains(&(KIND, serde_json::Value::from(PRESENCE_KIND)))
                    && fields.contains(&(PEER, short_peer_id(FIRST_SEED)))
            },
            PAYLOAD_WAIT,
        )
        .await;
        asking_clock.move_forward(SPEC_DEAF_TIMEOUT);
        let ask_at_the_rebinds_join = request_seq(
            witness
                .payload_where(
                    REBINDS_JOIN_ONCE_DEAF_WITHIN,
                    presence_request_by(SECOND_SEED),
                )
                .await,
        );
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");
        let (before_the_answering_member_took_the_ask, _ask) = recorded_until(
            &mut answering_events,
            PAYLOAD_ACCEPTED,
            |fields| {
                fields.contains(&(KIND, serde_json::Value::from(PRESENCE_REQUEST_KIND)))
                    && fields.contains(&(SEQ, serde_json::Value::from(ask_at_the_rebinds_join)))
            },
            PAYLOAD_WAIT,
        )
        .await;

        assert!(request_seq(first_ask) < ask_at_the_rebinds_join);
        assert_eq!(
            rebinds_among(&before_the_answering_member_took_the_ask),
            0,
            "the answering member began a rebind before the ask at the other's rebind's join reached it: {before_the_answering_member_took_the_ask:?}"
        );
        drop(scratch);
    }

    fn request_seq(payload: Option<Payload>) -> u64 {
        let Some(Payload::PresenceRequest(request)) = payload else {
            panic!("a presence request was expected");
        };
        request.seq
    }

    #[tokio::test(start_paused = true)]
    async fn spec_3_8_a_member_that_knows_no_other_node_never_rebinds_on_silence_or_at_its_join_timeout()
     {
        let scratch = Scratch::new("alone").await;
        let (record, mut events) = recorded_events();
        let started = Instant::now();

        let _member = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(record)],
        )
        .await;
        let (before_the_next_snapshot, _snapshot) = recorded_until(
            &mut events,
            STATE_COUNTERS,
            is_snapshot_after_the_start,
            INTERVAL_SNAPSHOT_WITHIN,
        )
        .await;

        assert_eq!(
            rebinds_among(&before_the_next_snapshot),
            0,
            "the member that knows no other node rebound: {before_the_next_snapshot:?}"
        );
        assert!(started.elapsed() >= SPEC_DEAF_TIMEOUT + REBIND_SLACK);
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
            .unwrap_or_else(WaitMissed::fail_the_test);
        let joined_through_the_peer = peer
            .first_neighbor_up_of(FIRST_SEED, IROH_DOCUMENTED_CLOSE_LATENCY + REBIND_SLACK)
            .await
            .unwrap_or_else(WaitMissed::fail_the_test);

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
        .unwrap_or_else(WaitMissed::fail_the_test);
        let dead_host = lookup_entry(&member.node.client.endpoint(), DEAD_HOST_SEED)
            .await
            .unwrap();

        rebound.unwrap_or_else(WaitMissed::fail_the_test);
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
    async fn observability_decision_9_a_rebind_records_each_member_lookup_entry_it_strips_of_a_relay_found_down_and_none_it_leaves_as_it_was()
     {
        let scratch = Scratch::new("strip-record").await;
        let (record, mut events) = recorded_events();
        let stripped_peer = QuietNode::answering(PEER_SEED, &[STRIPPED_RELAY]).await;
        let kept_peer = QuietNode::answering(KEPT_PEER_SEED, &[KEPT_RELAY]).await;
        let bare_peer = QuietNode::answering(BARE_PEER_SEED, &[]).await;
        let (member_endpoint, starting) = spawn_member(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_host_relays(&[
                    (LIVE_HOST_SEED, KEPT_RELAY),
                    (DEAD_HOST_SEED, STRIPPED_RELAY),
                ])
                .with_relay_probe(answering_only(KEPT_RELAY))
                .with_report(record),
        )
        .await;
        for peer in [&stripped_peer, &kept_peer, &bare_peer] {
            peer.introduce_to(&member_endpoint).await;
        }
        let _member = starting.await.unwrap();

        next_recorded(&mut events, REBINDING).await;
        next_recorded(&mut events, STATE_COUNTERS).await;
        let (between_the_snapshots, _at_the_fresh_bind) = recorded_until(
            &mut events,
            STATE_COUNTERS,
            |_fields| true,
            SNAPSHOT_AT_THE_REBIND_WITHIN,
        )
        .await;

        let replaced: Vec<&EventFields> = between_the_snapshots
            .iter()
            .filter(|(name, _fields)| *name == LOOKUP_REPLACED)
            .map(|(_name, fields)| fields)
            .collect();
        assert_eq!(replaced.len(), 1, "{between_the_snapshots:?}");
        assert_eq!(
            replaced[0].first(),
            Some(&("peer", short_peer_id(PEER_SEED)))
        );
        assert_eq!(
            replaced[0].last(),
            Some(&("relay", serde_json::Value::from(false)))
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
            .unwrap_or_else(WaitMissed::fail_the_test);
        let met = unreachable_member
            .first_neighbor_up_of(
                FIRST_SEED,
                IROH_DOCUMENTED_CLOSE_LATENCY
                    + SPEC_MEMBER_JOIN_WAIT
                    + SPEC_MEMBER_JOIN_INTERVAL
                    + REBIND_SLACK,
            )
            .await
            .unwrap_or_else(WaitMissed::fail_the_test);

        assert!(met.duration_since(rebound) >= SPEC_MEMBER_JOIN_WAIT);
        drop(scratch);
    }

    /// The longest the member takes to ask to join the members once its handed clock reads the member join wait past its rebind's join, which the runtime's timer would never reach on a clock an hour ahead.
    const MEMBER_JOIN_ASKED_ONCE_THE_CLOCK_READS_IT_WITHIN: Duration = Duration::from_secs(5);
    const NEIGHBOR_UP: &str = "gossip.neighbor_up";
    /// The longest the member takes to record a peer a gossip neighbor once that peer recorded the member one.
    const NEIGHBOR_UP_RECORDED_WITHIN: Duration =
        IROH_DOCUMENTED_CLOSE_LATENCY.saturating_add(REBIND_SLACK);
    const NEIGHBOR_DOWN: &str = "gossip.neighbor_down";
    /// The longest the member takes to record a gossip neighbor gone once that neighbor's endpoint closed.
    const NEIGHBOR_DOWN_RECORDED_WITHIN: Duration =
        IROH_DOCUMENTED_CLOSE_LATENCY.saturating_add(REBIND_SLACK);

    #[tokio::test]
    async fn spec_3_8_after_a_rebinds_join_the_member_asks_to_join_an_unreachable_member_once_its_handed_clock_reads_the_member_join_wait_then_one_member_join_interval_after_each_ask()
     {
        let scratch = Scratch::new("joins-on-clock").await;
        let host = QuietNode::start(HOST_SEED).await;
        let unreachable_member = QuietNode::start(PEER_SEED).await;
        let clock = StandingClock::standing_an_hour_ahead();
        let started = clock.now();
        let (record, mut events) = recorded_events();
        let running_host = [(HOST_SEED, host.addr())];

        let (mut members, ()) = tokio::join!(
            start_members_reaching(
                &scratch,
                vec![
                    MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                        .with_hosts(&[HOST_SEED])
                        .with_report(record)
                        .with_monotonic(clock.monotonic()),
                ],
                &running_host,
            ),
            async {
                joined_and_asked_for_presence(&mut events).await;
                clock.move_forward(SPEC_OBSERVER_WAIT);
            }
        );
        let member = members.pop().unwrap();
        unreachable_member.introduce_to(&member.endpoint).await;
        status_within(
            &member,
            PEER_SEED,
            |status| status == Status::Unreachable,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap_or_else(WaitMissed::fail_the_test);
        clock.move_forward(SPEC_DEAF_TIMEOUT);
        let rebound_at = clock.now();
        recorded_until(&mut events, REBINDING, |_fields| true, PAYLOAD_WAIT).await;
        joined_and_asked_for_presence(&mut events).await;
        let first_ask_due = later_by(clock.now(), SPEC_MEMBER_JOIN_WAIT);
        let mut asked = BTreeSet::from([
            later_by(started, SPEC_OBSERVER_WAIT),
            later_by(started, SPEC_JOIN_TIMEOUT),
            later_by(started, SPEC_DEAF_TIMEOUT),
            later_by(started, SPEC_STATE_SNAPSHOT_INTERVAL),
            later_by(rebound_at, SPEC_RELAY_PROBE_TIMEOUT),
            first_ask_due,
            later_by(rebound_at, SPEC_JOIN_TIMEOUT),
            later_by(rebound_at, SPEC_REBIND_INTERVAL),
            later_by(rebound_at, SPEC_STATE_SNAPSHOT_INTERVAL),
        ]);
        clock.asked_exactly("the member join wait", &asked).await;
        clock.move_late_past(first_ask_due);
        let first_ask_reading = clock.now();
        unreachable_member
            .first_neighbor_up_of(FIRST_SEED, MEMBER_JOIN_ASKED_ONCE_THE_CLOCK_READS_IT_WITHIN)
            .await
            .unwrap_or_else(|missed| {
                panic!("the unreachable member met no ask to join: {missed}, counted from the clock reading the member join wait")
            });
        recorded_until(
            &mut events,
            NEIGHBOR_UP,
            |fields| fields.contains(&(PEER, short_peer_id(PEER_SEED))),
            NEIGHBOR_UP_RECORDED_WITHIN,
        )
        .await;
        closed_within_the_documented_latency(unreachable_member.endpoint.endpoint().close()).await;
        recorded_until(
            &mut events,
            NEIGHBOR_DOWN,
            |fields| fields.contains(&(PEER, short_peer_id(PEER_SEED))),
            NEIGHBOR_DOWN_RECORDED_WITHIN,
        )
        .await;
        let second_ask_due = later_by(first_ask_reading, SPEC_MEMBER_JOIN_INTERVAL);
        asked.insert(second_ask_due);
        clock
            .asked_exactly(
                "the second ask, one member join interval after the first ask's reading",
                &asked,
            )
            .await;
        clock.move_late_past(second_ask_due);

        asked.insert(later_by(clock.now(), SPEC_MEMBER_JOIN_INTERVAL));
        clock
            .asked_exactly(
                "the third ask, one member join interval after the second ask's reading",
                &asked,
            )
            .await;
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_send_that_turns_its_unreachable_peer_online_lets_a_long_deaf_member_rebind_at_once()
     {
        let scratch = Scratch::new("reached").await;
        let (record, mut events) = recorded_events();
        let member = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(record)],
        )
        .await
        .pop()
        .unwrap();
        let peer = QuietNode::answering(PEER_SEED, &[]).await;
        peer.introduce_to(&member.endpoint).await;
        entry_meeting_within(
            &member,
            PEER_SEED,
            |listed| listed.attached == ATTACHED_ONLY_IN_THE_PEERS_ACK,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap_or_else(|missed| {
            panic!("the member never applied the ack of its own Introduce to the peer: {missed}")
        });
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
        .unwrap_or_else(WaitMissed::fail_the_test);
        let no_peer_online_since = Instant::now();
        let (while_no_peer_was_online, _snapshot) = recorded_until(
            &mut events,
            STATE_COUNTERS,
            is_snapshot_after_the_start,
            INTERVAL_SNAPSHOT_WITHIN,
        )
        .await;
        assert_eq!(
            rebinds_among(&while_no_peer_was_online),
            0,
            "the member rebound while no peer was online: {while_no_peer_was_online:?}"
        );
        assert!(no_peer_online_since.elapsed() >= SPEC_DEAF_TIMEOUT + REBIND_SLACK);

        member.node.send(PEER_HANDLE, SENT_BODY).await.unwrap();
        let reached = Instant::now();
        let (_before_the_rebind, _cause) =
            recorded_until(&mut events, REBINDING, |_fields| true, REBIND_SLACK).await;

        assert!(reached.elapsed() < REBIND_SLACK);
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
