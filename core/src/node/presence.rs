//! The presence half of the node of spec 3.8: init, the peer table fed by gossip and direct contact, and every announcement.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use iroh::endpoint::RelayStatus;
use iroh::protocol::Router;
use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey, Signature, TransportAddr, Watcher};
use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip};
use n0_future::{Stream, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use crate::config::{AcceptedValue, ConfigError, Host, HostOrigin, NetworkConfig};
use crate::crypto::NetworkKeys;
use crate::handle::{AnnouncedHandle, LocalHandle};
use crate::inbox::buffer::ReceivedMessage;
use crate::inbox::socket::{Attached, InboxSocket};
use crate::keys::parse_public_key;
use crate::net::direct::{ClientTimeouts, DirectClient, DirectError, DirectHost, DirectServer};
use crate::net::endpoint::{DIRECT_ALPN, NodeEndpoint};
use crate::net::gossip::{GossipError, GossipEvent, NetworkTopic};
use crate::payload::ZERO_MAC;
use crate::peers::{Effect, Peer, PeerTable};
use crate::wire::{
    self, Addr, Infrastructure, Introduce, IntroduceAck, Message, Nonce, Payload, Presence,
    SignatureBytes, Unreachable, Version,
};

const OBSERVER_WAIT: Duration = Duration::from_millis(3000);
const REANNOUNCE_INTERVAL: Duration = Duration::from_millis(2000);
const RELAY_LOSS_WAIT: Duration = Duration::from_millis(500);
const ANNOUNCEMENT_MAX_AGE_MS: u64 = 300_000;
const INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS: u64 = 300;
const MILLIS_PER_SECOND: u64 = 1000;
const UNSIGNED: SignatureBytes = SignatureBytes([0; Signature::LENGTH]);

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
type ProbeNonceSource = Arc<dyn Fn(&mut Nonce) + Send + Sync>;
type RunRecord = Arc<dyn Fn(NodeRecord) + Send + Sync>;
type Changes<T> = Pin<Box<dyn Stream<Item = T> + Send + Sync>>;

/// The card this node holds: its key, the handle it requests, and the public mapping it announces as JSON bytes.
pub struct Identity {
    pub card_key: SecretKey,
    pub requested_handle: AnnouncedHandle,
    pub public: Vec<u8>,
}

/// The network this node joins: its keys, its owner key, and its known hosts with the configuration file that keeps them.
pub struct Network {
    pub keys: NetworkKeys,
    pub owner: iroh::PublicKey,
    pub config: NetworkConfig,
    pub config_path: PathBuf,
}

/// The side effects the node runs on, initialized by the composition root.
pub struct NodeEffects {
    /// Milliseconds since the Unix epoch by this machine's clock.
    pub clock: Clock,
    /// A cryptographically secure source for the nonce of each probe.
    pub fill_probe_nonce: ProbeNonceSource,
    /// The run record, written to standard error by the command-line layer; it must not block.
    pub report: RunRecord,
    /// The inbox socket that started with the process.
    pub inbox: Arc<InboxSocket>,
}

/// A failure the node met while running, for the run record.
#[derive(Debug)]
pub enum NodeRecord {
    BroadcastFailed(GossipError),
    KnownHostsNotSaved(ConfigError),
    TopicFailed(GossipError),
    TopicClosed,
}

impl fmt::Display for NodeRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BroadcastFailed(source) => {
                write!(formatter, "a gossip broadcast failed: {source}")
            }
            Self::KnownHostsNotSaved(source) => {
                write!(formatter, "the known hosts were not saved: {source}")
            }
            Self::TopicFailed(source) => write!(formatter, "the gossip topic failed: {source}"),
            Self::TopicClosed => {
                formatter.write_str("the gossip topic closed, so this node announces nothing more")
            }
        }
    }
}

/// Why the node did not start.
#[derive(Debug)]
pub enum NodeError {
    KnownHostInvalid { id: String },
    Topic(GossipError),
    TopicClosedBeforeHandle,
}

impl fmt::Display for NodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KnownHostInvalid { id } => write!(
                formatter,
                "known host {id} needs a node id of {} and a relay of {}",
                AcceptedValue::HexKey,
                AcceptedValue::RelayUrl
            ),
            Self::Topic(source) => {
                write!(formatter, "cannot subscribe to the gossip topic: {source}")
            }
            Self::TopicClosedBeforeHandle => formatter
                .write_str("the gossip topic closed before this node assigned its own handle"),
        }
    }
}

impl std::error::Error for NodeError {}

/// A running member: joined to the topic, serving the direct protocol, and keeping its peer table by spec 3.8.
pub struct Node {
    state: Arc<NodeState>,
    own_handle: LocalHandle,
    _router: Router,
    presence_loop: JoinHandle<()>,
}

impl Node {
    /// Starts the direct listener and the topic on the endpoint, then returns once the node has assigned its own effective handle.
    ///
    /// # Errors
    /// `NodeError` when a known host is malformed or the topic closes before the handle is assigned.
    pub async fn start(
        endpoint: NodeEndpoint,
        identity: Identity,
        network: Network,
        effects: NodeEffects,
    ) -> Result<Self, NodeError> {
        let hosts = known_host_addresses(&network.config)?;
        seed_lookup_with_known_hosts(&endpoint, &hosts).await;
        let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());
        let bootstrap = hosts.iter().map(|host| host.id).collect();
        let topic = NetworkTopic::join(
            &gossip,
            network.keys.clone(),
            network.owner,
            bootstrap,
            Instant::now(),
        )
        .await
        .map_err(NodeError::Topic)?;
        let (to_loop, from_hosts) = mpsc::unbounded_channel();
        let state = Arc::new(NodeState::new(
            &identity,
            endpoint,
            &network.keys,
            &effects,
            to_loop,
        ));
        let router = Router::builder(state.endpoint.endpoint().clone())
            .accept(
                DIRECT_ALPN,
                DirectServer::new(network.keys, Arc::clone(&state)),
            )
            .accept(GOSSIP_ALPN, gossip)
            .spawn();
        let (assigned, own_handle) = oneshot::channel();
        let presence_loop = PresenceLoop {
            attached: effects.inbox.attached(),
            own_addresses: Box::pin(state.endpoint.endpoint().watch_addr().stream_updates_only()),
            relay_statuses: Box::pin(
                state
                    .endpoint
                    .endpoint()
                    .home_relay_status()
                    .stream_updates_only(),
            ),
            state: Arc::clone(&state),
            topic,
            from_hosts,
            requested_handle: identity.requested_handle,
            init: Init::AwaitingJoin { assigned },
            announcer: Announcer::default(),
            reannounce_due: None,
            relay_loss: RelayLossWatch::default(),
            known_hosts: KnownHosts {
                config: network.config,
                path: network.config_path,
            },
            fill_probe_nonce: effects.fill_probe_nonce,
            report: effects.report,
        };
        let presence_loop = tokio::spawn(presence_loop.run());
        let own_handle = own_handle
            .await
            .map_err(|_closed| NodeError::TopicClosedBeforeHandle)?;
        Ok(Self {
            state,
            own_handle,
            _router: router,
            presence_loop,
        })
    }

    /// The effective handle this node assigned itself at init.
    #[must_use]
    pub const fn own_handle(&self) -> &LocalHandle {
        &self.own_handle
    }

    /// Every other member this node knows now.
    #[must_use]
    pub fn peers(&self) -> Vec<Peer> {
        let now_ms = self.state.now_ms();
        self.state.table().peers(now_ms).cloned().collect()
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.presence_loop.abort();
    }
}

/// A known host's node id and relay, as the configuration names them.
struct KnownHostAddress {
    id: EndpointId,
    relay: RelayUrl,
}

/// The node id and relay of every host of the configuration, refusing a host whose id or relay does not parse.
fn known_host_addresses(config: &NetworkConfig) -> Result<Vec<KnownHostAddress>, NodeError> {
    config
        .hosts
        .iter()
        .map(|host| {
            let invalid = || NodeError::KnownHostInvalid {
                id: host.id.clone(),
            };
            Ok(KnownHostAddress {
                id: parse_public_key(&host.id).map_err(|_unparsed| invalid())?,
                relay: host.relay.parse().map_err(|_unparsed| invalid())?,
            })
        })
        .collect()
}

/// Gives each known host whose lookup entry holds no address its relay as its only address (spec 3.8).
async fn seed_lookup_with_known_hosts(endpoint: &NodeEndpoint, hosts: &[KnownHostAddress]) {
    for host in hosts {
        if !lookup_holds_address(endpoint.endpoint(), host.id).await {
            endpoint.replace_address(EndpointAddr::from_parts(
                host.id,
                [TransportAddr::Relay(host.relay.clone())],
            ));
        }
    }
}

/// Whether the endpoint's address lookup holds an entry for the node.
async fn lookup_holds_address(endpoint: &iroh::Endpoint, id: EndpointId) -> bool {
    let Ok(lookup) = endpoint.address_lookup() else {
        return false;
    };
    matches!(lookup.resolve(id).next().await, Some(Ok(Ok(_))))
}

/// What the direct server and the presence loop share: the identity, the peer table, the endpoint and the inbox.
struct NodeState {
    card_key: SecretKey,
    own_card: wire::PublicKey,
    public: Vec<u8>,
    own_handle: OnceLock<LocalHandle>,
    table: Mutex<PeerTable>,
    endpoint: NodeEndpoint,
    client: DirectClient,
    inbox: Arc<InboxSocket>,
    attached: Attached,
    clock: Clock,
    to_loop: mpsc::UnboundedSender<LoopEvent>,
}

impl NodeState {
    /// The shared state of a node that holds the identity's card on the endpoint.
    fn new(
        identity: &Identity,
        endpoint: NodeEndpoint,
        keys: &NetworkKeys,
        effects: &NodeEffects,
        to_loop: mpsc::UnboundedSender<LoopEvent>,
    ) -> Self {
        let own_card = *identity.card_key.public().as_bytes();
        let client = DirectClient::new(
            endpoint.endpoint().clone(),
            keys.clone(),
            ClientTimeouts::SPEC,
        );
        Self {
            card_key: identity.card_key.clone(),
            own_card,
            public: identity.public.clone(),
            own_handle: OnceLock::new(),
            table: Mutex::new(PeerTable::new(own_card)),
            endpoint,
            client,
            inbox: Arc::clone(&effects.inbox),
            attached: effects.inbox.attached(),
            clock: Arc::clone(&effects.clock),
            to_loop,
        }
    }

    /// Milliseconds since the Unix epoch by the clock the composition root handed in.
    fn now_ms(&self) -> u64 {
        (self.clock)()
    }

    /// The peer table, locked for one operation.
    fn table(&self) -> MutexGuard<'_, PeerTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The full current address of this node's endpoint, within the caps of spec 3.6.
    fn own_addr(&self) -> Addr {
        wire_addr(&self.endpoint.endpoint().addr())
    }

    /// This node's introduction: the effective handle once assigned, the public mapping, the attached flag and the address.
    fn introduction(&self) -> Introduce {
        Introduce {
            v: Version::CURRENT,
            handle: self
                .own_handle
                .get()
                .map(|handle| handle.as_str().to_owned()),
            public: self.public.clone(),
            attached: self.attached.is_attached(),
            addr: self.own_addr(),
            mac: ZERO_MAC,
        }
    }

    /// Replaces the card's lookup entry with the carried address.
    fn replace_lookup_entry(&self, card: &wire::PublicKey, addr: &Addr) {
        if let Some(id) = endpoint_id(card) {
            self.endpoint.replace_address(endpoint_addr(id, addr));
        }
    }

    /// Hands an event to the presence loop, which is gone only once the node is.
    fn hand_to_loop(&self, event: LoopEvent) {
        drop(self.to_loop.send(event));
    }

    /// Applies a validated Message to the table and returns the handle this node names its sender by, or none when its announced handle does not match.
    fn take_message(
        &self,
        sender: wire::PublicKey,
        message: &Message,
        now_ms: u64,
    ) -> Option<LocalHandle> {
        if message.handle.parse::<AnnouncedHandle>().is_err() {
            return None;
        }
        let mut table = self.table();
        let effects = table.receive_message(sender, message, now_ms);
        let from = table
            .peers(now_ms)
            .find(|peer| peer.card == sender)
            .map(|peer| peer.effective_handle.clone());
        drop(table);
        self.hand_to_loop(LoopEvent::Effects(effects));
        from
    }
}

impl DirectHost for Arc<NodeState> {
    fn is_stopping(&self) -> bool {
        false
    }

    fn own_card(&self) -> wire::PublicKey {
        self.own_card
    }

    fn is_attached(&self) -> bool {
        self.attached.is_attached()
    }

    fn receive_message(&self, sender: wire::PublicKey, message: Message) {
        let now_ms = self.now_ms();
        let Some(from) = self.take_message(sender, &message, now_ms) else {
            return;
        };
        self.inbox.publish(ReceivedMessage {
            id: uuid::Uuid::from_bytes(message.id).to_string(),
            from: from.as_str().to_owned(),
            body: message.body,
            sent_at: message.sent_at,
            received_at: now_ms,
        });
    }

    fn receive_introduce(&self, sender: wire::PublicKey, introduce: Introduce) -> IntroduceAck {
        let effects = self
            .table()
            .receive_introduce(sender, &introduce, self.now_ms());
        self.hand_to_loop(LoopEvent::Effects(effects));
        let introduction = self.introduction();
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

/// What reaches the presence loop from the direct server and from the requests it spawned.
enum LoopEvent {
    Effects(Vec<Effect>),
    IntroduceAnswered {
        card: wire::PublicKey,
        answer: Result<IntroduceAck, DirectError>,
    },
    ProbeAnswered {
        card: wire::PublicKey,
        acked: bool,
    },
}

/// A direct request the loop hands to a task, so no dial holds the loop.
enum DirectRequest {
    DropHeldConnection(wire::PublicKey),
    SendIntroduce(wire::PublicKey),
    SendProbe(wire::PublicKey, Nonce),
}

/// Runs the requests in their order, handing each answer back to the loop.
async fn run_requests(state: Arc<NodeState>, requests: Vec<DirectRequest>) {
    for request in requests {
        match request {
            DirectRequest::DropHeldConnection(card) => {
                if let Some(id) = endpoint_id(&card) {
                    state.client.drop_held_connection(id).await;
                }
            }
            DirectRequest::SendIntroduce(card) => {
                let answer = match endpoint_id(&card) {
                    Some(id) => state.client.introduce(id, state.introduction()).await,
                    None => Err(DirectError::Unreachable),
                };
                state.hand_to_loop(LoopEvent::IntroduceAnswered { card, answer });
            }
            DirectRequest::SendProbe(card, nonce) => {
                let acked = match endpoint_id(&card) {
                    Some(id) => state.client.probe(id, nonce).await.is_ok(),
                    None => false,
                };
                state.hand_to_loop(LoopEvent::ProbeAnswered { card, acked });
            }
        }
    }
}

/// Where init stands: awaiting the join, waiting the observer wait after it, or done.
enum Init {
    AwaitingJoin {
        assigned: oneshot::Sender<LocalHandle>,
    },
    ObserverWait {
        until: Instant,
        assigned: oneshot::Sender<LocalHandle>,
    },
    Done,
}

/// What woke the presence loop.
enum Wake {
    Topic(Option<Result<GossipEvent, GossipError>>),
    Loop(LoopEvent),
    AttachedChanged,
    InboxGone,
    OwnAddressChanged,
    HomeRelaysChanged(Vec<RelayStatus>),
    ObserverWaitOver,
    ReannounceDue,
    RelayLossDue,
}

/// The one task that owns the topic and performs every effect of the events/actions table of spec 3.8.
struct PresenceLoop {
    state: Arc<NodeState>,
    topic: NetworkTopic,
    from_hosts: mpsc::UnboundedReceiver<LoopEvent>,
    attached: Attached,
    own_addresses: Changes<EndpointAddr>,
    relay_statuses: Changes<Vec<RelayStatus>>,
    requested_handle: AnnouncedHandle,
    init: Init,
    announcer: Announcer,
    reannounce_due: Option<Instant>,
    relay_loss: RelayLossWatch,
    known_hosts: KnownHosts,
    fill_probe_nonce: ProbeNonceSource,
    report: RunRecord,
}

impl PresenceLoop {
    /// Handles every wake until the topic closes.
    async fn run(mut self) {
        let mut inbox_watched = true;
        loop {
            let wake = self.next_wake(inbox_watched).await;
            if matches!(wake, Wake::InboxGone) {
                inbox_watched = false;
            }
            if self.take(wake).await.is_break() {
                return;
            }
        }
    }

    /// Waits for the next event, change or deadline.
    async fn next_wake(&mut self, inbox_watched: bool) -> Wake {
        let observer_wait_over = match &self.init {
            Init::ObserverWait { until, .. } => Some(*until),
            Init::AwaitingJoin { .. } | Init::Done => None,
        };
        let relay_loss_due = self.relay_loss.next_deadline();
        tokio::select! {
            event = self.topic.next_event() => Wake::Topic(event),
            Some(event) = self.from_hosts.recv() => Wake::Loop(event),
            changed = self.attached.changed(), if inbox_watched => match changed {
                Ok(_) => Wake::AttachedChanged,
                Err(_) => Wake::InboxGone,
            },
            Some(_) = self.own_addresses.next() => Wake::OwnAddressChanged,
            Some(statuses) = self.relay_statuses.next() => Wake::HomeRelaysChanged(statuses),
            () = sleep_until_deadline(observer_wait_over) => Wake::ObserverWaitOver,
            () = sleep_until_deadline(self.reannounce_due) => Wake::ReannounceDue,
            () = sleep_until_deadline(relay_loss_due) => Wake::RelayLossDue,
        }
    }

    /// Acts on one wake, stopping once the topic has closed.
    async fn take(&mut self, wake: Wake) -> ControlFlow<()> {
        match wake {
            Wake::Topic(None) => {
                (self.report)(NodeRecord::TopicClosed);
                return ControlFlow::Break(());
            }
            Wake::Topic(Some(Err(failure))) => (self.report)(NodeRecord::TopicFailed(failure)),
            Wake::Topic(Some(Ok(event))) => self.take_gossip_event(event).await,
            Wake::Loop(event) => self.take_loop_event(event).await,
            Wake::AttachedChanged => self.announce_presence().await,
            Wake::InboxGone => {}
            Wake::OwnAddressChanged => self.reconsider_own_address().await,
            Wake::HomeRelaysChanged(statuses) => {
                self.relay_loss
                    .observe(&home_relays(&statuses), Instant::now());
                self.reconsider_own_address().await;
            }
            Wake::ObserverWaitOver => self.finish_observer_wait().await,
            Wake::ReannounceDue => {
                self.reannounce_due = None;
                self.reconsider_own_address().await;
            }
            Wake::RelayLossDue => self.remove_lost_relays().await,
        }
        ControlFlow::Continue(())
    }

    /// Applies the rows of spec 3.8 a topic event triggers: join, join timeout, neighbor up or down, payload.
    async fn take_gossip_event(&mut self, event: GossipEvent) {
        let now_ms = self.state.now_ms();
        match event {
            GossipEvent::Joined => self.take_joined().await,
            GossipEvent::JoinTimedOut => self.take_join_timeout(),
            GossipEvent::NeighborUp(node) => {
                let effects = self.state.table().neighbor_up(*node.as_bytes(), now_ms);
                self.perform(effects).await;
            }
            GossipEvent::NeighborDown(node) => {
                let effects = self.state.table().neighbor_down(*node.as_bytes(), now_ms);
                self.perform(effects).await;
            }
            GossipEvent::Payload(payload) => self.take_payload(*payload, now_ms).await,
        }
    }

    /// Starts the observer wait at the first join, and announces at every later join.
    async fn take_joined(&mut self) {
        match std::mem::replace(&mut self.init, Init::Done) {
            Init::AwaitingJoin { assigned } => {
                self.init = Init::ObserverWait {
                    until: Instant::now()
                        .checked_add(OBSERVER_WAIT)
                        .unwrap_or_else(Instant::now),
                    assigned,
                };
            }
            waiting @ Init::ObserverWait { .. } => self.init = waiting,
            Init::Done => self.announce_presence().await,
        }
    }

    /// Assigns the handle against the table as it stands when the first join gains no neighbor.
    fn take_join_timeout(&mut self) {
        match std::mem::replace(&mut self.init, Init::Done) {
            Init::AwaitingJoin { assigned } => self.assign_own_handle(assigned),
            waiting @ Init::ObserverWait { .. } => self.init = waiting,
            Init::Done => {}
        }
    }

    /// Assigns the handle and sends the first Presence once the observer wait is over.
    async fn finish_observer_wait(&mut self) {
        match std::mem::replace(&mut self.init, Init::Done) {
            Init::ObserverWait { assigned, .. } => {
                self.assign_own_handle(assigned);
                self.announce_presence().await;
            }
            awaiting @ Init::AwaitingJoin { .. } => self.init = awaiting,
            Init::Done => {}
        }
    }

    /// Assigns this node's effective handle against the table and hands it to init.
    fn assign_own_handle(&self, assigned: oneshot::Sender<LocalHandle>) {
        let own_handle = self
            .state
            .table()
            .assign_own_handle(&self.requested_handle, self.state.now_ms());
        drop(self.state.own_handle.set(own_handle.clone()));
        drop(assigned.send(own_handle));
    }

    /// Applies the row of spec 3.8 for a verified payload: Presence, Departure, Unreachable or Infrastructure.
    async fn take_payload(&mut self, payload: Payload, now_ms: u64) {
        match payload {
            Payload::Presence(presence) => {
                let effects = self.state.table().receive_presence(&presence, now_ms);
                self.perform(effects).await;
            }
            Payload::Departure(departure) => {
                self.state.table().receive_departure(&departure, now_ms);
            }
            Payload::Unreachable(report) if report.card == self.state.own_card => {
                if is_rumor_to_answer(report.ts, self.announcer.last_seq(), now_ms) {
                    self.announce_presence().await;
                }
            }
            Payload::Unreachable(report) => {
                self.state.table().receive_unreachable(&report, now_ms);
            }
            Payload::Infrastructure(record) => self.take_infrastructure(&record, now_ms).await,
        }
    }

    /// Applies the Infrastructure rows of spec 3.8: hold a newer record, point the lookup at it, add its relay, save a new host.
    async fn take_infrastructure(&mut self, record: &Infrastructure, now_ms: u64) {
        let verdict = self.known_hosts.take_record(record, now_ms);
        if verdict == RecordVerdict::Dropped {
            return;
        }
        self.state.replace_lookup_entry(&record.host, &record.addr);
        if let Ok(relay) = record.relay.as_str().parse::<RelayUrl>() {
            self.state.endpoint.add_relay(relay).await;
        }
        if verdict == RecordVerdict::FromNewHost
            && let Err(failure) = self.known_hosts.save(now_ms)
        {
            (self.report)(NodeRecord::KnownHostsNotSaved(failure));
        }
    }

    /// Applies the effects the direct server handed over and the answer of each request this node sent.
    async fn take_loop_event(&self, event: LoopEvent) {
        let now_ms = self.state.now_ms();
        match event {
            LoopEvent::Effects(effects) => self.perform(effects).await,
            LoopEvent::IntroduceAnswered {
                card,
                answer: Ok(ack),
            } => {
                let effects = self.state.table().receive_introduce_ack(card, &ack, now_ms);
                self.perform(sparing_the_answering_connection(effects))
                    .await;
            }
            LoopEvent::IntroduceAnswered {
                card,
                answer: Err(_),
            }
            | LoopEvent::ProbeAnswered { card, acked: false } => {
                let effects = self.state.table().direct_contact_failed(card, now_ms);
                self.perform(effects).await;
            }
            LoopEvent::ProbeAnswered { card, acked: true } => {
                self.state.table().direct_contact_succeeded(card, now_ms);
            }
        }
    }

    /// Performs the effects: lookup and broadcast here, direct requests in one task in their order.
    async fn perform(&self, effects: Vec<Effect>) {
        let mut requests = Vec::new();
        for effect in effects {
            match effect {
                Effect::ReplaceLookupEntry { card, addr } => {
                    self.state.replace_lookup_entry(&card, &addr);
                }
                Effect::BroadcastUnreachable(card) => self.broadcast_unreachable(card).await,
                Effect::DropHeldConnection(card) => {
                    requests.push(DirectRequest::DropHeldConnection(card));
                }
                Effect::SendIntroduce(card) => requests.push(DirectRequest::SendIntroduce(card)),
                Effect::SendProbe(card) => {
                    let mut nonce = Nonce::default();
                    (self.fill_probe_nonce)(&mut nonce);
                    requests.push(DirectRequest::SendProbe(card, nonce));
                }
            }
        }
        if !requests.is_empty() {
            tokio::spawn(run_requests(Arc::clone(&self.state), requests));
        }
    }

    /// Broadcasts this node's Presence with the next seq, once its handle is assigned.
    async fn announce_presence(&mut self) {
        let Some(own_handle) = self.state.own_handle.get() else {
            return;
        };
        let addr = self.state.own_addr();
        let seq = self.announcer.next_seq(self.state.now_ms());
        let presence = Payload::Presence(Presence {
            v: Version::CURRENT,
            card: self.state.own_card,
            handle: own_handle.as_str().to_owned(),
            public: self.state.public.clone(),
            attached: self.state.attached.is_attached(),
            addr: addr.clone(),
            seq,
            mac: ZERO_MAC,
            sig: UNSIGNED,
        });
        self.announcer.record(seq, addr, Instant::now());
        self.broadcast(presence).await;
    }

    /// Broadcasts that the card is unreachable, as observed now by this node.
    async fn broadcast_unreachable(&self, card: wire::PublicKey) {
        let report = Payload::Unreachable(Unreachable {
            v: Version::CURRENT,
            card,
            reporter: self.state.own_card,
            ts: self.state.now_ms(),
            mac: ZERO_MAC,
            sig: UNSIGNED,
        });
        self.broadcast(report).await;
    }

    /// Signs and broadcasts the payload under the card key, writing a failure to the run record.
    async fn broadcast(&self, payload: Payload) {
        if let Err(failure) = self.topic.broadcast(&self.state.card_key, payload).await {
            (self.report)(NodeRecord::BroadcastFailed(failure));
        }
    }

    /// Announces a changed address now, or once the re-announce interval since the last Presence has passed.
    async fn reconsider_own_address(&mut self) {
        match self
            .announcer
            .reannounce_step(&self.state.own_addr(), Instant::now())
        {
            ReannounceStep::Unchanged => {}
            ReannounceStep::AnnounceNow => self.announce_presence().await,
            ReannounceStep::AnnounceAt(due) => self.reannounce_due = Some(due),
        }
    }

    /// Removes every home relay still disconnected the relay-loss wait after it was lost.
    async fn remove_lost_relays(&mut self) {
        let statuses = self.state.endpoint.endpoint().home_relay_status().get();
        for relay in self
            .relay_loss
            .lost_relays(&home_relays(&statuses), Instant::now())
        {
            self.state.endpoint.remove_relay(&relay).await;
        }
    }
}

/// Sleeps until the deadline, or forever when there is none.
async fn sleep_until_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

/// The effects of an `IntroduceAck` less the drop of the held connection, since the ack came on that connection (spec 3.8).
fn sparing_the_answering_connection(effects: Vec<Effect>) -> Vec<Effect> {
    effects
        .into_iter()
        .filter(|effect| !matches!(effect, Effect::DropHeldConnection(_)))
        .collect()
}

/// Whether an Unreachable report about this node calls for a new Presence: within the announcement max age and not below the last Presence's seq.
fn is_rumor_to_answer(report_ts: u64, last_presence_seq: Option<u64>, now_ms: u64) -> bool {
    report_ts >= now_ms.saturating_sub(ANNOUNCEMENT_MAX_AGE_MS)
        && last_presence_seq.is_none_or(|seq| report_ts >= seq)
}

/// The last Presence this node sent: its seq, its address, and when it went out.
struct Announced {
    seq: u64,
    addr: Addr,
    at: Instant,
}

/// What a change of this node's address calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReannounceStep {
    Unchanged,
    AnnounceNow,
    AnnounceAt(Instant),
}

/// The seq and address of this node's announcements.
#[derive(Default)]
struct Announcer {
    last: Option<Announced>,
}

impl Announcer {
    /// The seq of the next Presence: `max(now_ms, last_seq + 1)`.
    fn next_seq(&self, now_ms: u64) -> u64 {
        self.last
            .as_ref()
            .map_or(now_ms, |last| now_ms.max(last.seq.saturating_add(1)))
    }

    /// The seq of the last Presence this node sent, if any.
    fn last_seq(&self) -> Option<u64> {
        self.last.as_ref().map(|last| last.seq)
    }

    /// Remembers the Presence just sent as the last one.
    fn record(&mut self, seq: u64, addr: Addr, at: Instant) {
        self.last = Some(Announced { seq, addr, at });
    }

    /// Whether the current address differs from the last announced one, and when it may be announced.
    fn reannounce_step(&self, current: &Addr, now: Instant) -> ReannounceStep {
        let Some(last) = &self.last else {
            return ReannounceStep::Unchanged;
        };
        if last.addr == *current {
            return ReannounceStep::Unchanged;
        }
        let earliest = last.at.checked_add(REANNOUNCE_INTERVAL).unwrap_or(last.at);
        if now >= earliest {
            ReannounceStep::AnnounceNow
        } else {
            ReannounceStep::AnnounceAt(earliest)
        }
    }
}

/// One home relay and whether the endpoint is connected to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HomeRelay {
    url: RelayUrl,
    connected: bool,
}

/// Each home relay iroh reports, with whether the endpoint is connected to it.
fn home_relays(statuses: &[RelayStatus]) -> Vec<HomeRelay> {
    statuses
        .iter()
        .map(|status| HomeRelay {
            url: status.url().clone(),
            connected: status.is_connected(),
        })
        .collect()
}

/// When each lost home relay is due for removal, the relay-loss wait after its loss.
#[derive(Default)]
struct RelayLossWatch {
    due: BTreeMap<RelayUrl, Instant>,
}

impl RelayLossWatch {
    /// Starts the wait for each newly lost home relay and forgets each relay that is connected again or no longer home.
    fn observe(&mut self, home: &[HomeRelay], now: Instant) {
        self.due.retain(|url, _| {
            home.iter()
                .any(|relay| relay.url == *url && !relay.connected)
        });
        for relay in home.iter().filter(|relay| !relay.connected) {
            self.due
                .entry(relay.url.clone())
                .or_insert_with(|| now.checked_add(RELAY_LOSS_WAIT).unwrap_or(now));
        }
    }

    /// The earliest moment a lost home relay is due for removal.
    fn next_deadline(&self) -> Option<Instant> {
        self.due.values().min().copied()
    }

    /// The home relays still disconnected whose wait is over at `now`, forgotten once returned.
    fn lost_relays(&mut self, home: &[HomeRelay], now: Instant) -> Vec<RelayUrl> {
        self.observe(home, now);
        let lost: Vec<RelayUrl> = self
            .due
            .iter()
            .filter(|(_, due)| **due <= now)
            .map(|(url, _)| url.clone())
            .collect();
        for url in &lost {
            self.due.remove(url);
        }
        lost
    }
}

/// What a valid Infrastructure record does to the known hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordVerdict {
    Dropped,
    FromKnownHost,
    FromNewHost,
}

/// The known hosts of spec 3.8, with the newest record `ts` held for each and the file that keeps them.
struct KnownHosts {
    config: NetworkConfig,
    path: PathBuf,
}

impl KnownHosts {
    /// Holds a record newer than the newest held for its host and within the future bound, adding a host never known before.
    fn take_record(&mut self, record: &Infrastructure, now_ms: u64) -> RecordVerdict {
        let latest_accepted_ts =
            seconds_of(now_ms).saturating_add(INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS);
        if record.ts > latest_accepted_ts {
            return RecordVerdict::Dropped;
        }
        let id = data_encoding::HEXLOWER.encode(&record.host);
        let Some(host) = self.config.hosts.iter_mut().find(|host| host.id == id) else {
            self.config.hosts.push(Host {
                id,
                relay: record.relay.as_str().to_owned(),
                ts: Some(record.ts),
                origin: HostOrigin::Learned { heard: now_ms },
            });
            return RecordVerdict::FromNewHost;
        };
        if host.ts.is_some_and(|held| record.ts <= held) {
            return RecordVerdict::Dropped;
        }
        host.ts = Some(record.ts);
        if let HostOrigin::Learned { heard } = &mut host.origin {
            *heard = now_ms;
        }
        RecordVerdict::FromKnownHost
    }

    /// Rewrites the network configuration with the known hosts (spec 3.3).
    fn save(&self, now_ms: u64) -> Result<(), ConfigError> {
        self.config
            .rewrite(&self.path, seconds_of(now_ms))
            .map(drop)
    }
}

/// Whole seconds in a span of milliseconds.
const fn seconds_of(milliseconds: u64) -> u64 {
    milliseconds / MILLIS_PER_SECOND
}

/// The card as an iroh node id, or none when its bytes are not a valid key.
fn endpoint_id(card: &wire::PublicKey) -> Option<EndpointId> {
    EndpointId::from_bytes(card).ok()
}

/// The wire form of an endpoint address, within the caps of spec 3.6.
fn wire_addr(addr: &EndpointAddr) -> Addr {
    Addr::from_endpoint_report(
        addr.relay_urls().map(ToString::to_string),
        addr.ip_addrs().copied(),
    )
}

/// The endpoint address of the node at a carried wire address, leaving out relay URLs that do not parse.
fn endpoint_addr(id: EndpointId, addr: &Addr) -> EndpointAddr {
    let relays = addr
        .relays()
        .iter()
        .filter_map(|relay| relay.as_str().parse().ok())
        .map(TransportAddr::Relay);
    let direct = addr.direct().iter().copied().map(TransportAddr::Ip);
    EndpointAddr::from_parts(id, relays.chain(direct))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use iroh::SecretKey;

    use super::*;
    use crate::config::{Host, HostOrigin, NetworkConfig};
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use crate::dirs::{Directories, OperatingSystemFilesystem};
    use crate::handle::AnnouncedHandle;
    use crate::inbox::socket::{InboxSocket, ProcessTable};
    use crate::keys::render_public_key;
    use crate::net::endpoint::NodeEndpoint;
    use crate::peers::Status;

    const NETWORK_SECRET: [u8; KEY_LEN] = [33; KEY_LEN];
    const OWNER_SEED: u8 = 60;
    const FIRST_SEED: u8 = 61;
    const SECOND_SEED: u8 = 62;
    const FIRST_HANDLE: &str = "amy";
    const SECOND_HANDLE: &str = "bob";
    const FIRST_PUBLIC: &[u8] = br#"{"role":"first"}"#;
    const SECOND_PUBLIC: &[u8] = br#"{"role":"second"}"#;
    const UNREACHABLE_RELAY: &str = "https://relay.estafette.invalid/";
    const SPEC_JOIN_TIMEOUT_PLUS_OBSERVER_WAIT: Duration = Duration::from_millis(8000);
    const NETWORK_FILE_NAME: &str = "network.toml";
    const SPEC_OBSERVER_WAIT: Duration = Duration::from_millis(3000);
    const LISTING_POLL: Duration = Duration::from_millis(20);
    const PROBE_NONCE: crate::wire::Nonce = [7; 16];
    const NODE_TESTS_AT_ONCE: usize = 2;
    const INBOX_FRAME_WAIT: Duration = Duration::from_secs(2);
    const MESSAGE_ID: crate::wire::MessageId = [
        0x01, 0x8f, 0x3a, 0x7c, 0x00, 0x00, 0x70, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x2a,
    ];
    const MESSAGE_ID_TEXT: &str = "018f3a7c-0000-7000-8000-00000000002a";
    const MESSAGE_SENT_AT: u64 = 1_700_000_123_456;
    const MESSAGE_BODY: &str = "a message before any presence";
    const INTRODUCE_ROUND_TRIP: Duration = Duration::from_millis(1500);
    const THIRD_SEED: u8 = 63;
    const THIRD_HANDLE: &str = "cal";
    const THIRD_PUBLIC: &[u8] = br#"{"role":"third"}"#;
    const SECOND_EFFECTIVE_FIRST_HANDLE: &str = "amy-2";
    const WITNESS_SEED: u8 = 70;
    const WITNESS_PUBLIC: &[u8] = br#"{"role":"witness"}"#;
    const WITNESS_JOIN_WAIT: Duration = Duration::from_secs(8);
    const MARKER_SEED: u8 = 80;
    const SECOND_MARKER_SEED: u8 = 81;
    const MARKER_HANDLE: &str = "mark";
    const MARKER_PUBLIC: &[u8] = br#"{"role":"marker"}"#;
    const REPLAYED_PUBLIC: &[u8] = br#"{"role":"replayed"}"#;
    const PAYLOAD_WAIT: Duration = Duration::from_secs(3);
    const NOT_AN_ANNOUNCED_HANDLE: &str = "Bob";
    const LATER_MESSAGE_BODY: &str = "a message under the announced handle";
    const MISMATCHED_MESSAGE_BODY: &str = "a message under a handle that does not match";
    const IMPOSTOR_HANDLE: &str = "imp";
    const HOST_SEED: u8 = 90;
    const SECOND_HOST_SEED: u8 = 91;
    const THIRD_HOST_SEED: u8 = 92;
    const HOST_RELAY: &str = "https://relay-host.estafette.invalid/";
    const SECOND_HOST_RELAY: &str = "https://relay-second-host.estafette.invalid/";
    const THIRD_HOST_RELAY: &str = "https://relay-third-host.estafette.invalid/";
    const SPEC_HOST_ADDED_WITHIN: Duration = Duration::from_millis(1000);
    const SPEC_INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS: u64 = 300;
    const SPEC_ANNOUNCEMENT_MAX_AGE_MS: u64 = 300_000;
    const SPEC_JOIN_TIMEOUT: Duration = Duration::from_millis(5000);
    const FROZEN_CLOCK_MS: u64 = 1_800_000_000_000;
    const CLOCK_AFTER_MAX_AGE_MS: u64 = FROZEN_CLOCK_MS + 400_000;
    const STALE_RUMOR_TS: u64 = CLOCK_AFTER_MAX_AGE_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS - 1;
    const FRESH_RUMOR_TS: u64 = CLOCK_AFTER_MAX_AGE_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS;
    const NOTHING_MORE_WAIT: Duration = Duration::from_millis(1000);
    const WITNESS_HANDLE: &str = "xen";
    const SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT: Duration = Duration::from_millis(5000);
    const PROBE_ANSWERED_WAIT: Duration = Duration::from_secs(5);
    const AFTER_PROBE_ACK: Duration = Duration::from_millis(300);

    fn network_secret() -> NetworkSecret {
        NetworkSecret::from_bytes(NETWORK_SECRET)
    }

    fn keys() -> NetworkKeys {
        NetworkKeys::derive(&network_secret())
    }

    fn secret_key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; KEY_LEN])
    }

    fn card(seed: u8) -> crate::wire::PublicKey {
        *secret_key(seed).public().as_bytes()
    }

    fn wall_clock_ms() -> u64 {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap()
    }

    struct EveryProcessRuns;

    impl ProcessTable for EveryProcessRuns {
        fn is_running(&self, _process_id: u32) -> std::io::Result<bool> {
            Ok(true)
        }
    }

    static NODE_TEST_SLOTS: tokio::sync::Semaphore =
        tokio::sync::Semaphore::const_new(NODE_TESTS_AT_ONCE);

    struct Scratch {
        root: PathBuf,
        directories: Directories,
        _slot: tokio::sync::SemaphorePermit<'static>,
    }

    impl Scratch {
        async fn new(tag: &str) -> Self {
            let slot = NODE_TEST_SLOTS.acquire().await.unwrap();
            let root = std::env::temp_dir().join(format!("u20-{tag}-{}", std::process::id()));
            drop(fs::remove_dir_all(&root));
            fs::create_dir_all(&root).unwrap();
            let base = root.clone();
            let environment = move |name: &str| match name {
                "XDG_CONFIG_HOME" => Some(base.join("c").into_os_string()),
                "XDG_DATA_HOME" => Some(base.join("d").into_os_string()),
                "XDG_RUNTIME_DIR" => Some(base.join("r").into_os_string()),
                _ => None::<OsString>,
            };
            let directories =
                Directories::establish(environment, &OperatingSystemFilesystem, None).unwrap();
            Self {
                root,
                directories,
                _slot: slot,
            }
        }

        fn network_file(&self, seed: u8) -> PathBuf {
            self.root.join(format!("{seed}-{NETWORK_FILE_NAME}"))
        }

        fn inbox(&self, seed: u8) -> Arc<InboxSocket> {
            Arc::new(
                InboxSocket::start(&self.directories, u32::from(seed), &EveryProcessRuns).unwrap(),
            )
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.root));
        }
    }

    async fn bind(seed: u8) -> NodeEndpoint {
        NodeEndpoint::bind(
            secret_key(seed),
            *keys().gate_key(),
            Vec::new(),
            move |nonce: &mut [u8; KEY_LEN]| *nonce = [seed; KEY_LEN],
            |_ignored| {},
        )
        .await
        .unwrap()
    }

    fn configured_host(seed: u8) -> Host {
        Host {
            id: render_public_key(&secret_key(seed).public()),
            relay: UNREACHABLE_RELAY.to_owned(),
            ts: None,
            origin: HostOrigin::Configured,
        }
    }

    fn config_with_hosts(hosts: Vec<Host>) -> NetworkConfig {
        NetworkConfig {
            secret: data_encoding::HEXLOWER.encode(&NETWORK_SECRET),
            owner: render_public_key(&secret_key(OWNER_SEED).public()),
            hosts,
        }
    }

    struct MemberPlan {
        seed: u8,
        handle: &'static str,
        public: &'static [u8],
        hosts: Vec<u8>,
        reachable: Vec<u8>,
        clock: Clock,
    }

    impl MemberPlan {
        fn new(seed: u8, handle: &'static str, public: &'static [u8]) -> Self {
            Self {
                seed,
                handle,
                public,
                hosts: Vec::new(),
                reachable: Vec::new(),
                clock: Arc::new(wall_clock_ms),
            }
        }

        fn with_hosts(self, hosts: &[u8]) -> Self {
            Self {
                hosts: hosts.to_vec(),
                ..self
            }
        }

        fn reaching(self, reachable: &[u8]) -> Self {
            Self {
                reachable: reachable.to_vec(),
                ..self
            }
        }
    }

    struct Member {
        node: Node,
        endpoint: iroh::Endpoint,
        inbox_path: PathBuf,
        config_path: PathBuf,
    }

    async fn start_members(scratch: &Scratch, plans: Vec<MemberPlan>) -> Vec<Member> {
        start_members_reaching(scratch, plans, &[]).await
    }

    async fn start_members_reaching(
        scratch: &Scratch,
        plans: Vec<MemberPlan>,
        running: &[(u8, iroh::EndpointAddr)],
    ) -> Vec<Member> {
        let mut endpoints = Vec::new();
        for plan in &plans {
            endpoints.push(bind(plan.seed).await);
        }
        let addresses: Vec<(u8, iroh::EndpointAddr)> = plans
            .iter()
            .zip(&endpoints)
            .map(|(plan, endpoint)| (plan.seed, endpoint.endpoint().addr()))
            .chain(running.iter().cloned())
            .collect();
        for (plan, endpoint) in plans.iter().zip(&endpoints) {
            for (seed, address) in &addresses {
                if plan.hosts.contains(seed) || plan.reachable.contains(seed) {
                    endpoint.replace_address(address.clone());
                }
            }
        }
        let starting: Vec<_> = plans
            .into_iter()
            .zip(endpoints)
            .map(|(plan, endpoint)| {
                let inbox = scratch.inbox(plan.seed);
                let inbox_path = scratch.directories.inbox_socket_file(u32::from(plan.seed));
                let config_path = scratch.network_file(plan.seed);
                tokio::spawn(start_member(
                    plan,
                    endpoint,
                    (inbox, inbox_path),
                    config_path,
                ))
            })
            .collect();
        let mut members = Vec::new();
        for member in starting {
            members.push(member.await.unwrap());
        }
        members
    }

    async fn start_member(
        plan: MemberPlan,
        node_endpoint: NodeEndpoint,
        (inbox, inbox_path): (Arc<InboxSocket>, PathBuf),
        config_path: PathBuf,
    ) -> Member {
        let endpoint = node_endpoint.endpoint().clone();
        let node = Node::start(
            node_endpoint,
            Identity {
                card_key: secret_key(plan.seed),
                requested_handle: plan.handle.parse::<AnnouncedHandle>().unwrap(),
                public: plan.public.to_vec(),
            },
            Network {
                keys: keys(),
                owner: secret_key(OWNER_SEED).public(),
                config: config_with_hosts(
                    plan.hosts
                        .iter()
                        .map(|&seed| configured_host(seed))
                        .collect(),
                ),
                config_path: config_path.clone(),
            },
            NodeEffects {
                clock: plan.clock,
                fill_probe_nonce: Arc::new(|nonce: &mut crate::wire::Nonce| *nonce = PROBE_NONCE),
                report: Arc::new(|_record: NodeRecord| {}),
                inbox,
            },
        )
        .await
        .unwrap();
        Member {
            node,
            endpoint,
            inbox_path,
            config_path,
        }
    }

    async fn start_pair(scratch: &Scratch) -> (Member, Member) {
        let mut members = start_members(
            scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_hosts(&[SECOND_SEED]),
                MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC)
                    .with_hosts(&[FIRST_SEED]),
            ],
        )
        .await;
        let second = members.pop().unwrap();
        let first = members.pop().unwrap();
        (first, second)
    }

    async fn listed_within(member: &Member, seed: u8, within: Duration) -> Option<Peer> {
        tokio::time::timeout(within, async {
            loop {
                if let Some(peer) = member
                    .node
                    .peers()
                    .into_iter()
                    .find(|peer| peer.card == card(seed))
                {
                    return peer;
                }
                tokio::time::sleep(LISTING_POLL).await;
            }
        })
        .await
        .ok()
    }

    #[tokio::test]
    async fn spec_3_8_members_joined_through_their_known_hosts_list_each_other_online_after_init() {
        let scratch = Scratch::new("pair").await;
        let started = tokio::time::Instant::now();

        let (first, second) = start_pair(&scratch).await;

        assert!(started.elapsed() < SPEC_JOIN_TIMEOUT_PLUS_OBSERVER_WAIT);
        assert_eq!(first.node.own_handle().as_str(), FIRST_HANDLE);
        assert_eq!(second.node.own_handle().as_str(), SECOND_HANDLE);
        let seen_by_first = listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        let seen_by_second = listed_within(&second, FIRST_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        assert_eq!(seen_by_first.effective_handle.as_str(), SECOND_HANDLE);
        assert_eq!(seen_by_first.public.as_deref(), Some(SECOND_PUBLIC));
        assert_eq!(seen_by_first.status, Status::Online);
        assert_eq!(seen_by_second.effective_handle.as_str(), FIRST_HANDLE);
        assert_eq!(seen_by_second.public.as_deref(), Some(FIRST_PUBLIC));
        assert_eq!(seen_by_second.status, Status::Online);
        drop(scratch);
    }

    async fn inbox_frames(member: &Member, count: usize) -> Vec<serde_json::Value> {
        let socket_path = &member.inbox_path;
        let mut stream = tokio::net::UnixStream::connect(socket_path).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"{\"since\": 0}\n")
            .await
            .unwrap();
        let mut reader = tokio::io::BufReader::new(stream);
        let mut frames = Vec::new();
        for _ in 0..count {
            let mut line = String::new();
            tokio::time::timeout(
                INBOX_FRAME_WAIT,
                tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line),
            )
            .await
            .unwrap()
            .unwrap();
            frames.push(serde_json::from_str(&line).unwrap());
        }
        frames
    }

    fn message_to(seed: u8, handle: &str) -> Message {
        Message {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            to: card(seed),
            handle: handle.to_owned(),
            attached: true,
            sent_at: MESSAGE_SENT_AT,
            body: MESSAGE_BODY.to_owned(),
            mac: ZERO_MAC,
        }
    }

    #[tokio::test]
    async fn test_8_message_from_a_card_whose_presence_was_never_seen_is_accepted_and_lists_the_sender_online()
     {
        let scratch = Scratch::new("t8").await;
        let mut members = start_members(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
                MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC).reaching(&[FIRST_SEED]),
            ],
        )
        .await;
        let second = members.pop().unwrap();
        let first = members.pop().unwrap();
        assert_eq!(first.node.peers(), Vec::<Peer>::new());
        let sender = DirectClient::new(second.endpoint.clone(), keys(), ClientTimeouts::SPEC);

        let ack = sender
            .send_message(first.endpoint.id(), message_to(FIRST_SEED, SECOND_HANDLE))
            .await
            .unwrap();

        assert!(ack.accepted);
        let listed = listed_within(&first, SECOND_SEED, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(listed.effective_handle.as_str(), SECOND_HANDLE);
        assert_eq!(listed.status, Status::Online);
        assert!(listed.attached);
        let frame = inbox_frames(&first, 1).await.remove(0);
        assert_eq!(frame["from"], SECOND_HANDLE);
        assert_eq!(frame["body"], MESSAGE_BODY);
        assert_eq!(frame["id"], MESSAGE_ID_TEXT);
        assert_eq!(frame["sent_at"], MESSAGE_SENT_AT);
        tokio::time::sleep(INTRODUCE_ROUND_TRIP).await;
        let after_introduce = listed_within(&first, SECOND_SEED, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(after_introduce.status, Status::Online);
        assert_eq!(after_introduce.public.as_deref(), Some(SECOND_PUBLIC));
        drop(scratch);
    }

    struct WitnessHost {
        own_card: crate::wire::PublicKey,
        handle: Option<&'static str>,
        addr: Addr,
        probes_answered: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[derive(Debug)]
    struct CountingConnections {
        server: DirectServer<WitnessHost>,
        connections: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl iroh::protocol::ProtocolHandler for CountingConnections {
        async fn accept(
            &self,
            connection: iroh::endpoint::Connection,
        ) -> Result<(), iroh::protocol::AcceptError> {
            self.connections
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.server.accept(connection).await
        }
    }

    impl DirectHost for WitnessHost {
        fn is_stopping(&self) -> bool {
            false
        }

        fn own_card(&self) -> crate::wire::PublicKey {
            self.own_card
        }

        fn is_attached(&self) -> bool {
            self.probes_answered
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            false
        }

        fn receive_message(&self, _sender: crate::wire::PublicKey, _message: Message) {}

        fn receive_introduce(
            &self,
            _sender: crate::wire::PublicKey,
            _introduce: Introduce,
        ) -> IntroduceAck {
            IntroduceAck {
                v: Version::CURRENT,
                handle: self.handle.map(str::to_owned),
                public: WITNESS_PUBLIC.to_vec(),
                attached: false,
                addr: self.addr.clone(),
                mac: ZERO_MAC,
            }
        }
    }

    struct Witness {
        gossip: Gossip,
        topic: NetworkTopic,
        probes_answered: Arc<std::sync::atomic::AtomicUsize>,
        direct_connections: Arc<std::sync::atomic::AtomicUsize>,
        _router: Router,
    }

    impl Witness {
        async fn joined_through(
            seed: u8,
            handle: Option<&'static str>,
            through: &iroh::Endpoint,
        ) -> Self {
            let node_endpoint = bind(seed).await;
            node_endpoint.replace_address(through.addr());
            let gossip = Gossip::builder().spawn(node_endpoint.endpoint().clone());
            let probes_answered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let host = WitnessHost {
                own_card: card(seed),
                handle,
                addr: wire_addr(&node_endpoint.endpoint().addr()),
                probes_answered: Arc::clone(&probes_answered),
            };
            let direct_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let counting = CountingConnections {
                server: DirectServer::new(keys(), host),
                connections: Arc::clone(&direct_connections),
            };
            let router = Router::builder(node_endpoint.endpoint().clone())
                .accept(DIRECT_ALPN, counting)
                .accept(GOSSIP_ALPN, gossip.clone())
                .spawn();
            let mut topic = NetworkTopic::join(
                &gossip,
                keys(),
                secret_key(OWNER_SEED).public(),
                vec![through.id()],
                Instant::now(),
            )
            .await
            .unwrap();
            tokio::time::timeout(WITNESS_JOIN_WAIT, async {
                while topic.next_event().await.unwrap().unwrap() != GossipEvent::Joined {}
            })
            .await
            .unwrap();
            Self {
                gossip,
                topic,
                probes_answered,
                direct_connections,
                _router: router,
            }
        }

        async fn broadcast_as(&self, signer_seed: u8, payload: Payload) {
            self.topic
                .broadcast(&secret_key(signer_seed), payload)
                .await
                .unwrap();
        }

        async fn payload_where(
            &mut self,
            within: Duration,
            mut pick: impl FnMut(&Payload) -> bool + Send,
        ) -> Option<Payload> {
            tokio::time::timeout(within, async {
                loop {
                    if let Some(Ok(GossipEvent::Payload(payload))) = self.topic.next_event().await
                        && pick(&payload)
                    {
                        return *payload;
                    }
                }
            })
            .await
            .ok()
        }
    }

    fn presence_of(seed: u8, handle: &str, public: &[u8], seq: u64) -> Payload {
        Payload::Presence(Presence {
            v: Version::CURRENT,
            card: card(seed),
            handle: handle.to_owned(),
            public: public.to_vec(),
            attached: false,
            addr: Addr::from_endpoint_report([UNREACHABLE_RELAY.to_owned()], []),
            seq,
            mac: ZERO_MAC,
            sig: UNSIGNED,
        })
    }

    async fn marker_reaches(member: &Member, witness: &Witness, marker_seed: u8) {
        witness
            .broadcast_as(
                marker_seed,
                presence_of(marker_seed, MARKER_HANDLE, MARKER_PUBLIC, wall_clock_ms()),
            )
            .await;
        listed_within(member, marker_seed, PAYLOAD_WAIT)
            .await
            .unwrap();
    }

    async fn start_third_through_second(
        scratch: &Scratch,
        second: &Member,
        third_handle: &'static str,
    ) -> Member {
        let mut third = start_members_reaching(
            scratch,
            vec![
                MemberPlan::new(THIRD_SEED, third_handle, THIRD_PUBLIC).with_hosts(&[SECOND_SEED]),
            ],
            &[(SECOND_SEED, second.endpoint.addr())],
        )
        .await;
        third.pop().unwrap()
    }

    #[tokio::test]
    async fn test_19_a_member_sees_a_third_member_joined_through_another_within_the_observer_wait()
    {
        let scratch = Scratch::new("t19").await;
        let (first, second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();

        let _third = start_third_through_second(&scratch, &second, THIRD_HANDLE).await;
        let third_presence_sent = tokio::time::Instant::now();
        let seen = listed_within(&first, THIRD_SEED, SPEC_OBSERVER_WAIT).await;

        assert!(third_presence_sent.elapsed() <= SPEC_OBSERVER_WAIT);
        let seen = seen.unwrap();
        assert_eq!(seen.effective_handle.as_str(), THIRD_HANDLE);
        assert_eq!(seen.public.as_deref(), Some(THIRD_PUBLIC));
        assert_eq!(seen.status, Status::Online);
        drop(scratch);
    }

    #[tokio::test]
    async fn test_7_a_second_member_requesting_a_taken_handle_assigns_itself_h_2_at_init_and_every_node_agrees()
     {
        let scratch = Scratch::new("t7").await;
        let (first, second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();

        let third = start_third_through_second(&scratch, &second, FIRST_HANDLE).await;

        assert_eq!(
            third.node.own_handle().as_str(),
            SECOND_EFFECTIVE_FIRST_HANDLE
        );
        let at_first = listed_within(&first, THIRD_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        let at_second = listed_within(&second, THIRD_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        assert_eq!(
            at_first.effective_handle.as_str(),
            SECOND_EFFECTIVE_FIRST_HANDLE
        );
        assert_eq!(
            at_second.effective_handle.as_str(),
            SECOND_EFFECTIVE_FIRST_HANDLE
        );
        drop(scratch);
    }

    async fn status_within(
        member: &Member,
        seed: u8,
        wanted: impl Fn(Status) -> bool + Send + Sync,
        within: Duration,
    ) -> Option<Peer> {
        tokio::time::timeout(within, async {
            loop {
                if let Some(peer) = member
                    .node
                    .peers()
                    .into_iter()
                    .find(|peer| peer.card == card(seed) && wanted(peer.status))
                {
                    return peer;
                }
                tokio::time::sleep(LISTING_POLL).await;
            }
        })
        .await
        .ok()
    }

    fn departure_of(seed: u8, seq: u64) -> Payload {
        Payload::Departure(crate::wire::Departure {
            v: Version::CURRENT,
            card: card(seed),
            seq,
            mac: ZERO_MAC,
            sig: UNSIGNED,
        })
    }

    #[tokio::test]
    async fn test_20_a_replayed_earlier_presence_changes_nothing_and_after_departure_leaves_the_member_gone()
     {
        let scratch = Scratch::new("t20").await;
        let earlier_session_seq = wall_clock_ms();
        let (first, _second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        let witness = Witness::joined_through(WITNESS_SEED, None, &first.endpoint).await;

        witness
            .broadcast_as(
                SECOND_SEED,
                presence_of(
                    SECOND_SEED,
                    SECOND_HANDLE,
                    REPLAYED_PUBLIC,
                    earlier_session_seq,
                ),
            )
            .await;
        marker_reaches(&first, &witness, MARKER_SEED).await;
        let after_replay = listed_within(&first, SECOND_SEED, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(after_replay.public.as_deref(), Some(SECOND_PUBLIC));
        assert_eq!(after_replay.status, Status::Online);

        witness
            .broadcast_as(SECOND_SEED, departure_of(SECOND_SEED, wall_clock_ms()))
            .await;
        status_within(
            &first,
            SECOND_SEED,
            |status| matches!(status, Status::Gone { .. }),
            PAYLOAD_WAIT,
        )
        .await
        .unwrap();
        witness
            .broadcast_as(
                SECOND_SEED,
                presence_of(
                    SECOND_SEED,
                    SECOND_HANDLE,
                    REPLAYED_PUBLIC,
                    earlier_session_seq,
                ),
            )
            .await;
        marker_reaches(&first, &witness, SECOND_MARKER_SEED).await;

        let after_departure = listed_within(&first, SECOND_SEED, Duration::ZERO)
            .await
            .unwrap();
        assert!(matches!(after_departure.status, Status::Gone { .. }));
        assert_eq!(after_departure.public.as_deref(), Some(SECOND_PUBLIC));
        drop(scratch);
    }

    async fn start_witnessed(
        scratch: &Scratch,
        plan: MemberPlan,
        witness_handle: Option<&'static str>,
    ) -> (Member, Witness) {
        let node_endpoint = bind(plan.seed).await;
        let endpoint = node_endpoint.endpoint().clone();
        let inbox = scratch.inbox(plan.seed);
        let inbox_path = scratch.directories.inbox_socket_file(u32::from(plan.seed));
        let config_path = scratch.network_file(plan.seed);
        let starting = tokio::spawn(start_member(
            plan,
            node_endpoint,
            (inbox, inbox_path),
            config_path,
        ));
        let witness = Witness::joined_through(WITNESS_SEED, witness_handle, &endpoint).await;
        (starting.await.unwrap(), witness)
    }

    fn frozen_clock(milliseconds: u64) -> Clock {
        Arc::new(move || milliseconds)
    }

    fn settable_clock(start_ms: u64) -> (Clock, Arc<std::sync::atomic::AtomicU64>) {
        let reading = Arc::new(std::sync::atomic::AtomicU64::new(start_ms));
        let read = Arc::clone(&reading);
        (
            Arc::new(move || read.load(std::sync::atomic::Ordering::SeqCst)),
            reading,
        )
    }

    impl MemberPlan {
        fn with_clock(self, clock: Clock) -> Self {
            Self { clock, ..self }
        }
    }

    fn infrastructure_of(host_seed: u8, relay: &str, ts: u64) -> Payload {
        Payload::Infrastructure(Infrastructure {
            v: Version::CURRENT,
            host: card(host_seed),
            relay: crate::wire::RelayUrl::try_from(relay.to_owned()).unwrap(),
            addr: Addr::from_endpoint_report([relay.to_owned()], []),
            ts,
            cert: SignatureBytes(
                crate::crypto::sign_host_certificate(
                    &secret_key(OWNER_SEED),
                    &secret_key(host_seed).public(),
                )
                .to_bytes(),
            ),
            mac: ZERO_MAC,
            sig: UNSIGNED,
        })
    }

    fn relay_url(text: &str) -> RelayUrl {
        text.parse().unwrap()
    }

    fn saved_host(member: &Member, host_seed: u8) -> Option<Host> {
        let id = render_public_key(&secret_key(host_seed).public());
        NetworkConfig::load(&member.config_path)
            .ok()?
            .hosts
            .into_iter()
            .find(|host| host.id == id)
    }

    async fn host_saved_within(member: &Member, host_seed: u8, within: Duration) -> Option<Host> {
        tokio::time::timeout(within, async {
            loop {
                if let Some(host) = saved_host(member, host_seed) {
                    return host;
                }
                tokio::time::sleep(LISTING_POLL).await;
            }
        })
        .await
        .ok()
    }

    async fn lookup_entry(member: &Member, seed: u8) -> Option<iroh::EndpointAddr> {
        let lookup = member.endpoint.address_lookup().unwrap();
        match lookup.resolve(secret_key(seed).public()).next().await {
            Some(Ok(Ok(item))) => Some(item.into_endpoint_addr()),
            _ => None,
        }
    }

    async fn relay_map_holds(member: &Member, relay: &str) -> bool {
        let removed = member.endpoint.remove_relay(&relay_url(relay)).await;
        if let Some(entry) = &removed {
            member
                .endpoint
                .insert_relay(relay_url(relay), Arc::clone(entry))
                .await;
        }
        removed.is_some()
    }

    #[tokio::test]
    async fn test_22_a_host_added_while_members_run_is_in_the_relay_map_lookup_and_configuration_within_1000_ms()
     {
        let scratch = Scratch::new("t22").await;
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
            None,
        )
        .await;
        let record_ts = wall_clock_ms() / 1000;

        witness
            .broadcast_as(
                HOST_SEED,
                infrastructure_of(HOST_SEED, HOST_RELAY, record_ts),
            )
            .await;
        let saved = host_saved_within(&member, HOST_SEED, SPEC_HOST_ADDED_WITHIN).await;

        let saved = saved.unwrap();
        assert_eq!(saved.relay, HOST_RELAY);
        assert_eq!(saved.ts, Some(record_ts));
        assert!(matches!(saved.origin, HostOrigin::Learned { .. }));
        assert!(relay_map_holds(&member, HOST_RELAY).await);
        let looked_up = lookup_entry(&member, HOST_SEED).await.unwrap();
        assert_eq!(
            looked_up.relay_urls().cloned().collect::<Vec<_>>(),
            vec![relay_url(HOST_RELAY)]
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn test_27_a_replayed_host_record_not_newer_than_the_last_accepted_is_dropped_and_a_newer_one_accepted()
     {
        let scratch = Scratch::new("t27").await;
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
            None,
        )
        .await;
        let record_ts = wall_clock_ms() / 1000;
        witness
            .broadcast_as(
                HOST_SEED,
                infrastructure_of(HOST_SEED, HOST_RELAY, record_ts),
            )
            .await;
        host_saved_within(&member, HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap();
        member
            .endpoint
            .remove_relay(&relay_url(HOST_RELAY))
            .await
            .unwrap();

        witness
            .broadcast_as(
                HOST_SEED,
                infrastructure_of(HOST_SEED, HOST_RELAY, record_ts),
            )
            .await;
        witness
            .broadcast_as(
                SECOND_HOST_SEED,
                infrastructure_of(SECOND_HOST_SEED, SECOND_HOST_RELAY, record_ts),
            )
            .await;
        host_saved_within(&member, SECOND_HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap();
        assert!(!relay_map_holds(&member, HOST_RELAY).await);
        assert_eq!(saved_host(&member, HOST_SEED).unwrap().ts, Some(record_ts));

        witness
            .broadcast_as(
                HOST_SEED,
                infrastructure_of(HOST_SEED, HOST_RELAY, record_ts + 1),
            )
            .await;
        witness
            .broadcast_as(
                THIRD_HOST_SEED,
                infrastructure_of(THIRD_HOST_SEED, THIRD_HOST_RELAY, record_ts),
            )
            .await;
        host_saved_within(&member, THIRD_HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap();
        assert!(relay_map_holds(&member, HOST_RELAY).await);
        assert_eq!(
            saved_host(&member, HOST_SEED).unwrap().ts,
            Some(record_ts + 1)
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_14_a_host_record_more_than_300_s_ahead_of_this_clock_is_dropped_and_one_300_s_ahead_accepted()
     {
        let scratch = Scratch::new("bound").await;
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_clock(frozen_clock(FROZEN_CLOCK_MS)),
            None,
        )
        .await;
        let latest_accepted_ts =
            FROZEN_CLOCK_MS / 1000 + SPEC_INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS;

        witness
            .broadcast_as(
                HOST_SEED,
                infrastructure_of(HOST_SEED, HOST_RELAY, latest_accepted_ts + 1),
            )
            .await;
        witness
            .broadcast_as(
                SECOND_HOST_SEED,
                infrastructure_of(SECOND_HOST_SEED, SECOND_HOST_RELAY, latest_accepted_ts),
            )
            .await;
        host_saved_within(&member, SECOND_HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap();

        assert!(saved_host(&member, HOST_SEED).is_none());
        assert!(!relay_map_holds(&member, HOST_RELAY).await);
        assert!(relay_map_holds(&member, SECOND_HOST_RELAY).await);
        drop(scratch);
    }

    fn presence_from(seed: u8) -> impl FnMut(&Payload) -> bool {
        move |payload| matches!(payload, Payload::Presence(presence) if presence.card == card(seed))
    }

    fn presence_fields(payload: Option<Payload>) -> (u64, bool) {
        let Some(Payload::Presence(presence)) = payload else {
            panic!("a presence was expected");
        };
        (presence.seq, presence.attached)
    }

    #[tokio::test]
    async fn spec_3_8_presence_carries_seq_max_of_now_and_last_plus_one_and_each_attach_or_detach_announces()
     {
        let scratch = Scratch::new("seq").await;
        let (member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_clock(frozen_clock(FROZEN_CLOCK_MS)),
            None,
        )
        .await;
        let first = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;

        let client = tokio::net::UnixStream::connect(&member.inbox_path)
            .await
            .unwrap();
        let on_attach = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;
        drop(client);
        let on_detach = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;

        assert_eq!(presence_fields(first), (FROZEN_CLOCK_MS, false));
        assert_eq!(presence_fields(on_attach), (FROZEN_CLOCK_MS + 1, true));
        assert_eq!(presence_fields(on_detach), (FROZEN_CLOCK_MS + 2, false));
        drop(scratch);
    }

    fn unreachable_about(seed: u8, reporter_seed: u8, ts: u64) -> Payload {
        Payload::Unreachable(Unreachable {
            v: Version::CURRENT,
            card: card(seed),
            reporter: card(reporter_seed),
            ts,
            mac: ZERO_MAC,
            sig: UNSIGNED,
        })
    }

    #[tokio::test]
    async fn spec_3_8_an_unreachable_report_about_this_node_is_answered_with_presence_unless_stale_or_below_its_last_seq()
     {
        let scratch = Scratch::new("rumor").await;
        let (clock, reading) = settable_clock(FROZEN_CLOCK_MS);
        let (_member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_clock(clock),
            None,
        )
        .await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .unwrap();

        witness
            .broadcast_as(
                WITNESS_SEED,
                unreachable_about(FIRST_SEED, WITNESS_SEED, FROZEN_CLOCK_MS - 1),
            )
            .await;
        reading.store(CLOCK_AFTER_MAX_AGE_MS, std::sync::atomic::Ordering::SeqCst);
        witness
            .broadcast_as(
                WITNESS_SEED,
                unreachable_about(FIRST_SEED, WITNESS_SEED, STALE_RUMOR_TS),
            )
            .await;
        witness
            .broadcast_as(
                WITNESS_SEED,
                unreachable_about(FIRST_SEED, WITNESS_SEED, FRESH_RUMOR_TS),
            )
            .await;
        let answer = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;
        let nothing_more = witness
            .payload_where(NOTHING_MORE_WAIT, presence_from(FIRST_SEED))
            .await;

        assert_eq!(presence_fields(answer), (CLOCK_AFTER_MAX_AGE_MS, false));
        assert!(nothing_more.is_none());
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_an_unreachable_report_about_a_peer_sets_it_unreachable() {
        let scratch = Scratch::new("peer-rumor").await;
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
            Some(WITNESS_HANDLE),
        )
        .await;
        witness
            .broadcast_as(
                WITNESS_SEED,
                presence_of(
                    WITNESS_SEED,
                    WITNESS_HANDLE,
                    WITNESS_PUBLIC,
                    wall_clock_ms(),
                ),
            )
            .await;
        status_within(
            &member,
            WITNESS_SEED,
            |status| status == Status::Online,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap();
        tokio::time::sleep(INTRODUCE_ROUND_TRIP).await;

        witness
            .broadcast_as(
                MARKER_SEED,
                unreachable_about(WITNESS_SEED, MARKER_SEED, wall_clock_ms()),
            )
            .await;

        status_within(
            &member,
            WITNESS_SEED,
            |status| status == Status::Unreachable,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap();
        drop(scratch);
    }

    fn unreachable_report(
        payload: Option<Payload>,
    ) -> (crate::wire::PublicKey, crate::wire::PublicKey) {
        let Some(Payload::Unreachable(report)) = payload else {
            panic!("an unreachable report was expected");
        };
        (report.card, report.reporter)
    }

    #[tokio::test]
    async fn test_5_logic_a_neighbor_down_whose_probe_fails_is_set_unreachable_and_announced() {
        let scratch = Scratch::new("down").await;
        let (first, second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        let mut witness = Witness::joined_through(WITNESS_SEED, None, &first.endpoint).await;
        let killed = tokio::time::Instant::now();

        drop(second.node);
        second.endpoint.close().await;
        let unreachable = status_within(
            &first,
            SECOND_SEED,
            |status| status == Status::Unreachable,
            SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT,
        )
        .await;
        let announced = witness
            .payload_where(PAYLOAD_WAIT, |payload| {
                matches!(payload, Payload::Unreachable(_))
            })
            .await;

        assert!(unreachable.is_some());
        assert!(killed.elapsed() < SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT + PAYLOAD_WAIT);
        assert_eq!(
            unreachable_report(announced),
            (card(SECOND_SEED), card(FIRST_SEED))
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_neighbor_down_whose_probe_is_acknowledged_stays_online_on_the_connection_the_introduce_ack_came_on()
     {
        let scratch = Scratch::new("probe-ack").await;
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
            Some(WITNESS_HANDLE),
        )
        .await;
        status_within(
            &member,
            WITNESS_SEED,
            |status| status == Status::Online,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap();
        tokio::time::sleep(INTRODUCE_ROUND_TRIP).await;
        let probes_before = witness
            .probes_answered
            .load(std::sync::atomic::Ordering::SeqCst);

        witness.gossip.shutdown().await.unwrap();
        let probed = tokio::time::timeout(PROBE_ANSWERED_WAIT, async {
            while witness
                .probes_answered
                .load(std::sync::atomic::Ordering::SeqCst)
                == probes_before
            {
                tokio::time::sleep(LISTING_POLL).await;
            }
        })
        .await;
        tokio::time::sleep(AFTER_PROBE_ACK).await;

        probed.unwrap();
        let after = listed_within(&member, WITNESS_SEED, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(after.status, Status::Online);
        assert_eq!(after.effective_handle.as_str(), WITNESS_HANDLE);
        assert_eq!(
            witness
                .direct_connections
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_an_introduce_that_fails_sets_the_peer_unreachable_and_announces_it() {
        let scratch = Scratch::new("intro-fail").await;
        let (member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
            None,
        )
        .await;

        witness
            .broadcast_as(
                MARKER_SEED,
                presence_of(MARKER_SEED, MARKER_HANDLE, MARKER_PUBLIC, wall_clock_ms()),
            )
            .await;
        let announced = witness
            .payload_where(
                SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT + PAYLOAD_WAIT,
                |payload| matches!(payload, Payload::Unreachable(_)),
            )
            .await;

        assert_eq!(
            unreachable_report(announced),
            (card(MARKER_SEED), card(FIRST_SEED))
        );
        let listed = listed_within(&member, MARKER_SEED, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(listed.status, Status::Unreachable);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_join_without_a_neighbor_assigns_the_handle_at_the_join_timeout_and_a_later_join_announces()
     {
        let scratch = Scratch::new("late").await;
        let started = tokio::time::Instant::now();
        let lonely = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)],
        )
        .await
        .pop()
        .unwrap();
        let init_took = started.elapsed();

        let mut witness = Witness::joined_through(WITNESS_SEED, None, &lonely.endpoint).await;
        let announced = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;

        assert!(init_took >= SPEC_JOIN_TIMEOUT);
        assert!(init_took < SPEC_JOIN_TIMEOUT + SPEC_OBSERVER_WAIT);
        assert_eq!(lonely.node.own_handle().as_str(), FIRST_HANDLE);
        assert!(announced.is_some());
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_each_known_host_without_an_address_gets_its_relay_and_a_fed_address_is_kept()
    {
        let scratch = Scratch::new("seed").await;
        let mut members = start_members(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_hosts(&[SECOND_SEED, HOST_SEED]),
                MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC)
                    .with_hosts(&[FIRST_SEED]),
            ],
        )
        .await;
        let _second = members.pop().unwrap();
        let first = members.pop().unwrap();

        let unfed_host = lookup_entry(&first, HOST_SEED).await.unwrap();
        let fed_host = lookup_entry(&first, SECOND_SEED).await.unwrap();

        assert_eq!(
            unfed_host.relay_urls().cloned().collect::<Vec<_>>(),
            vec![relay_url(UNREACHABLE_RELAY)]
        );
        assert_eq!(unfed_host.ip_addrs().count(), 0);
        assert!(fed_host.ip_addrs().count() > 0);
        drop(scratch);
    }

    const PROPERTY_CASES: u64 = 500;
    const STEPS_PER_CASE: u64 = 40;
    const SPEC_REANNOUNCE_INTERVAL: Duration = Duration::from_millis(2000);
    const SPEC_RELAY_LOSS_WAIT: Duration = Duration::from_millis(500);
    const LCG_MULTIPLIER: u64 = 6_364_136_223_846_793_005;
    const LCG_INCREMENT: u64 = 1_442_695_040_888_963_407;
    const LCG_HIGH_BITS_SHIFT: u32 = 33;
    const RELAY_POOL: [&str; 3] = [
        "https://relay-a.estafette.invalid/",
        "https://relay-b.estafette.invalid/",
        "https://relay-c.estafette.invalid/",
    ];
    const DIRECT_POOL: [&str; 2] = ["192.0.2.1:4433", "198.51.100.2:4433"];
    const LARGEST_STEP_MS: u64 = 700;

    struct Draws(u64);

    impl Draws {
        fn seeded(case: u64) -> Self {
            Self(
                case.wrapping_mul(LCG_MULTIPLIER)
                    .wrapping_add(LCG_INCREMENT),
            )
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(LCG_MULTIPLIER)
                .wrapping_add(LCG_INCREMENT);
            (self.0 >> LCG_HIGH_BITS_SHIFT).checked_rem(bound).unwrap()
        }
    }

    fn drawn_addr(draws: &mut Draws) -> Addr {
        let relay = RELAY_POOL[usize::try_from(draws.below(3)).unwrap()];
        let direct = DIRECT_POOL[usize::try_from(draws.below(2)).unwrap()];
        Addr::from_endpoint_report([relay.to_owned()], [direct.parse().unwrap()])
    }

    #[test]
    fn spec_3_8_a_changed_address_is_announced_no_sooner_than_the_reannounce_interval_and_at_its_end_if_it_still_differs()
     {
        let origin = Instant::now();
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let mut announcer = Announcer::default();
            let unannounced = drawn_addr(&mut draws);
            assert_eq!(
                announcer.reannounce_step(&unannounced, origin),
                ReannounceStep::Unchanged
            );
            let last_announced_addr = drawn_addr(&mut draws);
            let announced_at = origin + Duration::from_millis(draws.below(LARGEST_STEP_MS));
            announcer.record(1, last_announced_addr.clone(), announced_at);
            let current = drawn_addr(&mut draws);
            let now = announced_at + Duration::from_millis(draws.below(3 * LARGEST_STEP_MS + 1));
            let interval_over = announced_at + SPEC_REANNOUNCE_INTERVAL;

            let step = announcer.reannounce_step(&current, now);

            let expected = if current == last_announced_addr {
                ReannounceStep::Unchanged
            } else if now >= interval_over {
                ReannounceStep::AnnounceNow
            } else {
                ReannounceStep::AnnounceAt(interval_over)
            };
            assert_eq!(step, expected, "case {case}");
            if let ReannounceStep::AnnounceAt(due) = step {
                assert_eq!(
                    announcer.reannounce_step(&current, due),
                    ReannounceStep::AnnounceNow
                );
                assert_eq!(
                    announcer.reannounce_step(&last_announced_addr, due),
                    ReannounceStep::Unchanged
                );
            }
        }
    }

    fn drawn_home_relays(draws: &mut Draws) -> Vec<HomeRelay> {
        RELAY_POOL
            .iter()
            .filter_map(|relay| match draws.below(3) {
                0 => None,
                state => Some(HomeRelay {
                    url: relay_url(relay),
                    connected: state == 1,
                }),
            })
            .collect()
    }

    #[test]
    fn spec_3_8_a_home_relay_is_removed_only_once_disconnected_for_the_relay_loss_wait_and_then_without_fail()
     {
        let origin = Instant::now();
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let mut watch = RelayLossWatch::default();
            let mut lost_since: BTreeMap<RelayUrl, Instant> = BTreeMap::new();
            let mut last_removed: BTreeMap<RelayUrl, Instant> = BTreeMap::new();
            let mut now = origin;
            for step in 0..STEPS_PER_CASE {
                now += Duration::from_millis(draws.below(LARGEST_STEP_MS));
                let home = drawn_home_relays(&mut draws);
                lost_since.retain(|url, _| {
                    home.iter()
                        .any(|relay| relay.url == *url && !relay.connected)
                });
                for relay in home.iter().filter(|relay| !relay.connected) {
                    lost_since.entry(relay.url.clone()).or_insert(now);
                }
                if step % 2 == 0 {
                    watch.observe(&home, now);
                    continue;
                }
                let removed = watch.lost_relays(&home, now);
                for url in &removed {
                    let since = lost_since[url];
                    let waited_from = last_removed
                        .get(url)
                        .map_or(since, |removed_at| since.max(*removed_at));
                    assert!(
                        now >= waited_from + SPEC_RELAY_LOSS_WAIT,
                        "case {case} step {step}"
                    );
                }
                for (url, since) in &lost_since {
                    let never_removed_since = last_removed
                        .get(url)
                        .is_none_or(|removed_at| removed_at < since);
                    if never_removed_since && now >= *since + SPEC_RELAY_LOSS_WAIT {
                        assert!(removed.contains(url), "case {case} step {step}");
                    }
                }
                for url in removed {
                    last_removed.insert(url, now);
                }
            }
        }
    }

    #[tokio::test]
    async fn spec_3_1_a_message_whose_announced_handle_does_not_match_is_neither_queued_nor_listed()
    {
        let scratch = Scratch::new("bad-handle").await;
        let mut members = start_members(
            &scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
                MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC).reaching(&[FIRST_SEED]),
            ],
        )
        .await;
        let second = members.pop().unwrap();
        let first = members.pop().unwrap();
        let sender = DirectClient::new(second.endpoint.clone(), keys(), ClientTimeouts::SPEC);

        let mismatched = Message {
            body: MISMATCHED_MESSAGE_BODY.to_owned(),
            ..message_to(FIRST_SEED, NOT_AN_ANNOUNCED_HANDLE)
        };
        sender
            .send_message(first.endpoint.id(), mismatched.clone())
            .await
            .unwrap();
        let listed_after_unknown_mismatch = first.node.peers();
        sender
            .send_message(first.endpoint.id(), message_to(FIRST_SEED, SECOND_HANDLE))
            .await
            .unwrap();
        sender
            .send_message(first.endpoint.id(), mismatched)
            .await
            .unwrap();
        sender
            .send_message(
                first.endpoint.id(),
                Message {
                    body: LATER_MESSAGE_BODY.to_owned(),
                    ..message_to(FIRST_SEED, SECOND_HANDLE)
                },
            )
            .await
            .unwrap();

        assert_eq!(listed_after_unknown_mismatch, Vec::<Peer>::new());
        let frames = inbox_frames(&first, 2).await;
        assert_eq!(frames[0]["body"], MESSAGE_BODY);
        assert_eq!(frames[1]["seq"], 2);
        assert_eq!(frames[1]["body"], LATER_MESSAGE_BODY);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_presence_and_departure_for_this_nodes_own_card_are_ignored() {
        let scratch = Scratch::new("own-card").await;
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
            None,
        )
        .await;

        witness
            .broadcast_as(
                FIRST_SEED,
                presence_of(
                    FIRST_SEED,
                    IMPOSTOR_HANDLE,
                    REPLAYED_PUBLIC,
                    wall_clock_ms(),
                ),
            )
            .await;
        witness
            .broadcast_as(FIRST_SEED, departure_of(FIRST_SEED, wall_clock_ms()))
            .await;
        marker_reaches(&member, &witness, MARKER_SEED).await;

        assert!(
            member
                .node
                .peers()
                .iter()
                .all(|peer| peer.card != card(FIRST_SEED))
        );
        assert_eq!(member.node.own_handle().as_str(), FIRST_HANDLE);
        drop(scratch);
    }
}
