//! The presence half of the node of spec 3.8: init, the peer table fed by gossip and direct contact, and every announcement.

use std::ops::ControlFlow;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use iroh::endpoint::RelayStatus;
use iroh::protocol::Router;
use iroh::{EndpointAddr, EndpointId, SecretKey, Watcher};
use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::addresses::{lookup_replaced_event, replace_lookup_entry, wire_addr};
use super::announcements::{
    Announcer, HeardPresenceRequests, ReannounceStep, RequestSeqs, rumor_admission,
};
use super::direct_requests::{DirectAnswer, DirectRequest, DirectRequester, send_in_background};
#[cfg(test)]
use super::direct_requests::{DirectContact, DirectExchange, RequestCause};
use super::init::{Init, InitAction, InitEvent, PendingHandle};
use super::known_hosts::{
    KnownHosts, RelayOfRecord, known_host_addresses, seed_lookup_with_known_hosts,
};
use super::node_error::NodeError;
use super::payload_records::{
    broadcast_recording_outcome, named_admission_record, next_recorded_gossip_event,
    signer_admission_record,
};
use super::rebind::{Rebinding, RelayProbe};
use super::relay_loss::{RelayLossWatch, home_relays};
use crate::clock::{MonotonicClock, MonotonicTime, later_by};
use crate::config::NetworkConfig;
use crate::crypto::{NetworkKeys, NetworkSecret};
use crate::event::{ConfigurationRewrite, Event, PeerTableEvent, ProcessEvent, RunRecord};
use crate::handle::{AnnouncedHandle, LocalHandle};
use crate::inbox::buffer::ReceivedMessage;
use crate::inbox::socket::{Attached, InboxSocket};
use crate::keys::{parse_network_secret, parse_public_key};
use crate::net::direct::{ClientTimeouts, DirectClient, DirectHost, DirectServer, HeldConnections};
use crate::net::endpoint::{
    BoundEndpoint, CounterSnapshot, DIRECT_ALPN, NodeEndpoint, RebindCause, SnapshotMoment,
};
use crate::net::gossip::{GossipError, GossipEvent, NetworkTopic};
use crate::payload::{
    DescribedPayload, OpenedPayload, PayloadDropRule, PayloadFacts, ZERO_MAC, ZERO_SIGNATURE,
};
use crate::peers::{
    ContactSucceeded, Effect, FailedContactKind, Operated, Peer, PeerTable, TableChange,
    announced_sender_handle,
};
use crate::stream_items::{Stream, StreamExt};
use crate::wire::{
    self, Addr, Departure, Introduce, IntroduceAck, Message, Nonce, Payload, Presence,
    PresenceRequest, Unreachable, Version,
};

/// The longest a node goes without a snapshot of its counters, so a live process writes its events file at least this often (`spec/observability.md` decisions 9 and 13).
const STATE_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(60);
const SECRET_FIELD: &str = "secret";
const OWNER_FIELD: &str = "owner";

/// Milliseconds since the Unix epoch by this machine's clock.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Fills a probe nonce from the random source.
pub type ProbeNonceSource = Arc<dyn Fn(&mut Nonce) + Send + Sync>;
type Changes<T> = Pin<Box<dyn Stream<Item = T> + Send + Sync>>;
type OwnAddress = Box<dyn Fn() -> Addr + Send + Sync>;

/// The card this node holds: its key, the handle it requests, and the public mapping it announces as JSON bytes.
pub struct Identity {
    pub card_key: SecretKey,
    pub requested_handle: AnnouncedHandle,
    pub public: Vec<u8>,
}

/// The network this node joins: its configuration and the file that keeps it, with the keys and owner key derived from it.
pub struct Network {
    keys: NetworkKeys,
    owner: iroh::PublicKey,
    config: NetworkConfig,
    config_path: PathBuf,
}

impl Network {
    /// Takes the network configuration and its file, deriving the network keys from its secret and reading its owner key.
    ///
    /// # Errors
    /// `NodeError::NetworkKeyInvalid` when the secret or the owner key does not parse.
    pub fn new(config: NetworkConfig, config_path: PathBuf) -> Result<Self, NodeError> {
        let secret = parse_network_secret(&config.secret).map_err(|source| {
            NodeError::NetworkKeyInvalid {
                field: SECRET_FIELD,
                source,
            }
        })?;
        let owner =
            parse_public_key(&config.owner).map_err(|source| NodeError::NetworkKeyInvalid {
                field: OWNER_FIELD,
                source,
            })?;
        Ok(Self {
            keys: NetworkKeys::derive(&NetworkSecret::from_bytes(*secret.as_bytes())),
            owner,
            config,
            config_path,
        })
    }

    /// The topic id, mac key and gate key derived from the network secret.
    #[must_use]
    pub const fn keys(&self) -> &NetworkKeys {
        &self.keys
    }

    /// The owner key that certifies every host.
    pub(crate) const fn owner(&self) -> iroh::PublicKey {
        self.owner
    }

    /// The known hosts of the configuration, kept in its file.
    pub(crate) fn into_known_hosts(self) -> KnownHosts {
        KnownHosts::new(self.config, self.config_path)
    }
}

/// The side effects the node runs on, initialized by the composition root.
#[derive(Clone)]
pub struct NodeEffects {
    /// Milliseconds since the Unix epoch by this machine's clock.
    pub clock: Clock,
    /// The monotonic clock every timed arm of the presence loop is measured on, the runtime's own in production.
    pub monotonic: MonotonicClock,
    /// A cryptographically secure source of 16 random bytes: the nonce of each probe and the id of each message.
    pub fill_probe_nonce: ProbeNonceSource,
    /// The run record, written to standard error by the command-line layer; it must not block.
    pub report: RunRecord,
    /// The inbox socket that started with the process.
    pub inbox: Arc<InboxSocket>,
    /// Whether a TCP connect to a relay's host and port succeeds, `relay_answers` in production; the node gives it the relay probe timeout.
    pub probe_relay: RelayProbe,
}

/// A running member: joined to the topic, serving the direct protocol, keeping its peer table by spec 3.8, and rebinding when it falls deaf.
pub struct Node {
    pub(super) state: Arc<NodeState>,
    pub(super) client: Arc<DirectClient>,
    pub(super) fill_random_bytes: ProbeNonceSource,
    own_handle: LocalHandle,
    known_hosts: Arc<KnownHosts>,
    report: RunRecord,
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
        let client = Arc::new(DirectClient::new(
            endpoint.endpoint().clone(),
            network.keys.clone(),
            ClientTimeouts::SPEC,
            endpoint.held_connections(),
        ));
        let (to_loop, from_hosts) = mpsc::unbounded_channel();
        let state = Arc::new(NodeState::new(
            &identity,
            Arc::clone(&client),
            &effects,
            to_loop,
            endpoint.held_connections(),
        ));
        let fill_random_bytes = Arc::clone(&effects.fill_probe_nonce);
        let report = Arc::clone(&effects.report);
        let (presence_loop, own_handle) = PresenceLoop::join(
            endpoint,
            identity,
            network,
            effects,
            Arc::clone(&state),
            Arc::clone(&client),
            from_hosts,
        )
        .await?;
        let known_hosts = Arc::clone(&presence_loop.known_hosts);
        let presence_loop = tokio::spawn(presence_loop.run());
        let own_handle = own_handle
            .await
            .map_err(|_closed| NodeError::TopicClosedBeforeHandle)?;
        Ok(Self {
            state,
            client,
            fill_random_bytes,
            own_handle,
            known_hosts,
            report,
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

    /// Starts refusing Messages and ignoring Probes and Introduces, and hands the Departure to the presence loop.
    pub(super) fn start_stopping(&self) {
        self.state.stopping.store(true, Ordering::SeqCst);
        self.state.hand_to_loop(LoopEvent::Depart);
    }

    /// Drops every direct connection the client holds, whether or not its card is in the table.
    pub(super) async fn drop_held_connections(&self) {
        self.client.drop_all_held_connections().await;
    }

    /// Rewrites the network configuration when a held host record is newer than the file's, and records what the rewrite did.
    pub(super) fn rewrite_configuration_if_newer(&self) {
        let rewrite = match self
            .known_hosts
            .save_if_newer_than_file(self.state.now_ms())
        {
            Ok(rewrite) => rewrite,
            Err(failure) => {
                (self.report)(crate::event::GossipEvent::KnownHostsNotSaved(failure).into());
                ConfigurationRewrite::Failed
            }
        };
        (self.report)(ProcessEvent::ConfigurationRewriteDecided(rewrite).into());
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.presence_loop.abort();
    }
}

/// Serves the direct protocol and a fresh gossip instance on the endpoint, returning the router that shuts both down with the endpoint, and the gossip.
pub fn serve<H: DirectHost + Clone>(
    endpoint: &NodeEndpoint,
    keys: &NetworkKeys,
    host: &H,
) -> (Router, Gossip) {
    let gossip = endpoint.endpoint().spawn_gossip();
    let router = endpoint
        .endpoint()
        .router_builder()
        .accept(
            DIRECT_ALPN,
            DirectServer::new(keys.clone(), host.clone(), endpoint),
        )
        .accept(GOSSIP_ALPN, gossip.clone())
        .spawn();
    (router, gossip)
}

/// Each change of the endpoint's own address.
pub(super) fn own_address_changes(endpoint: &BoundEndpoint) -> Changes<EndpointAddr> {
    Box::pin(endpoint.watch_addr().stream_updates_only())
}

/// Each change of the endpoint's home relays and their connection status.
pub(super) fn home_relay_changes(endpoint: &BoundEndpoint) -> Changes<Vec<RelayStatus>> {
    Box::pin(endpoint.home_relay_status().stream_updates_only())
}

/// What the direct server and the presence loop share: the public half of the identity, the peer table, this node's address and the inbox.
pub(super) struct NodeState {
    own_card: wire::PublicKey,
    public: Vec<u8>,
    own_handle: OnceLock<LocalHandle>,
    own_address: OwnAddress,
    inbox: Arc<InboxSocket>,
    attached: Attached,
    clock: Clock,
    to_loop: mpsc::UnboundedSender<LoopEvent>,
    stopping: AtomicBool,
    rows: PeerRows,
}

/// The peer table of a node of any kind and the register of every connection its endpoint holds, changed together by the rows of spec 3.8 every such node applies, recording what each row decided.
pub struct PeerRows {
    table: Mutex<PeerTable>,
    held_connections: HeldConnections,
    report: RunRecord,
    #[cfg(test)]
    direct_exchanges: Mutex<Vec<DirectExchange>>,
}

/// A received payload the peer table admits by the operation of its own kind.
trait AdmittedByTheTable: DescribedPayload {
    /// What the table gives when it takes the payload.
    type Taken;

    /// Admits this payload to `table` at `now_ms`, or names the rule that drops it.
    fn admit_to(
        &self,
        table: &mut PeerTable,
        now_ms: u64,
    ) -> Operated<Result<Self::Taken, PayloadDropRule>>;
}

impl AdmittedByTheTable for Presence {
    type Taken = Vec<Effect>;

    fn admit_to(
        &self,
        table: &mut PeerTable,
        now_ms: u64,
    ) -> Operated<Result<Vec<Effect>, PayloadDropRule>> {
        table.admit_presence(self, now_ms)
    }
}

impl AdmittedByTheTable for Departure {
    type Taken = Vec<Effect>;

    fn admit_to(
        &self,
        table: &mut PeerTable,
        now_ms: u64,
    ) -> Operated<Result<Vec<Effect>, PayloadDropRule>> {
        table.admit_departure(self, now_ms)
    }
}

impl AdmittedByTheTable for Unreachable {
    type Taken = ();

    fn admit_to(
        &self,
        table: &mut PeerTable,
        now_ms: u64,
    ) -> Operated<Result<(), PayloadDropRule>> {
        table.admit_unreachable(self, now_ms)
    }
}

impl PeerRows {
    /// An empty table for the node of `own_card`, closing connections through the register of the endpoint it runs on and recording each row's decision to `report`.
    pub(crate) fn new(
        own_card: wire::PublicKey,
        held_connections: HeldConnections,
        report: RunRecord,
    ) -> Self {
        Self {
            table: Mutex::new(PeerTable::new(own_card)),
            held_connections,
            report,
            #[cfg(test)]
            direct_exchanges: Mutex::new(Vec::new()),
        }
    }

    /// Notes the direct requests made and the answers applied, after every earlier one.
    #[cfg(test)]
    pub(crate) fn note_direct_exchanges(
        &self,
        exchanges: impl IntoIterator<Item = DirectExchange>,
    ) {
        self.direct_exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(exchanges);
    }

    /// Every direct request made and answer applied so far, in order.
    #[cfg(test)]
    pub(crate) fn direct_exchanges(&self) -> Vec<DirectExchange> {
        self.direct_exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many probes to the card the node has made for neighbor-downs of that card.
    #[cfg(test)]
    pub(crate) fn probes_for_neighbor_downs_of(&self, card: wire::PublicKey) -> usize {
        self.direct_exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|exchange| {
                matches!(
                    exchange,
                    DirectExchange::Made {
                        kind: DirectContact::Probe,
                        cause: Some(RequestCause::NeighborDown { card: down, .. }),
                        ..
                    } if *down == card
                )
            })
            .count()
    }

    /// The peer table, locked for one operation.
    pub(crate) fn table(&self) -> MutexGuard<'_, PeerTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs one operation on the locked table, returning what it gave once the lock is released.
    fn operate_on_locked_table<Given>(
        &self,
        operation: impl FnOnce(&mut PeerTable) -> Given,
    ) -> Given {
        operation(&mut self.table())
    }

    /// Records each change of the table.
    fn record_changes(&self, changes: Vec<TableChange>) {
        for change in changes {
            (self.report)(PeerTableEvent::of_change(change).into());
        }
    }

    /// Runs one operation on the locked table, records each change it made, and returns what it gave.
    pub(crate) fn change<Given>(
        &self,
        operation: impl FnOnce(&mut PeerTable) -> Operated<Given>,
    ) -> Given {
        let operated = self.operate_on_locked_table(operation);
        self.record_changes(operated.changes);
        operated.value
    }

    /// Runs the admission of a received payload on the locked table, records whether it was taken under its signer's handle, then each change it made, and returns what it gave.
    fn admit<Received: AdmittedByTheTable>(
        &self,
        payload: &Received,
        now_ms: u64,
    ) -> Result<Received::Taken, PayloadDropRule> {
        let (
            Operated {
                value: admitted,
                changes,
            },
            record,
        ) = self.operate_on_locked_table(|table| {
            let admitted = payload.admit_to(table, now_ms);
            let record = signer_admission_record(table, payload.facts(), &admitted.value, now_ms);
            (admitted, record)
        });
        (self.report)(record);
        self.record_changes(changes);
        admitted
    }

    /// Applies a Presence to the table, recording whether it was taken, and when it is taken closes every connection to or from its card, of any protocol, whose process no longer runs, and every other while iroh stays on that process's path (spec 3.8, address lookup).
    pub(crate) fn take_presence(&self, presence: &Presence, now_ms: u64) -> Vec<Effect> {
        let admitted = self.admit(presence, now_ms);
        let Ok(effects) = admitted else {
            return Vec::new();
        };
        if effects.contains(&Effect::DropHeldConnection(presence.card)) {
            self.held_connections.close_silent_of(presence.card);
        }
        effects
    }

    /// Applies a Departure to the table, returning the drop of the held connection it calls for once it is taken (spec 3.8).
    #[must_use]
    pub(crate) fn take_departure(&self, departure: &Departure, now_ms: u64) -> Vec<Effect> {
        self.admit(departure, now_ms).unwrap_or_default()
    }

    /// Applies an Unreachable report about another card to the table, recording whether it was taken (spec 3.8).
    pub(crate) fn take_unreachable(&self, report: &Unreachable, now_ms: u64) {
        let _admission = self.admit(report, now_ms);
    }

    /// Applies a gossip report of a new neighbor, returning the Introduce it calls for (spec 3.8).
    #[must_use]
    pub(crate) fn take_neighbor_up(&self, node: EndpointId, now_ms: u64) -> Vec<Effect> {
        self.table()
            .neighbor_up(wire::PublicKey::from(node), now_ms)
    }

    /// Applies a gossip report of a lost neighbor, returning the Probe it calls for (spec 3.8).
    #[must_use]
    pub(crate) fn take_neighbor_down(&self, node: EndpointId, now_ms: u64) -> Vec<Effect> {
        self.change(|table| table.neighbor_down(wire::PublicKey::from(node), now_ms))
    }

    /// Applies the answer to a direct request this node sent: an `IntroduceAck` as an introduction that spares the connection it came on, a failure as a failed contact, an acknowledged Probe as a contact that succeeded (spec 3.8).
    pub(crate) fn take_direct_answer(&self, answer: DirectAnswer, now_ms: u64) -> Vec<Effect> {
        match answer {
            DirectAnswer::Introduce {
                card,
                answer: Ok(ack),
            } => sparing_the_answering_connection(
                self.change(|table| table.receive_introduce_ack(card, &ack, now_ms)),
            ),
            DirectAnswer::Introduce {
                card,
                answer: Err(_),
            } => self.change(|table| {
                table.direct_contact_failed(card, now_ms, FailedContactKind::Introduce)
            }),
            DirectAnswer::Probe {
                card,
                answer: Err(_),
            } => self.change(|table| {
                table.direct_contact_failed(card, now_ms, FailedContactKind::Probe)
            }),
            DirectAnswer::Probe {
                card,
                answer: Ok(()),
            } => {
                self.change(|table| {
                    table.direct_contact_succeeded(card, now_ms, ContactSucceeded::Probe)
                });
                Vec::new()
            }
        }
    }
}

impl NodeState {
    /// The shared state of a node that holds the identity's card, reading its address from the endpoint the client dials from and closing connections through that endpoint's register.
    fn new(
        identity: &Identity,
        client: Arc<DirectClient>,
        effects: &NodeEffects,
        to_loop: mpsc::UnboundedSender<LoopEvent>,
        held_connections: HeldConnections,
    ) -> Self {
        let own_card = wire::PublicKey::from(identity.card_key.public());
        Self {
            own_card,
            public: identity.public.clone(),
            own_handle: OnceLock::new(),
            own_address: Box::new(move || wire_addr(&client.endpoint().addr())),
            inbox: Arc::clone(&effects.inbox),
            attached: effects.inbox.attached(),
            clock: Arc::clone(&effects.clock),
            to_loop,
            stopping: AtomicBool::new(false),
            rows: PeerRows::new(own_card, held_connections, Arc::clone(&effects.report)),
        }
    }

    /// Milliseconds since the Unix epoch by the clock the composition root handed in.
    pub(super) fn now_ms(&self) -> u64 {
        (self.clock)()
    }

    /// Sets the peer Unreachable after a send to it failed, and hands the announcement of that to the presence loop.
    pub(super) fn contact_failed(&self, card: wire::PublicKey) {
        let now_ms = self.now_ms();
        let effects = self
            .rows
            .change(|table| table.direct_contact_failed(card, now_ms, FailedContactKind::Send));
        self.hand_to_loop(LoopEvent::Effects {
            effects,
            #[cfg(test)]
            cause: RequestCause::SendFailed(card),
        });
    }

    /// Sets the peer Online after a send to it was acknowledged, and wakes the presence loop, since an Online peer lets this member rebind.
    pub(super) fn contact_succeeded(&self, card: wire::PublicKey) {
        let now_ms = self.now_ms();
        self.rows
            .change(|table| table.direct_contact_succeeded(card, now_ms, ContactSucceeded::Send));
        self.hand_to_loop(LoopEvent::PeerReached);
    }

    /// The peer table, locked for one operation.
    pub(super) fn table(&self) -> MutexGuard<'_, PeerTable> {
        self.rows.table()
    }

    /// The full current address of this node's endpoint, within the caps of spec 3.6.
    fn own_addr(&self) -> Addr {
        (self.own_address)()
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
        announced_sender_handle(message)?;
        let (effects, from) = self.rows.change(|table| {
            let Operated { value, changes } = table.receive_message(sender, message, now_ms);
            let from = table
                .peers(now_ms)
                .find(|peer| peer.card == sender)
                .map(|peer| peer.effective_handle.clone());
            Operated {
                value: (value, from),
                changes,
            }
        });
        self.hand_to_loop(LoopEvent::Effects {
            effects,
            #[cfg(test)]
            cause: RequestCause::Message {
                sender,
                id: message.id,
            },
        });
        from
    }
}

impl DirectHost for Arc<NodeState> {
    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn own_card(&self) -> Option<wire::PublicKey> {
        Some(self.own_card)
    }

    fn is_attached(&self) -> bool {
        self.attached.is_attached()
    }

    fn receive_message(&self, sender: wire::PublicKey, message: Message) -> bool {
        let now_ms = self.now_ms();
        let Some(from) = self.take_message(sender, &message, now_ms) else {
            return self.is_attached();
        };
        self.inbox.publish(ReceivedMessage {
            id: message.id,
            from: from.as_str().to_owned(),
            body: message.body,
            sent_at: message.sent_at,
            received_at: now_ms,
        })
    }

    fn receive_introduce(&self, sender: wire::PublicKey, introduce: Introduce) -> IntroduceAck {
        let now_ms = self.now_ms();
        let effects = self
            .rows
            .change(|table| table.receive_introduce(sender, &introduce, now_ms));
        self.hand_to_loop(LoopEvent::Effects {
            effects,
            #[cfg(test)]
            cause: RequestCause::Introduce(sender),
        });
        IntroduceAck::from(self.introduction())
    }
}

impl NodeState {
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
}

/// The node's requester for the requests of one wake of its loop, handing each answer back to the loop.
struct LoopRequests {
    state: Arc<NodeState>,
    #[cfg(test)]
    cause: Option<RequestCause>,
}

impl DirectRequester for LoopRequests {
    fn is_stopping(&self) -> bool {
        self.state.is_stopping()
    }

    fn introduction(&self) -> Introduce {
        self.state.introduction()
    }

    fn hand_answer(&self, answer: DirectAnswer) {
        self.state.hand_to_loop(LoopEvent::Answered {
            answer,
            #[cfg(test)]
            cause: self.cause,
        });
    }
}

/// What reaches the presence loop from the direct server and from the requests it spawned.
enum LoopEvent {
    Depart,
    Effects {
        effects: Vec<Effect>,
        #[cfg(test)]
        cause: RequestCause,
    },
    PeerReached,
    Answered {
        answer: DirectAnswer,
        #[cfg(test)]
        cause: Option<RequestCause>,
    },
}

/// Whether the presence loop still watches the inbox's attached flag, which it stops doing once the inbox is gone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum InboxWatch {
    Watching,
    Gone,
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
    RebindDue(RebindCause),
    MemberJoinDue,
    SnapshotDue,
}

/// The one task that owns the topic and performs every effect of the events/actions table of spec 3.8, the rebind included.
pub(super) struct PresenceLoop {
    pub(super) state: Arc<NodeState>,
    card_key: SecretKey,
    pub(super) endpoint: NodeEndpoint,
    pub(super) router: Router,
    pub(super) gossip: Gossip,
    pub(super) keys: NetworkKeys,
    pub(super) owner: iroh::PublicKey,
    pub(super) client: Arc<DirectClient>,
    pub(super) topic: NetworkTopic,
    from_hosts: mpsc::UnboundedReceiver<LoopEvent>,
    attached: Attached,
    inbox: InboxWatch,
    pub(super) own_addresses: Changes<EndpointAddr>,
    pub(super) relay_statuses: Changes<Vec<RelayStatus>>,
    requested_handle: AnnouncedHandle,
    init: Init,
    announcer: Announcer,
    reannounce_due: Option<Instant>,
    heard_requests: HeardPresenceRequests,
    request_seqs: RequestSeqs,
    pub(super) relay_loss: RelayLossWatch,
    pub(super) known_hosts: Arc<KnownHosts>,
    pub(super) rebinding: Rebinding,
    fill_probe_nonce: ProbeNonceSource,
    pub(super) monotonic: MonotonicClock,
    pub(super) report: RunRecord,
    snapshot_due: Instant,
    #[cfg(test)]
    cause: Option<RequestCause>,
}

impl PresenceLoop {
    /// Serves the endpoint and joins the topic through the known hosts, returning the loop and the receiver of the handle its init assigns, or a `NodeError` when a known host is malformed or the topic cannot be joined.
    async fn join(
        endpoint: NodeEndpoint,
        identity: Identity,
        network: Network,
        effects: NodeEffects,
        state: Arc<NodeState>,
        client: Arc<DirectClient>,
        from_hosts: mpsc::UnboundedReceiver<LoopEvent>,
    ) -> Result<(Self, oneshot::Receiver<LocalHandle>), NodeError> {
        let hosts = known_host_addresses(&network.config)?;
        let seeded = seed_lookup_with_known_hosts(&endpoint, &hosts);
        let (router, gossip) = serve(&endpoint, &network.keys, &state);
        let subscribed = effects.monotonic.now();
        let topic = NetworkTopic::join(
            &gossip,
            network.keys.clone(),
            network.owner,
            hosts.iter().map(|host| host.id).collect(),
            Arc::clone(&effects.monotonic),
        )
        .await
        .map_err(NodeError::Topic)?;
        let (assigned, own_handle) = oneshot::channel();
        let own_addresses = own_address_changes(endpoint.endpoint());
        let relay_statuses = home_relay_changes(endpoint.endpoint());
        let presence_loop = Self {
            state,
            card_key: identity.card_key,
            endpoint,
            router,
            gossip,
            keys: network.keys,
            owner: network.owner,
            client,
            topic,
            from_hosts,
            attached: effects.inbox.attached(),
            inbox: InboxWatch::Watching,
            own_addresses,
            relay_statuses,
            requested_handle: identity.requested_handle,
            init: Init::AwaitingJoin(PendingHandle {
                assigned,
                held_introduces: Vec::new(),
            }),
            announcer: Announcer::default(),
            reannounce_due: None,
            heard_requests: HeardPresenceRequests::default(),
            request_seqs: RequestSeqs::default(),
            relay_loss: RelayLossWatch::default(),
            known_hosts: Arc::new(KnownHosts::new(network.config, network.config_path)),
            rebinding: Rebinding::new(subscribed, effects.probe_relay),
            fill_probe_nonce: effects.fill_probe_nonce,
            monotonic: effects.monotonic,
            report: effects.report,
            snapshot_due: subscribed,
            #[cfg(test)]
            cause: None,
        };
        for card in seeded {
            presence_loop.record_lookup_of(&card);
        }
        Ok((presence_loop, own_handle))
    }

    /// Records the counters at start, then handles every wake until the topic closes.
    async fn run(mut self) {
        self.record_state(SnapshotMoment::Start);
        loop {
            let wake = self.next_wake().await;
            if self.take(wake).await.is_break() {
                return;
            }
        }
    }

    /// Waits for the next event, change or deadline.
    async fn next_wake(&mut self) -> Wake {
        let observer_wait_over = match &self.init {
            Init::ObserverWait { until, .. } => Some(*until),
            Init::AwaitingJoin { .. } | Init::Done => None,
        };
        let relay_loss_due = self.relay_loss.next_deadline();
        let next_rebind = self.may_rebind().then(|| self.rebinding.next_rebind());
        let member_join_due = self.rebinding.next_ask();
        tokio::select! {
            event = next_recorded_gossip_event(
                &mut self.topic,
                Some((&self.state.rows, &*self.state.clock)),
                &*self.report,
            ) => Wake::Topic(event),
            Some(event) = self.from_hosts.recv() => Wake::Loop(event),
            changed = self.attached.changed(), if self.inbox == InboxWatch::Watching => match changed {
                Ok(_) => Wake::AttachedChanged,
                Err(_) => Wake::InboxGone,
            },
            Some(_) = self.own_addresses.next() => Wake::OwnAddressChanged,
            Some(statuses) = self.relay_statuses.next() => Wake::HomeRelaysChanged(statuses),
            () = deadline_reached(&*self.monotonic, observer_wait_over) => Wake::ObserverWaitOver,
            () = deadline_reached(&*self.monotonic, self.reannounce_due) => Wake::ReannounceDue,
            () = deadline_reached(&*self.monotonic, relay_loss_due) => Wake::RelayLossDue,
            cause = rebind_cause_once_due(&*self.monotonic, next_rebind) => Wake::RebindDue(cause),
            () = deadline_reached(&*self.monotonic, member_join_due) => Wake::MemberJoinDue,
            () = self.monotonic.reached(self.snapshot_due) => Wake::SnapshotDue,
        }
    }

    /// Acts on one wake, stopping once the topic has closed or a rebind left no endpoint or topic.
    async fn take(&mut self, wake: Wake) -> ControlFlow<()> {
        #[cfg(test)]
        {
            self.cause = None;
        }
        match wake {
            Wake::Topic(None) => {
                (self.report)(crate::event::GossipEvent::TopicClosed.into());
                return ControlFlow::Break(());
            }
            Wake::Topic(Some(Err(failure))) => {
                (self.report)(crate::event::GossipEvent::TopicFailed(failure).into());
            }
            Wake::Topic(Some(Ok(event))) => self.take_gossip_event(event).await,
            Wake::Loop(event) => self.take_loop_event(event).await,
            Wake::AttachedChanged => self.announce_presence().await,
            Wake::InboxGone => self.inbox = InboxWatch::Gone,
            Wake::OwnAddressChanged => self.reconsider_own_address().await,
            Wake::HomeRelaysChanged(statuses) => {
                self.endpoint.record_home_relays(&statuses);
                self.relay_loss
                    .observe(&home_relays(&statuses), self.monotonic.now());
                self.reconsider_own_address().await;
            }
            Wake::ObserverWaitOver => self.advance_init(InitEvent::ObserverWaitOver).await,
            Wake::ReannounceDue => {
                self.reannounce_due = None;
                self.reconsider_own_address().await;
            }
            Wake::RelayLossDue => self.remove_lost_relays().await,
            Wake::RebindDue(cause) => return self.rebind(cause).await,
            Wake::MemberJoinDue => self.ask_members_to_join().await,
            Wake::SnapshotDue => self.record_state(SnapshotMoment::Interval),
        }
        ControlFlow::Continue(())
    }

    /// Applies the rows of spec 3.8 a topic event triggers: join, join timeout, neighbor up or down, payload.
    async fn take_gossip_event(&mut self, event: GossipEvent) {
        let now_ms = self.state.now_ms();
        match event {
            GossipEvent::Joined => self.take_joined().await,
            GossipEvent::JoinTimedOut => self.take_join_timeout().await,
            GossipEvent::NeighborUp(node) => {
                #[cfg(test)]
                {
                    self.cause = Some(RequestCause::NeighborUp(wire::PublicKey::from(node)));
                }
                self.rebinding.neighbor_up(node);
                let effects = self.state.rows.take_neighbor_up(node, now_ms);
                self.perform(effects).await;
            }
            GossipEvent::NeighborDown(node) => {
                #[cfg(test)]
                {
                    self.cause = Some(RequestCause::NeighborDown {
                        card: wire::PublicKey::from(node),
                        earlier_probes: self
                            .state
                            .rows
                            .probes_for_neighbor_downs_of(wire::PublicKey::from(node)),
                    });
                }
                self.rebinding.neighbor_down(node);
                let effects = self.state.rows.take_neighbor_down(node, now_ms);
                self.perform(effects).await;
            }
            GossipEvent::Payload(payload) => {
                self.rebinding.heard(self.monotonic.now());
                self.take_payload(*payload, now_ms).await;
            }
        }
    }

    /// Asks every member to announce itself at every join, then starts the observer wait at the first join, announces at every later join, and starts the member join wait after a rebind's.
    async fn take_joined(&mut self) {
        self.rebinding.joined(self.monotonic.now());
        self.request_presence().await;
        self.advance_init(InitEvent::Joined {
            at: self.monotonic.now(),
        })
        .await;
    }

    /// Broadcasts a request that every member announce itself at once, signed by this node's card with a seq above its last request's, unless this node is stopping.
    async fn request_presence(&mut self) {
        if self.state.is_stopping() {
            return;
        }
        let seq = self.request_seqs.advance(self.state.now_ms());
        self.broadcast(presence_request(self.state.own_card, seq))
            .await;
    }

    /// Assigns the handle against the table as it stands when the first join gains no neighbor, and has a join that gains none end in a rebind when this member may rebind then.
    async fn take_join_timeout(&mut self) {
        if self.may_rebind() {
            self.rebinding.join_timed_out(self.monotonic.now());
        }
        self.advance_init(InitEvent::JoinTimedOut).await;
    }

    /// Moves init on by the event and does what that step asks: assigns the handle, announces, or both in that order, then sends the Introduces init held.
    async fn advance_init(&mut self, event: InitEvent) {
        #[cfg(test)]
        {
            self.cause = Some(RequestCause::HandleAssigned);
        }
        let (init, action) = std::mem::replace(&mut self.init, Init::Done).after(event);
        self.init = init;
        match action {
            None => {}
            Some(InitAction::Announce) => self.announce_presence().await,
            Some(InitAction::AssignHandle(pending)) => {
                self.assign_own_handle(pending.assigned);
                self.introduce_to(pending.held_introduces);
            }
            Some(InitAction::AssignHandleThenAnnounce(pending)) => {
                self.assign_own_handle(pending.assigned);
                self.announce_presence().await;
                self.introduce_to(pending.held_introduces);
            }
        }
    }

    /// Introduces this node to the cards in their order, in one task.
    fn introduce_to(&self, cards: Vec<wire::PublicKey>) {
        self.send_requests(
            cards
                .into_iter()
                .map(DirectRequest::SendIntroduce)
                .collect(),
        );
    }

    /// Sends the direct requests in their order, in one task.
    fn send_requests(&self, requests: Vec<DirectRequest>) {
        #[cfg(test)]
        self.state.rows.note_direct_exchanges(
            requests
                .iter()
                .filter_map(|request| request.made(self.cause)),
        );
        let requester = LoopRequests {
            state: Arc::clone(&self.state),
            #[cfg(test)]
            cause: self.cause,
        };
        send_in_background(requester, &self.client, requests);
    }

    /// Assigns this node's effective handle against the table and hands it to init.
    fn assign_own_handle(&self, assigned: oneshot::Sender<LocalHandle>) {
        let now_ms = self.state.now_ms();
        let own_handle = self
            .state
            .rows
            .change(|table| table.assign_own_handle(&self.requested_handle, now_ms));
        drop(self.state.own_handle.set(own_handle.clone()));
        drop(assigned.send(own_handle));
    }

    /// Applies the row of spec 3.8 for a verified payload: Presence, Departure, Unreachable or Infrastructure.
    async fn take_payload(&mut self, payload: OpenedPayload, now_ms: u64) {
        match payload {
            OpenedPayload::Presence(presence) => {
                #[cfg(test)]
                {
                    self.cause = Some(RequestCause::Presence(presence.card));
                }
                let effects = self.state.rows.take_presence(&presence, now_ms);
                self.perform(effects).await;
            }
            OpenedPayload::Departure(departure) => {
                #[cfg(test)]
                {
                    self.cause = Some(RequestCause::Departure(departure.card));
                }
                let effects = self.state.rows.take_departure(&departure, now_ms);
                self.perform(effects).await;
            }
            OpenedPayload::Unreachable(report) if report.card == self.state.own_card => {
                let admitted = rumor_admission(report.ts, self.announcer.last_seq(), now_ms);
                self.record_admission(report.facts(), admitted, now_ms);
                if admitted.is_ok() {
                    self.announce_presence().await;
                }
            }
            OpenedPayload::Unreachable(report) => {
                self.state.rows.take_unreachable(&report, now_ms);
            }
            OpenedPayload::Infrastructure(record) => {
                self.known_hosts
                    .take_infrastructure(
                        &record,
                        now_ms,
                        &self.endpoint,
                        RelayOfRecord::AddToRelayMap,
                        &*self.report,
                    )
                    .await;
            }
            OpenedPayload::PresenceRequest(request) => {
                self.answer_presence_request(&request, now_ms).await;
            }
        }
    }

    /// Sends Presence at once for a request within the announcement max age and above its requester's last seq, recording whether the request was taken.
    async fn answer_presence_request(&mut self, request: &PresenceRequest, now_ms: u64) {
        let admitted = self.heard_requests.admit(request, now_ms);
        self.record_admission(request.facts(), admitted, now_ms);
        if admitted.is_ok() {
            self.announce_presence().await;
        }
    }

    /// Records what the lookup entry of `card` now holds, naming the card by the handle the table holds for it: how many direct addresses and whether a relay, never where.
    pub(super) fn record_lookup_of(&self, card: &wire::PublicKey) {
        let handle = self.state.table().handle_of(*card, self.state.now_ms());
        if let Some(event) = lookup_replaced_event(&self.endpoint, card, handle) {
            (self.report)(event);
        }
    }

    /// Records whether a received payload was taken or dropped by the rule named, naming its signer by the handle the table holds for it, if any.
    fn record_admission(
        &self,
        facts: PayloadFacts,
        admission: Result<(), PayloadDropRule>,
        now_ms: u64,
    ) {
        (self.report)(named_admission_record(
            Some((&self.state.rows, now_ms)),
            facts,
            &admission,
        ));
    }

    /// Applies the effects the direct server handed over and the answer of each request this node sent.
    async fn take_loop_event(&mut self, event: LoopEvent) {
        let now_ms = self.state.now_ms();
        match event {
            LoopEvent::Depart => self.depart(now_ms).await,
            LoopEvent::Effects {
                effects,
                #[cfg(test)]
                cause,
            } => {
                #[cfg(test)]
                {
                    self.cause = Some(cause);
                }
                self.perform(effects).await;
            }
            LoopEvent::PeerReached => {}
            LoopEvent::Answered {
                answer,
                #[cfg(test)]
                cause,
            } => {
                #[cfg(test)]
                let answered = answer.answered(cause);
                #[cfg(test)]
                {
                    self.cause = Some(answer.as_cause());
                }
                if let Some(record) = answer.failure_record() {
                    (self.report)(record);
                }
                let effects = self.state.rows.take_direct_answer(answer, now_ms);
                self.perform(effects).await;
                #[cfg(test)]
                self.state.rows.note_direct_exchanges([answered]);
            }
        }
    }

    /// Performs the effects: lookup and broadcast here, direct requests in one task in their order, each Introduce held while init has not assigned the handle.
    async fn perform(&mut self, effects: Vec<Effect>) {
        let mut requests = Vec::new();
        for effect in effects {
            match effect {
                Effect::ReplaceLookupEntry { card, addr } => {
                    replace_lookup_entry(&self.endpoint, &card, &addr);
                    self.record_lookup_of(&card);
                }
                Effect::BroadcastUnreachable(card) => self.broadcast_unreachable(card).await,
                Effect::DropHeldConnection(card) => {
                    requests.push(DirectRequest::DropHeldConnection(card));
                }
                Effect::SendIntroduce(card) => requests.extend(
                    self.init
                        .introduce_now_or_hold(card)
                        .map(DirectRequest::SendIntroduce),
                ),
                Effect::SendProbe(card) => {
                    let mut nonce = Nonce::default();
                    (self.fill_probe_nonce)(&mut nonce);
                    requests.push(DirectRequest::SendProbe(card, nonce));
                }
            }
        }
        self.send_requests(requests);
    }

    /// Broadcasts the Departure and announces nothing after it, then records the stop's counters.
    async fn depart(&mut self, now_ms: u64) {
        let seq = self.announcer.next_seq(now_ms);
        self.broadcast(Payload::Departure(Departure {
            v: Version::CURRENT,
            card: self.state.own_card,
            seq,
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        }))
        .await;
        self.record_state(SnapshotMoment::Stop);
    }

    /// Records the socket and gossip counters at the moment, and takes the next snapshot the state snapshot interval later.
    pub(super) fn record_state(&mut self, moment: SnapshotMoment) {
        self.snapshot_due = snapshot_due_after(self.monotonic.now());
        record_counters(
            moment,
            self.endpoint.endpoint(),
            &self.gossip,
            &*self.report,
        );
    }

    /// Broadcasts this node's Presence with the next seq, once its handle is assigned and before any Departure.
    async fn announce_presence(&mut self) {
        let Some(own_handle) = self.state.own_handle.get() else {
            return;
        };
        if self.state.is_stopping() {
            return;
        }
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
            sig: ZERO_SIGNATURE,
        });
        self.announcer.record(seq, addr, self.monotonic.now());
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
            sig: ZERO_SIGNATURE,
        });
        self.broadcast(report).await;
    }

    /// Signs and broadcasts the payload under the card key, recording its kind and stamp once sent and a failure otherwise.
    async fn broadcast(&self, payload: Payload) {
        broadcast_recording_outcome(&self.topic, &self.card_key, payload, &*self.report).await;
    }

    /// Announces a changed address now, or once the re-announce interval since the last Presence has passed.
    async fn reconsider_own_address(&mut self) {
        let now = self.monotonic.now();
        match self.announcer.reannounce_step(&self.state.own_addr(), now) {
            ReannounceStep::Unchanged => {}
            ReannounceStep::AnnounceNow => self.announce_presence().await,
            ReannounceStep::AnnounceAt(due) => self.put_off_announcement_until(due, now),
        }
    }

    /// Takes the re-announce wake to `due`, recording the put-off once, when it moves the wake.
    fn put_off_announcement_until(&mut self, due: Instant, now: Instant) {
        if self.reannounce_due != Some(due) {
            (self.report)(
                crate::event::GossipEvent::PresenceDeferred {
                    deferred_ms: milliseconds_between(now, due),
                }
                .into(),
            );
        }
        self.reannounce_due = Some(due);
    }

    /// Removes every home relay still disconnected the relay-loss wait after it was lost.
    async fn remove_lost_relays(&mut self) {
        let statuses = self.endpoint.endpoint().home_relay_status().get();
        for relay in self
            .relay_loss
            .lost_relays(&home_relays(&statuses), self.monotonic.now())
        {
            let host = self.known_hosts.host_with_relay(&relay);
            self.endpoint.remove_relay(&relay, host).await;
        }
    }
}

/// A request from the requester's card that every member announce itself at once, carrying the seq; the broadcast signs and macs it.
pub const fn presence_request(requester: wire::PublicKey, seq: u64) -> Payload {
    Payload::PresenceRequest(PresenceRequest {
        v: Version::CURRENT,
        requester,
        seq,
        mac: ZERO_MAC,
        sig: ZERO_SIGNATURE,
    })
}

/// The whole milliseconds from `earlier` to `later`, none when `later` is not after `earlier`.
fn milliseconds_between(earlier: Instant, later: Instant) -> u64 {
    u64::try_from(later.saturating_duration_since(earlier).as_millis()).unwrap_or(u64::MAX)
}

/// When a node of any kind next records its counters after `now`, so its events file is written at least every state snapshot interval.
pub fn snapshot_due_after(now: Instant) -> Instant {
    later_by(now, STATE_SNAPSHOT_INTERVAL)
}

/// Records the counters iroh keeps for the endpoint's socket and for the gossip at the moment.
pub fn record_counters(
    moment: SnapshotMoment,
    endpoint: &BoundEndpoint,
    gossip: &Gossip,
    report: &(dyn Fn(Event) + Send + Sync),
) {
    let snapshot = CounterSnapshot::read(moment, endpoint.socket_metrics(), gossip.metrics());
    report(crate::event::StateEvent::Counters(snapshot).into());
}

/// Waits until the clock reads the deadline, or forever when there is none.
pub async fn deadline_reached(clock: &dyn MonotonicTime, deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => clock.reached(deadline).await,
        None => std::future::pending().await,
    }
}

/// Waits until the clock reads the instant the rebind is due and answers why, or forever when none is.
async fn rebind_cause_once_due(
    clock: &dyn MonotonicTime,
    next_rebind: Option<(Instant, RebindCause)>,
) -> RebindCause {
    match next_rebind {
        Some((due, cause)) => {
            clock.reached(due).await;
            cause
        }
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

#[cfg(test)]
#[path = "presence_events_tests.rs"]
mod events_tests;

#[cfg(test)]
pub(super) mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use iroh::RelayUrl;
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::*;
    use crate::clock::{RuntimeClock, later_by};
    use crate::config::{Host, HostOrigin, NetworkConfig};
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use crate::dirs::{CardsDirectoryOverride, Directories, OperatingSystemFilesystem};
    use crate::event::{DirectEvent, Event};
    use crate::handle::AnnouncedHandle;
    use crate::inbox::socket::{InboxSocket, ProcessTable};
    use crate::keys::render_public_key;
    use crate::net::direct::tests::{
        INTERVAL_MOMENT, INTERVAL_SNAPSHOT_WITHIN, PAYLOAD_ACCEPTED, SPEC_DEAF_TIMEOUT,
        SPEC_STATE_SNAPSHOT_INTERVAL, STATE_COUNTERS, ShiftedClock, StandingClock,
        closed_within_the_documented_latency, recorded_until_introduce_sent_to, was_taken_at,
    };
    use crate::net::direct::{DirectError, GONE_PROCESS_SILENCE, HEARING_SAMPLE_INTERVAL};
    use crate::net::endpoint::NodeEndpoint;
    use crate::node::direct_requests::{DirectContact, DirectExchange, DirectOutcome};
    use crate::peers::Status;
    use crate::seeded_draws::Draws;
    use crate::test_waits::{
        PROBE_NONCE, RecordedEvent, WaitMissed, found_within, next_recorded_meeting,
        recorded_events, recorded_until, secret_key, short_id_field, wall_clock_ms,
    };
    use crate::wire::{Infrastructure, SignatureBytes};

    const NETWORK_SECRET: [u8; KEY_LEN] = [33; KEY_LEN];
    pub(in crate::node) const OWNER_SEED: u8 = 60;
    pub(in crate::node) const FIRST_SEED: u8 = 61;
    pub(in crate::node) const SECOND_SEED: u8 = 62;
    pub(in crate::node) const FIRST_HANDLE: &str = "amy";
    pub(in crate::node) const SECOND_HANDLE: &str = "bob";
    pub(in crate::node) const FIRST_PUBLIC: &[u8] = br#"{"role":"first"}"#;
    pub(in crate::node) const SECOND_PUBLIC: &[u8] = br#"{"role":"second"}"#;
    const UNREACHABLE_RELAY: &str = "https://relay.estafette.invalid/";
    const SPEC_JOIN_TIMEOUT_PLUS_OBSERVER_WAIT: Duration = Duration::from_millis(8000);
    pub(in crate::node) const NETWORK_FILE_NAME: &str = "network.toml";
    const NOT_A_KEY: &str = "not a key";
    /// A third of the observer wait, the span in which a handle is assigned once the handed clock reads the wait.
    const ASSIGNED_ONCE_THE_CLOCK_READS_THE_WAIT_WITHIN: Duration = Duration::from_millis(1000);
    pub(in crate::node) const SPEC_OBSERVER_WAIT: Duration = Duration::from_millis(3000);
    #[cfg(feature = "relay")]
    const HOME_RELAY_STATUS_WITHIN: Duration = Duration::from_secs(10);
    #[cfg(feature = "relay")]
    const RELAY_LOSS_REMOVAL_SLACK: Duration = Duration::from_millis(1500);
    const NODE_TESTS_AT_ONCE: usize = 2;
    const INBOX_FRAME_WAIT: Duration = Duration::from_secs(2);
    const MESSAGE_ID: crate::wire::MessageId = [
        0x01, 0x8f, 0x3a, 0x7c, 0x00, 0x00, 0x70, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x2a,
    ];
    const MESSAGE_ID_TEXT: &str = "018f3a7c-0000-7000-8000-00000000002a";
    const MESSAGE_SENT_AT: u64 = 1_700_000_123_456;
    const MESSAGE_BODY: &str = "a message before any presence";
    pub(in crate::node) const INTRODUCE_ROUND_TRIP: Duration = Duration::from_millis(1500);
    pub(in crate::node) const THIRD_SEED: u8 = 63;
    pub(in crate::node) const THIRD_HANDLE: &str = "cal";
    pub(in crate::node) const THIRD_PUBLIC: &[u8] = br#"{"role":"third"}"#;
    pub(in crate::node) const SECOND_EFFECTIVE_FIRST_HANDLE: &str = "amy-2";
    pub(in crate::node) const WITNESS_SEED: u8 = 70;
    pub(in crate::node) const WITNESS_PUBLIC: &[u8] = br#"{"role":"witness"}"#;
    const WITNESS_JOIN_WAIT: Duration = Duration::from_secs(8);
    pub(in crate::node) const MARKER_SEED: u8 = 80;
    pub(in crate::node) const SECOND_MARKER_SEED: u8 = 81;
    pub(in crate::node) const MARKER_HANDLE: &str = "mark";
    pub(in crate::node) const MARKER_PUBLIC: &[u8] = br#"{"role":"marker"}"#;
    pub(in crate::node) const REPLAYED_PUBLIC: &[u8] = br#"{"role":"replayed"}"#;
    pub(in crate::node) const PAYLOAD_WAIT: Duration = Duration::from_secs(3);
    pub(in crate::node) const MOVED_HOST_RELAY: &str =
        "https://relay-moved-host.estafette.invalid/";
    pub(in crate::node) const NOT_AN_ANNOUNCED_HANDLE: &str = "Bob";
    const LATER_MESSAGE_BODY: &str = "a message under the announced handle";
    const MISMATCHED_MESSAGE_BODY: &str = "a message under a handle that does not match";
    const IMPOSTOR_HANDLE: &str = "imp";
    pub(in crate::node) const HOST_SEED: u8 = 90;
    const REPLAY_DIRECT_ADDRESS: &str = "203.0.113.9:4433";
    const SECOND_HOST_SEED: u8 = 91;
    const THIRD_HOST_SEED: u8 = 92;
    pub(in crate::node) const HOST_RELAY: &str = "https://relay-host.estafette.invalid/";
    const SECOND_HOST_RELAY: &str = "https://relay-second-host.estafette.invalid/";
    const THIRD_HOST_RELAY: &str = "https://relay-third-host.estafette.invalid/";
    const SPEC_HOST_ADDED_WITHIN: Duration = Duration::from_millis(1000);
    pub(in crate::node) const SPEC_INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS: u64 = 300;
    pub(in crate::node) const SPEC_ANNOUNCEMENT_MAX_AGE_MS: u64 = 300_000;
    const SPEC_JOIN_TIMEOUT: Duration = Duration::from_millis(5000);
    pub(in crate::node) const FROZEN_CLOCK_MS: u64 = 1_800_000_000_000;
    const CLOCK_AFTER_MAX_AGE_MS: u64 = FROZEN_CLOCK_MS + 400_000;
    const STALE_RUMOR_TS: u64 = CLOCK_AFTER_MAX_AGE_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS - 1;
    const FRESH_RUMOR_TS: u64 = CLOCK_AFTER_MAX_AGE_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS;
    pub(in crate::node) const WITNESS_HANDLE: &str = "xen";
    const SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT: Duration = Duration::from_millis(5000);
    const TWO_UNREACHABLE_INTRODUCES_FAIL_WITHIN: Duration = SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT
        .saturating_add(SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT)
        .saturating_add(PAYLOAD_WAIT);
    const PROBE_ANSWERED_WAIT: Duration = Duration::from_secs(5);
    const AFTER_PROBE_ACK: Duration = Duration::from_millis(300);
    pub(in crate::node) const KIND: &str = "kind";
    pub(in crate::node) const PRESENCE_KIND: &str = "presence";
    pub(in crate::node) const PRESENCE_REQUEST_KIND: &str = "presence_request";
    const PAYLOAD_DROPPED: &str = "gossip.payload_dropped";
    const RULE: &str = "rule";
    const NOT_ABOVE_REQUESTER_SEQ_RULE: &str = "not_above_requester_seq";
    const OLDER_THAN_MAX_AGE_RULE: &str = "older_than_max_age";
    const AHEAD_OF_MAX_AGE_RULE: &str = "ahead_of_max_age";
    const PAYLOAD_BROADCAST: &str = "gossip.payload_broadcast";
    /// The span after a request or report that a test waits out to show that no answer comes.
    const ANSWER_WINDOW: Duration = Duration::from_millis(1000);
    const TOPIC_JOINED: &str = "gossip.topic_joined";
    const UNREACHABLE_KIND: &str = "unreachable";
    pub(in crate::node) const PEER: &str = "peer";
    pub(in crate::node) const SEQ: &str = "seq";
    pub(in crate::node) const TS: &str = "ts";
    const NEIGHBOR_UP: &str = "gossip.neighbor_up";
    /// The longest the member takes to record a peer a gossip neighbor once the peer joined the topic through it.
    const NEIGHBOR_UP_RECORDED_WITHIN: Duration = Duration::from_secs(5);

    const FIRST_REQUESTER_SEED: u8 = 72;
    const SECOND_REQUESTER_SEED: u8 = 73;
    const REPLAYED_REQUEST_SEQ: u64 = FROZEN_CLOCK_MS - 1;
    const STALE_REQUEST_SEQ: u64 = FROZEN_CLOCK_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS - 1;
    const FAR_FUTURE_REQUEST_SEQ: u64 = FROZEN_CLOCK_MS + SPEC_ANNOUNCEMENT_MAX_AGE_MS + 1;
    const ANSWERED_AT_ONCE_WITHIN: Duration = Duration::from_millis(100);
    const HELD_INTRODUCE_SENT_WITHIN: Duration =
        SPEC_JOIN_TIMEOUT_PLUS_OBSERVER_WAIT.saturating_add(INTRODUCE_ROUND_TRIP);
    const OWN_HANDLE_ASSIGNED: &str = "peers.own_handle_assigned";
    const CONTACT_FAILED: &str = "direct.contact_failed";

    fn network_secret() -> NetworkSecret {
        NetworkSecret::from_bytes(NETWORK_SECRET)
    }

    pub(in crate::node) fn keys() -> NetworkKeys {
        NetworkKeys::derive(&network_secret())
    }

    pub(in crate::node) fn card(seed: u8) -> crate::wire::PublicKey {
        crate::wire::PublicKey::from(secret_key(seed).public())
    }

    struct EveryProcessRuns;

    impl ProcessTable for EveryProcessRuns {
        fn is_running(&self, _process_id: u32) -> std::io::Result<bool> {
            Ok(true)
        }
    }

    static NODE_TEST_SLOTS: tokio::sync::Semaphore =
        tokio::sync::Semaphore::const_new(NODE_TESTS_AT_ONCE);

    pub(in crate::node) struct Scratch {
        root: PathBuf,
        directories: Directories,
        _slot: tokio::sync::SemaphorePermit<'static>,
    }

    impl Scratch {
        pub(in crate::node) async fn new(tag: &str) -> Self {
            let slot = NODE_TEST_SLOTS.acquire().await.unwrap();
            let root = crate::this_process_directory::fresh_directory_of_this_process_in(
                &std::env::temp_dir(),
                &format!("u20-{tag}"),
            )
            .unwrap();
            let base = root.clone();
            let environment = move |name: &str| match name {
                "XDG_CONFIG_HOME" => Some(base.join("c").into_os_string()),
                "XDG_DATA_HOME" => Some(base.join("d").into_os_string()),
                "XDG_RUNTIME_DIR" => Some(base.join("r").into_os_string()),
                _ => None::<OsString>,
            };
            let directories = Directories::establish(
                environment,
                &OperatingSystemFilesystem::of_this_process().unwrap(),
                CardsDirectoryOverride::Offered(None),
            )
            .unwrap();
            Self {
                root,
                directories,
                _slot: slot,
            }
        }

        pub(in crate::node) fn locked_card(
            &self,
            seed: u8,
        ) -> (PathBuf, crate::sidecar::HeldSidecar) {
            let sidecar_path = self.root.join(format!("{seed}-sidecar"));
            crate::sidecar::create_card(
                &self.root.join(format!("{seed}-card.toml")),
                &sidecar_path,
                &"card".parse().unwrap(),
                [seed; KEY_LEN],
            )
            .unwrap();
            let held = crate::sidecar::claim(
                &sidecar_path,
                card(seed).as_bytes(),
                &crate::sidecar::ThreadSleep,
            )
            .unwrap();
            (sidecar_path, held)
        }

        fn network_file(&self, seed: u8) -> PathBuf {
            self.root.join(format!("{seed}-{NETWORK_FILE_NAME}"))
        }

        fn inbox(&self, seed: u8) -> Arc<InboxSocket> {
            Arc::new(
                InboxSocket::start(
                    &self.directories,
                    u32::from(seed),
                    &EveryProcessRuns,
                    |_failure| {},
                )
                .unwrap(),
            )
        }

        /// The inbox socket of the seed's member, its events handed to the record and its clients served by a runtime on a thread of its own, and the sender whose drop ends that thread.
        pub(in crate::node) fn inbox_served_apart(
            &self,
            seed: u8,
            record: impl Fn(crate::event::InboxEvent) + Send + Sync + 'static,
        ) -> (Arc<InboxSocket>, tokio::sync::oneshot::Sender<()>) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let socket = {
                let _entered = runtime.enter();
                Arc::new(
                    InboxSocket::start(
                        &self.directories,
                        u32::from(seed),
                        &EveryProcessRuns,
                        record,
                    )
                    .unwrap(),
                )
            };
            let (serving, ends) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || runtime.block_on(ends).unwrap_or_default());
            (socket, serving)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            crate::scratch_removal::remove_when_the_test_ends(&self.root);
        }
    }

    pub(in crate::node) async fn bind(seed: u8) -> NodeEndpoint {
        bind_holding(seed, crate::net::direct::tests::unrecorded_register()).await
    }

    /// Binds the endpoint of the seed, holding every connection its gate lets through in the register.
    pub(in crate::node) async fn bind_holding(
        seed: u8,
        connections: crate::net::direct::HeldConnections,
    ) -> NodeEndpoint {
        NodeEndpoint::bind(
            secret_key(seed),
            *keys().gate_key(),
            Vec::new(),
            move |nonce: &mut [u8; KEY_LEN]| *nonce = [seed; KEY_LEN],
            |_ignored| {},
            connections,
        )
        .await
        .unwrap()
    }

    fn configured_host(seed: u8, relay: &str) -> Host {
        Host {
            id: render_public_key(&secret_key(seed).public()),
            relay: relay.to_owned(),
            ts: None,
            origin: HostOrigin::Configured,
        }
    }

    pub(in crate::node) fn config_with_hosts(hosts: Vec<Host>) -> NetworkConfig {
        NetworkConfig {
            secret: data_encoding::HEXLOWER.encode(&NETWORK_SECRET),
            owner: render_public_key(&secret_key(OWNER_SEED).public()),
            hosts,
        }
    }

    pub(in crate::node) struct MemberPlan {
        seed: u8,
        handle: &'static str,
        public: &'static [u8],
        hosts: Vec<u8>,
        host_relays: BTreeMap<u8, String>,
        reachable: Vec<u8>,
        clock: Clock,
        monotonic: MonotonicClock,
        report: RunRecord,
        inbox: Option<Arc<InboxSocket>>,
        probe_relay: RelayProbe,
        register: HeldConnections,
    }

    impl MemberPlan {
        pub(in crate::node) fn new(seed: u8, handle: &'static str, public: &'static [u8]) -> Self {
            Self {
                seed,
                handle,
                public,
                hosts: Vec::new(),
                host_relays: BTreeMap::new(),
                reachable: Vec::new(),
                clock: Arc::new(wall_clock_ms),
                monotonic: Arc::new(RuntimeClock),
                report: Arc::new(|_event: Event| {}),
                inbox: None,
                probe_relay: Arc::new(|_relay| Box::pin(async { true })),
                register: HeldConnections::new(
                    Arc::new(RuntimeClock),
                    crate::net::direct::tests::dropping_transport(),
                ),
            }
        }

        pub(in crate::node) fn with_register(self, register: HeldConnections) -> Self {
            Self { register, ..self }
        }

        pub(in crate::node) fn with_host_relays(self, hosts: &[(u8, &str)]) -> Self {
            Self {
                hosts: hosts.iter().map(|(seed, _)| *seed).collect(),
                host_relays: hosts
                    .iter()
                    .map(|(seed, relay)| (*seed, (*relay).to_owned()))
                    .collect(),
                ..self
            }
        }

        pub(in crate::node) fn with_relay_probe(self, probe_relay: RelayProbe) -> Self {
            Self {
                probe_relay,
                ..self
            }
        }

        pub(in crate::node) fn with_report(self, report: RunRecord) -> Self {
            Self { report, ..self }
        }

        pub(in crate::node) fn with_inbox(self, inbox: Arc<InboxSocket>) -> Self {
            Self {
                inbox: Some(inbox),
                ..self
            }
        }

        pub(in crate::node) fn with_hosts(self, hosts: &[u8]) -> Self {
            Self {
                hosts: hosts.to_vec(),
                ..self
            }
        }

        pub(in crate::node) fn reaching(self, reachable: &[u8]) -> Self {
            Self {
                reachable: reachable.to_vec(),
                ..self
            }
        }
    }

    pub(in crate::node) struct Member {
        pub(in crate::node) node: Node,
        pub(in crate::node) endpoint: BoundEndpoint,
        pub(in crate::node) inbox_path: PathBuf,
        pub(in crate::node) config_path: PathBuf,
    }

    pub(in crate::node) async fn start_members(
        scratch: &Scratch,
        plans: Vec<MemberPlan>,
    ) -> Vec<Member> {
        start_members_reaching(scratch, plans, &[]).await
    }

    pub(in crate::node) async fn start_members_reaching(
        scratch: &Scratch,
        plans: Vec<MemberPlan>,
        running: &[(u8, iroh::EndpointAddr)],
    ) -> Vec<Member> {
        let mut endpoints = Vec::new();
        for plan in &plans {
            endpoints.push(bind_holding(plan.seed, plan.register.clone()).await);
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
                let inbox = plan
                    .inbox
                    .clone()
                    .unwrap_or_else(|| scratch.inbox(plan.seed));
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
            Network::new(
                config_with_hosts(
                    plan.hosts
                        .iter()
                        .map(|&seed| {
                            configured_host(
                                seed,
                                plan.host_relays
                                    .get(&seed)
                                    .map_or(UNREACHABLE_RELAY, String::as_str),
                            )
                        })
                        .collect(),
                ),
                config_path.clone(),
            )
            .unwrap(),
            NodeEffects {
                clock: plan.clock,
                monotonic: plan.monotonic,
                fill_probe_nonce: Arc::new(|nonce: &mut crate::wire::Nonce| *nonce = PROBE_NONCE),
                report: plan.report,
                inbox,
                probe_relay: plan.probe_relay,
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

    /// A member's presence loop, built as `Node::start` builds it and not yet running, on the endpoint and effects of the plan and the network file of its seed with no host.
    pub(in crate::node) async fn idle_presence_loop(
        scratch: &Scratch,
        plan: MemberPlan,
    ) -> PresenceLoop {
        let node_endpoint = bind_holding(plan.seed, plan.register.clone()).await;
        let network = Network::new(
            config_with_hosts(Vec::new()),
            scratch.network_file(plan.seed),
        )
        .unwrap();
        let identity = Identity {
            card_key: secret_key(plan.seed),
            requested_handle: plan.handle.parse::<AnnouncedHandle>().unwrap(),
            public: plan.public.to_vec(),
        };
        let effects = NodeEffects {
            clock: plan.clock,
            monotonic: plan.monotonic,
            fill_probe_nonce: Arc::new(|nonce: &mut crate::wire::Nonce| *nonce = PROBE_NONCE),
            report: plan.report,
            inbox: scratch.inbox(plan.seed),
            probe_relay: plan.probe_relay,
        };
        let client = Arc::new(DirectClient::new(
            node_endpoint.endpoint().clone(),
            network.keys.clone(),
            ClientTimeouts::SPEC,
            node_endpoint.held_connections(),
        ));
        let (to_loop, from_hosts) = mpsc::unbounded_channel();
        let state = Arc::new(NodeState::new(
            &identity,
            Arc::clone(&client),
            &effects,
            to_loop,
            node_endpoint.held_connections(),
        ));
        let (presence_loop, _own_handle) = PresenceLoop::join(
            node_endpoint,
            identity,
            network,
            effects,
            state,
            client,
            from_hosts,
        )
        .await
        .unwrap();
        presence_loop
    }

    pub(in crate::node) async fn start_pair(scratch: &Scratch) -> (Member, Member) {
        start_pair_reporting_first(scratch, Arc::new(|_event: Event| {})).await
    }

    async fn start_pair_reporting_first(scratch: &Scratch, report: RunRecord) -> (Member, Member) {
        let mut members = start_members(
            scratch,
            vec![
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_hosts(&[SECOND_SEED])
                    .with_report(report),
                MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC)
                    .with_hosts(&[FIRST_SEED]),
            ],
        )
        .await;
        let second = members.pop().unwrap();
        let first = members.pop().unwrap();
        (first, second)
    }

    pub(in crate::node) async fn listed_within(
        member: &Member,
        seed: u8,
        within: Duration,
    ) -> Result<Peer, WaitMissed> {
        let awaited = format!("entry for the card of seed {seed} listed");
        entry_meeting_within(member, seed, &awaited, |_listed| true, within).await
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
            .unwrap_or_else(|missed| missed.fail_the_test());
        let seen_by_second = listed_within(&second, FIRST_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert_eq!(seen_by_first.effective_handle.as_str(), SECOND_HANDLE);
        assert_eq!(seen_by_first.public.as_deref(), Some(SECOND_PUBLIC));
        assert_eq!(seen_by_first.status, Status::Online);
        assert_eq!(seen_by_second.effective_handle.as_str(), FIRST_HANDLE);
        assert_eq!(seen_by_second.public.as_deref(), Some(FIRST_PUBLIC));
        assert_eq!(seen_by_second.status, Status::Online);
        drop(scratch);
    }

    pub(in crate::node) async fn inbox_frames(
        member: &Member,
        count: usize,
    ) -> Vec<serde_json::Value> {
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

    pub(in crate::node) fn message_to(seed: u8, handle: &str) -> Message {
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
        let sender = DirectClient::new(
            second.endpoint.clone(),
            keys(),
            ClientTimeouts::SPEC,
            crate::net::direct::tests::unrecorded_register(),
        );

        let introduce_answers_before =
            settled_direct_answers(&first, SECOND_SEED, DirectContact::Introduce).await;
        let message = message_to(FIRST_SEED, SECOND_HANDLE);
        let introduced_by = RequestCause::Message {
            sender: card(SECOND_SEED),
            id: message.id,
        };
        let ack = sender
            .send_message(first.endpoint.id(), message)
            .await
            .unwrap();

        assert!(ack.accepted);
        let listed = listed_within(&first, SECOND_SEED, Duration::ZERO)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert_eq!(listed.effective_handle.as_str(), SECOND_HANDLE);
        assert_eq!(listed.status, Status::Online);
        assert!(listed.attached);
        let frame = inbox_frames(&first, 1).await.remove(0);
        assert_eq!(frame["from"], SECOND_HANDLE);
        assert_eq!(frame["body"], MESSAGE_BODY);
        assert_eq!(frame["id"], MESSAGE_ID_TEXT);
        assert_eq!(frame["sent_at"], MESSAGE_SENT_AT);
        direct_answer_after(
            &first,
            SECOND_SEED,
            DirectContact::Introduce,
            introduce_answers_before,
            introduced_by,
            DirectOutcome::Acknowledged,
            INTRODUCE_ROUND_TRIP,
        )
        .await;
        let after_introduce = listed_within(&first, SECOND_SEED, Duration::ZERO)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert_eq!(after_introduce.status, Status::Online);
        assert_eq!(after_introduce.public.as_deref(), Some(SECOND_PUBLIC));
        drop(scratch);
    }

    struct WitnessHost {
        own_card: crate::wire::PublicKey,
        handle: Option<&'static str>,
        addr: Addr,
        probes_answered: Arc<std::sync::atomic::AtomicUsize>,
        introductions: Introductions,
    }

    type Introductions = Arc<tokio::sync::watch::Sender<Vec<(crate::wire::PublicKey, Introduce)>>>;

    #[derive(Debug)]
    struct CountingConnections {
        server: DirectServer<WitnessHost>,
        connections: Arc<std::sync::atomic::AtomicUsize>,
        accepted: Arc<std::sync::Mutex<Vec<iroh::endpoint::Connection>>>,
    }

    impl iroh::protocol::ProtocolHandler for CountingConnections {
        async fn accept(
            &self,
            connection: iroh::endpoint::Connection,
        ) -> Result<(), iroh::protocol::AcceptError> {
            self.connections
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.accepted.lock().unwrap().push(connection.clone());
            self.server.accept(connection).await
        }
    }

    impl DirectHost for WitnessHost {
        fn is_stopping(&self) -> bool {
            false
        }

        fn own_card(&self) -> Option<crate::wire::PublicKey> {
            Some(self.own_card)
        }

        fn is_attached(&self) -> bool {
            self.probes_answered
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            false
        }

        fn receive_message(&self, _sender: crate::wire::PublicKey, _message: Message) -> bool {
            false
        }

        fn receive_introduce(
            &self,
            sender: crate::wire::PublicKey,
            introduce: Introduce,
        ) -> IntroduceAck {
            self.introductions
                .send_modify(|taken| taken.push((sender, introduce)));
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

    pub(in crate::node) struct Witness {
        gossip: Gossip,
        topic: NetworkTopic,
        probes_answered: Arc<std::sync::atomic::AtomicUsize>,
        introductions: Introductions,
        direct_connections: Arc<std::sync::atomic::AtomicUsize>,
        pub(in crate::node) accepted: Arc<std::sync::Mutex<Vec<iroh::endpoint::Connection>>>,
        router: Router,
    }

    impl Witness {
        pub(in crate::node) async fn joined_through(
            seed: u8,
            handle: Option<&'static str>,
            through: &BoundEndpoint,
        ) -> Self {
            let node_endpoint = bind(seed).await;
            node_endpoint.replace_address(through.addr());
            let gossip = node_endpoint.endpoint().spawn_gossip();
            let probes_answered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let introductions = Introductions::default();
            let host = WitnessHost {
                own_card: card(seed),
                handle,
                addr: wire_addr(&node_endpoint.endpoint().addr()),
                probes_answered: Arc::clone(&probes_answered),
                introductions: Arc::clone(&introductions),
            };
            let direct_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let accepted = Arc::new(std::sync::Mutex::new(Vec::new()));
            let counting = CountingConnections {
                server: DirectServer::new(keys(), host, &node_endpoint),
                connections: Arc::clone(&direct_connections),
                accepted: Arc::clone(&accepted),
            };
            let router = node_endpoint
                .endpoint()
                .router_builder()
                .accept(DIRECT_ALPN, counting)
                .accept(GOSSIP_ALPN, gossip.clone())
                .spawn();
            let mut topic = NetworkTopic::join(
                &gossip,
                keys(),
                secret_key(OWNER_SEED).public(),
                vec![through.id()],
                Arc::new(RuntimeClock),
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
                introductions,
                direct_connections,
                accepted,
                router,
            }
        }

        /// The address the witness's endpoint is reached at.
        pub(in crate::node) fn address(&self) -> iroh::EndpointAddr {
            self.router.endpoint().addr()
        }

        /// The first Introduce the witness took from the card of the seed, waiting for one within the wait.
        #[must_use]
        pub(in crate::node) async fn first_introduce_from(
            &self,
            seed: u8,
            within: Duration,
        ) -> Option<Introduce> {
            let from_the_card =
                |(sender, _introduce): &(crate::wire::PublicKey, Introduce)| *sender == card(seed);
            let mut taken = self.introductions.subscribe();
            let taken = tokio::time::timeout(
                within,
                taken.wait_for(|taken| taken.iter().any(from_the_card)),
            )
            .await
            .ok()?
            .ok()?;
            taken
                .iter()
                .find(|entry| from_the_card(entry))
                .map(|(_sender, introduce)| introduce.clone())
        }

        /// How many Introduces the witness has taken from the card of the seed.
        pub(in crate::node) fn introduces_taken_from(&self, seed: u8) -> usize {
            self.introductions
                .borrow()
                .iter()
                .filter(|(sender, _introduce)| *sender == card(seed))
                .count()
        }

        /// Closes the witness's endpoint, so it leaves the topic and answers no direct request.
        pub(in crate::node) async fn close(&self) {
            self.router.shutdown().await.unwrap();
        }

        /// Leaves the topic while the returned router still answers direct requests.
        pub(in crate::node) fn leave_topic(self) -> Router {
            self.router
        }

        pub(in crate::node) async fn broadcast_as(&self, signer_seed: u8, payload: Payload) {
            self.topic
                .broadcast(&secret_key(signer_seed), payload)
                .await
                .unwrap();
        }

        /// Broadcasts a Presence of the marker card, which no member has met.
        pub(in crate::node) async fn broadcast_marker(&self, marker_seed: u8) {
            self.broadcast_as(
                marker_seed,
                presence_of(marker_seed, MARKER_HANDLE, MARKER_PUBLIC, wall_clock_ms()),
            )
            .await;
        }

        /// Broadcasts the bytes on the topic as they are, sealed or not.
        pub(in crate::node) async fn broadcast_bytes(&self, bytes: Vec<u8>) {
            self.topic.broadcast_bytes(bytes).await.unwrap();
        }

        #[must_use]
        pub(in crate::node) async fn payload_where(
            &mut self,
            within: Duration,
            mut pick: impl FnMut(&Payload) -> bool + Send,
        ) -> Option<Payload> {
            tokio::time::timeout(within, async {
                loop {
                    if let Some(Ok(GossipEvent::Payload(payload))) = self.topic.next_event().await {
                        let payload = Payload::from(*payload);
                        if pick(&payload) {
                            return payload;
                        }
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
            sig: ZERO_SIGNATURE,
        })
    }

    async fn marker_reaches(member: &Member, witness: &Witness, marker_seed: u8) {
        witness.broadcast_marker(marker_seed).await;
        listed_within(member, marker_seed, PAYLOAD_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
    }

    pub(in crate::node) async fn start_third_through_second(
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
            .unwrap_or_else(|missed| missed.fail_the_test());

        let _third = start_third_through_second(&scratch, &second, THIRD_HANDLE).await;
        let third_presence_sent = tokio::time::Instant::now();
        let seen = listed_within(&first, THIRD_SEED, SPEC_OBSERVER_WAIT).await;

        assert!(third_presence_sent.elapsed() <= SPEC_OBSERVER_WAIT);
        let seen = seen.expect("the entry of THIRD_SEED listed at first within SPEC_OBSERVER_WAIT");
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
            .unwrap_or_else(|missed| missed.fail_the_test());

        let third = start_third_through_second(&scratch, &second, FIRST_HANDLE).await;

        assert_eq!(
            third.node.own_handle().as_str(),
            SECOND_EFFECTIVE_FIRST_HANDLE
        );
        let at_first = listed_within(&first, THIRD_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        let at_second = listed_within(&second, THIRD_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
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

    pub(in crate::node) async fn status_within(
        member: &Member,
        seed: u8,
        awaited: &str,
        wanted: impl Fn(Status) -> bool + Send + Sync,
        within: Duration,
    ) -> Result<Peer, WaitMissed> {
        entry_meeting_within(member, seed, awaited, |peer| wanted(peer.status), within).await
    }

    fn departure_of(seed: u8, seq: u64) -> Payload {
        Payload::Departure(crate::wire::Departure {
            v: Version::CURRENT,
            card: card(seed),
            seq,
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
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
            .unwrap_or_else(|missed| missed.fail_the_test());
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
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert_eq!(after_replay.public.as_deref(), Some(SECOND_PUBLIC));
        assert_eq!(after_replay.status, Status::Online);

        witness
            .broadcast_as(SECOND_SEED, departure_of(SECOND_SEED, wall_clock_ms()))
            .await;
        status_within(
            &first,
            SECOND_SEED,
            "second member listed as gone by the first",
            |status| matches!(status, Status::Gone { .. }),
            PAYLOAD_WAIT,
        )
        .await
        .unwrap_or_else(|missed| missed.fail_the_test());
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
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert!(matches!(after_departure.status, Status::Gone { .. }));
        assert_eq!(after_departure.public.as_deref(), Some(SECOND_PUBLIC));
        drop(scratch);
    }

    pub(in crate::node) async fn spawn_member(
        scratch: &Scratch,
        plan: MemberPlan,
    ) -> (BoundEndpoint, JoinHandle<Member>) {
        let node_endpoint = bind_holding(plan.seed, plan.register.clone()).await;
        let endpoint = node_endpoint.endpoint().clone();
        let inbox = plan
            .inbox
            .clone()
            .unwrap_or_else(|| scratch.inbox(plan.seed));
        let inbox_path = scratch.directories.inbox_socket_file(u32::from(plan.seed));
        let config_path = scratch.network_file(plan.seed);
        let starting = tokio::spawn(start_member(
            plan,
            node_endpoint,
            (inbox, inbox_path),
            config_path,
        ));
        (endpoint, starting)
    }

    pub(in crate::node) async fn start_witnessed(
        scratch: &Scratch,
        plan: MemberPlan,
        witness_handle: Option<&'static str>,
    ) -> (Member, Witness) {
        let (endpoint, starting) = spawn_member(scratch, plan).await;
        let witness = Witness::joined_through(WITNESS_SEED, witness_handle, &endpoint).await;
        (starting.await.unwrap(), witness)
    }

    pub(in crate::node) fn frozen_clock(milliseconds: u64) -> Clock {
        Arc::new(move || milliseconds)
    }

    pub(in crate::node) fn settable_clock(
        start_ms: u64,
    ) -> (Clock, Arc<std::sync::atomic::AtomicU64>) {
        let reading = Arc::new(std::sync::atomic::AtomicU64::new(start_ms));
        let read = Arc::clone(&reading);
        (
            Arc::new(move || read.load(std::sync::atomic::Ordering::SeqCst)),
            reading,
        )
    }

    impl MemberPlan {
        pub(in crate::node) fn with_clock(self, clock: Clock) -> Self {
            Self { clock, ..self }
        }

        pub(in crate::node) fn with_monotonic(self, monotonic: MonotonicClock) -> Self {
            Self { monotonic, ..self }
        }
    }

    fn replayed_infrastructure_of(host_seed: u8, relay: &str, ts: u64) -> Payload {
        let Payload::Infrastructure(record) = infrastructure_of(host_seed, relay, ts) else {
            panic!("infrastructure_of builds an infrastructure record");
        };
        Payload::Infrastructure(Infrastructure {
            addr: Addr::from_endpoint_report(
                [relay.to_owned()],
                [REPLAY_DIRECT_ADDRESS.parse().unwrap()],
            ),
            ..record
        })
    }

    pub(in crate::node) fn infrastructure_of(host_seed: u8, relay: &str, ts: u64) -> Payload {
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
            sig: ZERO_SIGNATURE,
        })
    }

    pub(in crate::node) fn relay_url(text: &str) -> RelayUrl {
        text.parse().unwrap()
    }

    pub(in crate::node) fn saved_host(
        config_path: &std::path::Path,
        host_seed: u8,
    ) -> Option<Host> {
        let id = render_public_key(&secret_key(host_seed).public());
        NetworkConfig::load(config_path)
            .ok()?
            .hosts
            .into_iter()
            .find(|host| host.id == id)
    }

    pub(in crate::node) async fn host_saved_within(
        config_path: &std::path::Path,
        host_seed: u8,
        within: Duration,
    ) -> Result<Host, WaitMissed> {
        let awaited = format!("host of seed {host_seed} in the network file");
        found_within(within, &awaited, async || {
            saved_host(config_path, host_seed)
        })
        .await
    }

    async fn lookup_entry(member: &Member, seed: u8) -> Option<iroh::EndpointAddr> {
        let lookup = member.endpoint.address_lookup().unwrap();
        match lookup.resolve(secret_key(seed).public()).next().await {
            Some(Ok(Ok(item))) => Some(item.into_endpoint_addr()),
            _ => None,
        }
    }

    pub(in crate::node) async fn relay_map_holds(endpoint: &BoundEndpoint, relay: &str) -> bool {
        let removed = endpoint
            .remove_relay_past_the_guard(&relay_url(relay))
            .await;
        if let Some(entry) = &removed {
            endpoint
                .insert_relay_past_the_guard(relay_url(relay), Arc::clone(entry))
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
        let saved = host_saved_within(&member.config_path, HOST_SEED, SPEC_HOST_ADDED_WITHIN)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert_eq!(saved.relay, HOST_RELAY);
        assert_eq!(saved.ts, Some(record_ts));
        assert!(matches!(saved.origin, HostOrigin::Learned { .. }));
        assert!(relay_map_holds(&member.endpoint, HOST_RELAY).await);
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
        host_saved_within(&member.config_path, HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        member
            .endpoint
            .remove_relay_past_the_guard(&relay_url(HOST_RELAY))
            .await
            .unwrap();

        witness
            .broadcast_as(
                HOST_SEED,
                replayed_infrastructure_of(HOST_SEED, HOST_RELAY, record_ts),
            )
            .await;
        witness
            .broadcast_as(
                HOST_SEED,
                replayed_infrastructure_of(HOST_SEED, HOST_RELAY, record_ts - 1),
            )
            .await;
        witness
            .broadcast_as(
                SECOND_HOST_SEED,
                infrastructure_of(SECOND_HOST_SEED, SECOND_HOST_RELAY, record_ts),
            )
            .await;
        host_saved_within(&member.config_path, SECOND_HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert!(!relay_map_holds(&member.endpoint, HOST_RELAY).await);
        assert_eq!(
            saved_host(&member.config_path, HOST_SEED).unwrap().ts,
            Some(record_ts)
        );

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
        host_saved_within(&member.config_path, THIRD_HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert!(relay_map_holds(&member.endpoint, HOST_RELAY).await);
        assert_eq!(
            saved_host(&member.config_path, HOST_SEED).unwrap().ts,
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
        host_saved_within(&member.config_path, SECOND_HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());

        assert!(saved_host(&member.config_path, HOST_SEED).is_none());
        assert!(!relay_map_holds(&member.endpoint, HOST_RELAY).await);
        assert!(relay_map_holds(&member.endpoint, SECOND_HOST_RELAY).await);
        drop(scratch);
    }

    pub(in crate::node) fn presence_from(seed: u8) -> impl FnMut(&Payload) -> bool {
        move |payload| matches!(payload, Payload::Presence(presence) if presence.card == card(seed))
    }

    pub(in crate::node) fn presence_request_by(seed: u8) -> impl FnMut(&Payload) -> bool {
        move |payload| matches!(payload, Payload::PresenceRequest(request) if request.requester == card(seed))
    }

    fn presence_fields(payload: Option<Payload>) -> (u64, bool) {
        let Some(Payload::Presence(presence)) = payload else {
            panic!("a presence was expected");
        };
        (presence.seq, presence.attached)
    }

    fn presence_request_from(requester_seed: u8, seq: u64) -> Payload {
        Payload::PresenceRequest(crate::wire::PresenceRequest {
            v: Version::CURRENT,
            requester: card(requester_seed),
            seq,
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        })
    }

    /// Every event the member recorded before it dropped a presence request by the rule, failing when it drops none within the wait for a payload.
    async fn recorded_until_request_dropped_by(
        recorded: &mut UnboundedReceiver<RecordedEvent>,
        rule: &str,
    ) -> Vec<RecordedEvent> {
        recorded_until(
            recorded,
            PAYLOAD_DROPPED,
            |fields| {
                fields.contains(&(KIND, serde_json::Value::from(PRESENCE_REQUEST_KIND)))
                    && fields.contains(&(RULE, serde_json::Value::from(rule)))
            },
            PAYLOAD_WAIT,
        )
        .await
        .0
    }

    /// Every event the member recorded before it took a presence request of the requester, failing when it takes none within the wait for a payload.
    async fn recorded_until_request_taken_from(
        recorded: &mut UnboundedReceiver<RecordedEvent>,
        requester_seed: u8,
    ) -> Vec<RecordedEvent> {
        recorded_until(
            recorded,
            PAYLOAD_ACCEPTED,
            |fields| {
                fields.contains(&(KIND, serde_json::Value::from(PRESENCE_REQUEST_KIND)))
                    && fields.contains(&(PEER, short_peer_id(requester_seed)))
            },
            PAYLOAD_WAIT,
        )
        .await
        .0
    }

    /// Every event the member recorded before it judged an Unreachable report about itself stamped `ts`, under the record's name, failing when it judges none so within the wait for a payload.
    async fn recorded_until_report_judged(
        recorded: &mut UnboundedReceiver<RecordedEvent>,
        record_name: &str,
        ts: u64,
    ) -> Vec<RecordedEvent> {
        recorded_until(
            recorded,
            record_name,
            |fields| {
                fields.contains(&(KIND, serde_json::Value::from(UNREACHABLE_KIND)))
                    && fields.contains(&(TS, serde_json::Value::from(ts)))
            },
            PAYLOAD_WAIT,
        )
        .await
        .0
    }

    /// Every event the member recorded before its broadcast of the Presence carrying the seq, failing when it records none within the wait for a payload.
    async fn recorded_until_presence_broadcast(
        recorded: &mut UnboundedReceiver<RecordedEvent>,
        seq: u64,
    ) -> Vec<RecordedEvent> {
        recorded_until(
            recorded,
            PAYLOAD_BROADCAST,
            |fields| *fields == presence_broadcast_with(seq),
            PAYLOAD_WAIT,
        )
        .await
        .0
    }

    /// Every event the member recorded until it reported its topic joined and asked for presence at that join, by when it has read its clock for the join, failing by name when either does not come within the wait for a payload.
    pub(in crate::node) async fn joined_and_asked_for_presence(
        recorded: &mut UnboundedReceiver<RecordedEvent>,
    ) -> Vec<RecordedEvent> {
        let (mut before, _joined) =
            recorded_until(recorded, TOPIC_JOINED, |_fields| true, PAYLOAD_WAIT).await;
        let (before_the_ask, _asked) = recorded_until(
            recorded,
            PAYLOAD_BROADCAST,
            |fields| fields.contains(&(KIND, serde_json::Value::from(PRESENCE_REQUEST_KIND))),
            PAYLOAD_WAIT,
        )
        .await;
        before.extend(before_the_ask);
        before
    }

    /// Every event the member recorded until its counters snapshot at the interval, which only the passing of the snapshot interval since its start produces, failing when none comes within the wait for it.
    async fn recorded_until_the_interval_snapshot(
        recorded: &mut UnboundedReceiver<RecordedEvent>,
    ) -> Vec<RecordedEvent> {
        recorded_until(
            recorded,
            STATE_COUNTERS,
            |fields| was_taken_at(fields, INTERVAL_MOMENT),
            INTERVAL_SNAPSHOT_WITHIN,
        )
        .await
        .0
    }

    /// The fields of each broadcast of a payload of the kind among the events, in order.
    fn broadcasts_of(events: &[RecordedEvent], kind: &str) -> Vec<crate::event::EventFields> {
        events
            .iter()
            .filter(|(name, fields)| {
                *name == PAYLOAD_BROADCAST
                    && fields.contains(&(KIND, serde_json::Value::from(kind)))
            })
            .map(|(_name, fields)| fields.clone())
            .collect()
    }

    /// The fields the member records for its broadcast of a Presence carrying the seq.
    fn presence_broadcast_with(seq: u64) -> crate::event::EventFields {
        vec![
            (KIND, serde_json::Value::from(PRESENCE_KIND)),
            (SEQ, serde_json::Value::from(seq)),
        ]
    }

    /// The field the events name the seed's card by: the first bytes of its key in lowercase hex.
    pub(in crate::node) fn short_peer_id(seed: u8) -> serde_json::Value {
        short_id_field(&secret_key(seed).public())
    }

    /// The peers a member recorded as gossip neighbors, read without consuming any event of the member's run record.
    pub(in crate::node) struct NeighborsRecorded(
        tokio::sync::watch::Receiver<Vec<serde_json::Value>>,
    );

    impl NeighborsRecorded {
        /// Waits until the member recorded the seed's node a gossip neighbor, failing with that condition when the bound runs out.
        pub(in crate::node) async fn until_recorded(&mut self, seed: u8) {
            let peer = short_peer_id(seed);
            tokio::time::timeout(
                NEIGHBOR_UP_RECORDED_WITHIN,
                self.0.wait_for(|peers| peers.contains(&peer)),
            )
            .await
            .unwrap_or_else(|_elapsed| {
                panic!("no {NEIGHBOR_UP} of seed {seed} within {NEIGHBOR_UP_RECORDED_WITHIN:?}")
            })
            .unwrap_or_else(|_closed| {
                panic!("the run record closed before any {NEIGHBOR_UP} of seed {seed}")
            });
        }
    }

    /// A run record that hands every event to the report and notes each peer recorded a gossip neighbor, with the notes' reader.
    pub(in crate::node) fn noting_neighbors(report: RunRecord) -> (RunRecord, NeighborsRecorded) {
        let (noted, recorded) = tokio::sync::watch::channel(Vec::new());
        let record: RunRecord = Arc::new(move |event: Event| {
            if event.name() == NEIGHBOR_UP {
                noted.send_modify(|peers| {
                    peers.extend(
                        event
                            .fields()
                            .into_iter()
                            .filter(|(field, _peer)| *field == PEER)
                            .map(|(_field, peer)| peer),
                    );
                });
            }
            report(event);
        });
        (record, NeighborsRecorded(recorded))
    }

    /// Waits until the member's own address is the one the Presence announced, so no re-announce of a changed address is pending, failing with both addresses when it is not within the wait for a payload.
    async fn announced_address_still_held(member: &Member, announced: &Payload) {
        let Payload::Presence(presence) = announced else {
            panic!("the member's answer is no Presence: {announced:?}");
        };
        found_within(
            PAYLOAD_WAIT,
            "the member's address back at the one it announced",
            async || (member.node.state.own_addr() == presence.addr).then_some(()),
        )
        .await
        .unwrap_or_else(|missed| {
            panic!(
                "{missed}: it moved from {:?} to {:?}",
                presence.addr,
                member.node.state.own_addr()
            )
        });
    }

    async fn ask_for_presence(witness: &Witness, requester_seed: u8, seq: u64) {
        witness
            .broadcast_as(requester_seed, presence_request_from(requester_seed, seq))
            .await;
    }

    #[tokio::test]
    async fn spec_3_15_test_15_a_member_answers_a_presence_request_with_one_presence() {
        let scratch = Scratch::new("asked").await;
        let (record, mut recorded) = recorded_events();
        let (member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_clock(frozen_clock(FROZEN_CLOCK_MS))
                .with_report(record),
            None,
        )
        .await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");

        ask_for_presence(&witness, FIRST_REQUESTER_SEED, FROZEN_CLOCK_MS).await;
        let answer = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");
        announced_address_still_held(&member, &answer).await;
        recorded_until_request_taken_from(&mut recorded, FIRST_REQUESTER_SEED).await;
        let asked = Instant::now();
        let after_the_request = recorded_until_the_interval_snapshot(&mut recorded).await;

        assert!(asked.elapsed() >= SPEC_REANNOUNCE_INTERVAL + ANSWER_WINDOW);
        assert_eq!(presence_fields(Some(answer)), (FROZEN_CLOCK_MS + 1, false));
        assert_eq!(
            broadcasts_of(&after_the_request, PRESENCE_KIND),
            vec![presence_broadcast_with(FROZEN_CLOCK_MS + 1)],
            "the member sent other than one Presence for the request before its counters at the interval: {after_the_request:?}"
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_16_two_presence_requests_from_one_requester_inside_the_reannounce_interval_are_each_answered_at_once()
     {
        let scratch = Scratch::new("asked-twice").await;
        let (_member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_clock(frozen_clock(FROZEN_CLOCK_MS)),
            None,
        )
        .await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");

        ask_for_presence(&witness, FIRST_REQUESTER_SEED, FROZEN_CLOCK_MS).await;
        let first_answer = witness
            .payload_where(ANSWERED_AT_ONCE_WITHIN, presence_from(FIRST_SEED))
            .await;
        ask_for_presence(&witness, FIRST_REQUESTER_SEED, FROZEN_CLOCK_MS + 1).await;
        let second_answer = witness
            .payload_where(ANSWERED_AT_ONCE_WITHIN, presence_from(FIRST_SEED))
            .await;

        assert_eq!(presence_fields(first_answer), (FROZEN_CLOCK_MS + 1, false));
        assert_eq!(presence_fields(second_answer), (FROZEN_CLOCK_MS + 2, false));
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_presence_request_replayed_below_its_requesters_last_seq_or_older_than_the_max_age_is_dropped()
     {
        let scratch = Scratch::new("asked-stale").await;
        let (record, mut recorded) = recorded_events();
        let (member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_clock(frozen_clock(FROZEN_CLOCK_MS))
                .with_report(record),
            None,
        )
        .await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");
        ask_for_presence(&witness, FIRST_REQUESTER_SEED, FROZEN_CLOCK_MS).await;
        let answered = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");
        announced_address_still_held(&member, &answered).await;

        ask_for_presence(&witness, FIRST_REQUESTER_SEED, REPLAYED_REQUEST_SEQ).await;
        recorded_until_request_dropped_by(&mut recorded, NOT_ABOVE_REQUESTER_SEQ_RULE).await;
        announced_address_still_held(&member, &answered).await;
        ask_for_presence(&witness, SECOND_REQUESTER_SEED, STALE_REQUEST_SEQ).await;
        let mut after_replay =
            recorded_until_request_dropped_by(&mut recorded, OLDER_THAN_MAX_AGE_RULE).await;
        let stale_dropped = Instant::now();
        let after_stale = recorded_until_the_interval_snapshot(&mut recorded).await;
        let stale_window = stale_dropped.elapsed();
        after_replay.extend(after_stale.iter().cloned());
        ask_for_presence(&witness, SECOND_REQUESTER_SEED, FROZEN_CLOCK_MS).await;
        recorded_until_request_taken_from(&mut recorded, SECOND_REQUESTER_SEED).await;
        let after_fresh = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;

        assert!(stale_window >= ANSWER_WINDOW);
        assert_eq!(
            broadcasts_of(&after_replay, PRESENCE_KIND),
            Vec::<crate::event::EventFields>::new(),
            "the member answered the replayed request: {after_replay:?}"
        );
        assert_eq!(
            broadcasts_of(&after_stale, PRESENCE_KIND),
            Vec::<crate::event::EventFields>::new(),
            "the member answered the stale request: {after_stale:?}"
        );
        assert_eq!(presence_fields(after_fresh), (FROZEN_CLOCK_MS + 2, false));
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_presence_request_dated_past_the_max_age_ahead_is_dropped_and_blocks_no_later_request()
     {
        let scratch = Scratch::new("asked-future").await;
        let (record, mut recorded) = recorded_events();
        let (_member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_clock(frozen_clock(FROZEN_CLOCK_MS))
                .with_report(record),
            None,
        )
        .await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");

        ask_for_presence(&witness, FIRST_REQUESTER_SEED, FAR_FUTURE_REQUEST_SEQ).await;
        recorded_until_request_dropped_by(&mut recorded, AHEAD_OF_MAX_AGE_RULE).await;
        let dropped = Instant::now();
        let after_far_future = recorded_until_the_interval_snapshot(&mut recorded).await;
        let far_future_window = dropped.elapsed();
        ask_for_presence(&witness, FIRST_REQUESTER_SEED, FROZEN_CLOCK_MS).await;
        recorded_until_request_taken_from(&mut recorded, FIRST_REQUESTER_SEED).await;
        let after_fresh = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;

        assert!(far_future_window >= ANSWER_WINDOW);
        assert_eq!(
            broadcasts_of(&after_far_future, PRESENCE_KIND),
            Vec::<crate::event::EventFields>::new(),
            "the member answered a far-future request with a Presence before its counters at the interval: {after_far_future:?}"
        );
        assert_eq!(presence_fields(after_fresh), (FROZEN_CLOCK_MS + 1, false));
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_stopping_member_answers_a_presence_request_with_nothing() {
        let scratch = Scratch::new("asked-stopping").await;
        let (record, mut recorded) = recorded_events();
        let (member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_clock(frozen_clock(FROZEN_CLOCK_MS))
                .with_report(record),
            None,
        )
        .await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");
        member.node.start_stopping();
        witness
            .payload_where(PAYLOAD_WAIT, |payload| {
                matches!(payload, Payload::Departure(departure) if departure.card == card(FIRST_SEED))
            })
            .await
            .expect("a Departure from the first member on the witness's topic");

        ask_for_presence(&witness, FIRST_REQUESTER_SEED, FROZEN_CLOCK_MS).await;
        recorded_until_request_taken_from(&mut recorded, FIRST_REQUESTER_SEED).await;
        let asked = Instant::now();
        let after_the_request = recorded_until_the_interval_snapshot(&mut recorded).await;

        assert!(asked.elapsed() >= ANSWER_WINDOW);
        assert_eq!(
            broadcasts_of(&after_the_request, PRESENCE_KIND),
            Vec::<crate::event::EventFields>::new(),
            "the stopping member answered the request: {after_the_request:?}"
        );
        drop(scratch);
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

    pub(in crate::node) fn unreachable_about(seed: u8, reporter_seed: u8, ts: u64) -> Payload {
        Payload::Unreachable(Unreachable {
            v: Version::CURRENT,
            card: card(seed),
            reporter: card(reporter_seed),
            ts,
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        })
    }

    #[tokio::test]
    async fn spec_3_8_an_unreachable_report_about_this_node_is_answered_with_presence_unless_stale_or_below_its_last_seq()
     {
        let scratch = Scratch::new("rumor").await;
        let (record, mut recorded) = recorded_events();
        let (clock, reading) = settable_clock(FROZEN_CLOCK_MS);
        let (_member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_clock(clock)
                .with_report(record),
            None,
        )
        .await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");
        recorded_until_presence_broadcast(&mut recorded, FROZEN_CLOCK_MS).await;

        witness
            .broadcast_as(
                WITNESS_SEED,
                unreachable_about(FIRST_SEED, WITNESS_SEED, FROZEN_CLOCK_MS - 1),
            )
            .await;
        let mut after_the_first_presence =
            recorded_until_report_judged(&mut recorded, PAYLOAD_DROPPED, FROZEN_CLOCK_MS - 1).await;
        reading.store(CLOCK_AFTER_MAX_AGE_MS, std::sync::atomic::Ordering::SeqCst);
        witness
            .broadcast_as(
                WITNESS_SEED,
                unreachable_about(FIRST_SEED, WITNESS_SEED, STALE_RUMOR_TS),
            )
            .await;
        after_the_first_presence.extend(
            recorded_until_report_judged(&mut recorded, PAYLOAD_DROPPED, STALE_RUMOR_TS).await,
        );
        witness
            .broadcast_as(
                WITNESS_SEED,
                unreachable_about(FIRST_SEED, WITNESS_SEED, FRESH_RUMOR_TS),
            )
            .await;
        after_the_first_presence.extend(
            recorded_until_report_judged(&mut recorded, PAYLOAD_ACCEPTED, FRESH_RUMOR_TS).await,
        );
        let answered = Instant::now();
        let answer = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;
        after_the_first_presence.extend(recorded_until_the_interval_snapshot(&mut recorded).await);

        assert!(answered.elapsed() >= ANSWER_WINDOW);
        assert_eq!(presence_fields(answer), (CLOCK_AFTER_MAX_AGE_MS, false));
        assert_eq!(
            broadcasts_of(&after_the_first_presence, PRESENCE_KIND),
            vec![presence_broadcast_with(CLOCK_AFTER_MAX_AGE_MS)],
            "the member sent other than one Presence for the three reports before its counters at the interval: {after_the_first_presence:?}"
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_an_unreachable_report_about_a_peer_sets_it_unreachable() {
        let scratch = Scratch::new("peer-rumor").await;
        let (record, mut recorded) = recorded_events();
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(record),
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
            "witness listed online by the member",
            |status| status == Status::Online,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap_or_else(|missed| missed.fail_the_test());
        settled_direct_answers(&member, WITNESS_SEED, DirectContact::Introduce).await;
        next_recorded_meeting(
            &mut recorded,
            PAYLOAD_ACCEPTED,
            |fields| fields.contains(&(KIND, serde_json::Value::from(PRESENCE_KIND))),
            PAYLOAD_WAIT,
        )
        .await;

        witness
            .broadcast_as(
                MARKER_SEED,
                unreachable_about(WITNESS_SEED, MARKER_SEED, wall_clock_ms()),
            )
            .await;

        status_within(
            &member,
            WITNESS_SEED,
            "witness listed as unreachable by the member",
            |status| status == Status::Unreachable,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap_or_else(|missed| missed.fail_the_test());
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
        let (report, mut neighbors) = noting_neighbors(Arc::new(|_event: Event| {}));
        let (first, second) = start_pair_reporting_first(&scratch, report).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        let mut witness = Witness::joined_through(WITNESS_SEED, None, &first.endpoint).await;
        neighbors.until_recorded(SECOND_SEED).await;
        let killed = tokio::time::Instant::now();

        drop(second.node);
        closed_within_the_documented_latency(second.endpoint.close()).await;
        let unreachable = status_within(
            &first,
            SECOND_SEED,
            "second member listed as unreachable by the first",
            |status| status == Status::Unreachable,
            SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT,
        )
        .await;
        let announced = witness
            .payload_where(PAYLOAD_WAIT, |payload| {
                matches!(payload, Payload::Unreachable(_))
            })
            .await;

        unreachable.unwrap_or_else(|missed| missed.fail_the_test());
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
        let (report, mut neighbors) = noting_neighbors(Arc::new(|_event: Event| {}));
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(report),
            Some(WITNESS_HANDLE),
        )
        .await;
        status_within(
            &member,
            WITNESS_SEED,
            "witness listed online by the member",
            |status| status == Status::Online,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap_or_else(|missed| missed.fail_the_test());
        neighbors.until_recorded(WITNESS_SEED).await;
        settled_direct_answers(&member, WITNESS_SEED, DirectContact::Introduce).await;
        let probe_answers_before =
            settled_direct_answers(&member, WITNESS_SEED, DirectContact::Probe).await;
        marker_reaches(&member, &witness, MARKER_SEED).await;
        let probes_for_earlier_downs = member
            .node
            .state
            .rows
            .probes_for_neighbor_downs_of(card(WITNESS_SEED));
        let probes_before = witness
            .probes_answered
            .load(std::sync::atomic::Ordering::SeqCst);

        witness.gossip.shutdown().await.unwrap();
        found_within(
            PROBE_ANSWERED_WAIT,
            "probe answered by the witness",
            async || {
                (witness
                    .probes_answered
                    .load(std::sync::atomic::Ordering::SeqCst)
                    != probes_before)
                    .then_some(())
            },
        )
        .await
        .unwrap_or_else(|missed| missed.fail_the_test());
        direct_answer_after(
            &member,
            WITNESS_SEED,
            DirectContact::Probe,
            probe_answers_before,
            RequestCause::NeighborDown {
                card: card(WITNESS_SEED),
                earlier_probes: probes_for_earlier_downs,
            },
            DirectOutcome::Acknowledged,
            AFTER_PROBE_ACK,
        )
        .await;

        let after = listed_within(&member, WITNESS_SEED, Duration::ZERO)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
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
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert_eq!(listed.status, Status::Unreachable);
        drop(scratch);
    }

    fn recorded_contact_failures() -> (
        RunRecord,
        mpsc::UnboundedReceiver<(EndpointId, DirectError)>,
    ) {
        let (failed, failures) = mpsc::unbounded_channel();
        let report: RunRecord = Arc::new(move |record| {
            if let Event::Direct(DirectEvent::ContactFailed { peer, reason }) = record {
                drop(failed.send((peer, reason)));
            }
        });
        (report, failures)
    }

    /// Discards every item already waiting in the receiver.
    fn discard_waiting<Item>(receiver: &mut mpsc::UnboundedReceiver<Item>) {
        while receiver.try_recv().is_ok() {}
    }

    #[tokio::test]
    async fn observability_decision_9_an_introduce_that_fails_is_recorded_naming_the_node_and_why()
    {
        let scratch = Scratch::new("intro-fail-record").await;
        let (report, mut failures) = recorded_contact_failures();
        let (_member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(report),
            None,
        )
        .await;

        witness
            .broadcast_as(
                MARKER_SEED,
                presence_of(MARKER_SEED, MARKER_HANDLE, MARKER_PUBLIC, wall_clock_ms()),
            )
            .await;
        let recorded = tokio::time::timeout(
            SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT + PAYLOAD_WAIT,
            failures.recv(),
        )
        .await
        .ok()
        .flatten();

        assert_eq!(
            recorded,
            Some((secret_key(MARKER_SEED).public(), DirectError::Unreachable))
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn observability_decision_9_a_probe_that_fails_is_recorded_naming_the_node() {
        let scratch = Scratch::new("probe-fail-record").await;
        let (report, mut failures) = recorded_contact_failures();
        let (report, mut neighbors) = noting_neighbors(report);
        let (member, witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(report),
            Some(WITNESS_HANDLE),
        )
        .await;
        status_within(
            &member,
            WITNESS_SEED,
            "witness listed online by the member",
            |status| status == Status::Online,
            PAYLOAD_WAIT,
        )
        .await
        .unwrap_or_else(|missed| missed.fail_the_test());
        settled_direct_answers(&member, WITNESS_SEED, DirectContact::Introduce).await;
        settled_direct_answers(&member, WITNESS_SEED, DirectContact::Probe).await;
        discard_waiting(&mut failures);

        neighbors.until_recorded(WITNESS_SEED).await;
        witness.close().await;
        let recorded = tokio::time::timeout(
            SPEC_NEIGHBOR_DOWN_AND_PROBE_TIMEOUT + PAYLOAD_WAIT,
            failures.recv(),
        )
        .await
        .ok()
        .flatten()
        .map(|(peer, _reason)| peer);

        assert_eq!(recorded, Some(secret_key(WITNESS_SEED).public()));
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
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");

        assert!(init_took >= SPEC_JOIN_TIMEOUT);
        assert!(init_took < SPEC_JOIN_TIMEOUT + SPEC_OBSERVER_WAIT);
        assert_eq!(lonely.node.own_handle().as_str(), FIRST_HANDLE);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_member_whose_first_join_gained_no_neighbor_asks_every_member_to_announce_itself_at_its_later_join()
     {
        let scratch = Scratch::new("late-ask").await;
        let lonely = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)],
        )
        .await
        .pop()
        .unwrap();

        let mut witness = Witness::joined_through(WITNESS_SEED, None, &lonely.endpoint).await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_request_by(FIRST_SEED))
            .await
            .expect("a presence request by the first member on the witness's topic");
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_stopping_member_whose_topic_joins_asks_no_member_to_announce_itself() {
        let scratch = Scratch::new("stopping-ask").await;
        let (record, mut recorded) = recorded_events();
        let lonely = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(record)],
        )
        .await
        .pop()
        .unwrap();
        lonely.node.start_stopping();

        let _witness = Witness::joined_through(WITNESS_SEED, None, &lonely.endpoint).await;
        recorded_until(&mut recorded, TOPIC_JOINED, |_fields| true, PAYLOAD_WAIT).await;
        let joined = Instant::now();
        let after_the_join = recorded_until_the_interval_snapshot(&mut recorded).await;

        assert!(joined.elapsed() >= PAYLOAD_WAIT);
        assert_eq!(
            broadcasts_of(&after_the_join, PRESENCE_REQUEST_KIND),
            Vec::<crate::event::EventFields>::new(),
            "the stopping member sent a presence request once its topic joined: {after_the_join:?}"
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_joining_member_asks_announces_and_introduces_itself_to_its_gossip_neighbor_under_its_assigned_handle_alone()
     {
        let scratch = Scratch::new("held-introduce").await;
        let first = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)],
        )
        .await
        .pop()
        .unwrap();
        let mut witness =
            Witness::joined_through(WITNESS_SEED, Some(WITNESS_HANDLE), &first.endpoint).await;

        let _joining = start_members_reaching(
            &scratch,
            vec![
                MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC)
                    .with_hosts(&[WITNESS_SEED]),
            ],
            &[(WITNESS_SEED, witness.address())],
        )
        .await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_request_by(SECOND_SEED))
            .await
            .expect("a presence request by the second member on the witness's topic");
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(SECOND_SEED))
            .await
            .expect("a Presence from the second member on the witness's topic");
        let first_introduce = witness
            .first_introduce_from(SECOND_SEED, PAYLOAD_WAIT)
            .await;

        assert_eq!(
            first_introduce.and_then(|introduce| introduce.handle),
            Some(SECOND_HANDLE.to_owned())
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_joining_member_whose_gossip_neighbor_announces_itself_before_the_handle_introduces_itself_to_it_once()
     {
        let scratch = Scratch::new("held-once").await;
        let first = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)],
        )
        .await
        .pop()
        .unwrap();
        let mut witness =
            Witness::joined_through(WITNESS_SEED, Some(WITNESS_HANDLE), &first.endpoint).await;
        let witness_address = witness.address();
        let witness_presence = Payload::Presence(Presence {
            v: Version::CURRENT,
            card: card(WITNESS_SEED),
            handle: WITNESS_HANDLE.to_owned(),
            public: WITNESS_PUBLIC.to_vec(),
            attached: false,
            addr: wire_addr(&witness_address),
            seq: wall_clock_ms(),
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        });
        let (report, mut events) = recorded_events();
        let running = [(WITNESS_SEED, witness_address)];

        let (_joining, ()) = tokio::join!(
            start_members_reaching(
                &scratch,
                vec![
                    MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC)
                        .with_hosts(&[WITNESS_SEED])
                        .with_report(report),
                ],
                &running,
            ),
            async {
                witness
                    .payload_where(PAYLOAD_WAIT, presence_request_by(SECOND_SEED))
                    .await
                    .expect("a presence request by the second member on the witness's topic");
                witness.broadcast_as(WITNESS_SEED, witness_presence).await;
                next_recorded_meeting(
                    &mut events,
                    PAYLOAD_ACCEPTED,
                    |fields| {
                        fields.contains(&(KIND, serde_json::Value::from(PRESENCE_KIND)))
                            && fields.contains(&(PEER, short_peer_id(WITNESS_SEED)))
                    },
                    PAYLOAD_WAIT,
                )
                .await;
                witness
                    .broadcast_as(
                        MARKER_SEED,
                        presence_of(MARKER_SEED, MARKER_HANDLE, MARKER_PUBLIC, wall_clock_ms()),
                    )
                    .await;
                witness
                    .broadcast_as(
                        SECOND_MARKER_SEED,
                        presence_of(
                            SECOND_MARKER_SEED,
                            MARKER_HANDLE,
                            MARKER_PUBLIC,
                            wall_clock_ms(),
                        ),
                    )
                    .await;
            }
        );
        next_recorded_meeting(
            &mut events,
            CONTACT_FAILED,
            |fields| fields.contains(&(PEER, short_peer_id(SECOND_MARKER_SEED))),
            TWO_UNREACHABLE_INTRODUCES_FAIL_WITHIN,
        )
        .await;

        assert_eq!(witness.introduces_taken_from(SECOND_SEED), 1);
        drop(scratch);
    }

    #[tokio::test]
    async fn observability_decision_9_a_joining_member_records_the_introduce_it_held_as_sent_after_its_handle_is_assigned()
     {
        let scratch = Scratch::new("held-introduce-sent").await;
        let first = start_members(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)],
        )
        .await
        .pop()
        .unwrap();
        let witness =
            Witness::joined_through(WITNESS_SEED, Some(WITNESS_HANDLE), &first.endpoint).await;
        let (record, mut events) = recorded_events();

        let _joining = start_members_reaching(
            &scratch,
            vec![
                MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC)
                    .with_hosts(&[WITNESS_SEED])
                    .with_report(Arc::clone(&record))
                    .with_register(HeldConnections::new(
                        Arc::new(RuntimeClock),
                        crate::net::endpoint::TransportRecord::new(
                            record,
                            crate::net::direct::tests::test_salt(),
                        ),
                    )),
            ],
            &[(WITNESS_SEED, witness.address())],
        )
        .await;
        let (recorded_before, sent) =
            recorded_names_until_introduce_sent_to(&mut events, WITNESS_SEED).await;

        assert!(
            recorded_before.contains(&OWN_HANDLE_ASSIGNED),
            "the held introduce is sent after {OWN_HANDLE_ASSIGNED}: {recorded_before:?}"
        );
        assert_eq!(
            crate::net::direct::tests::field(&sent, "outcome"),
            Some(&serde_json::Value::from("acknowledged"))
        );
        assert_eq!(
            crate::net::direct::tests::field(&sent, "connection_source"),
            Some(&serde_json::Value::from("dialed"))
        );
        drop(scratch);
    }

    /// The names of the events recorded before this node's next sent Introduce to the card of the seed, within the bound, and that event's fields.
    async fn recorded_names_until_introduce_sent_to(
        events: &mut tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
        seed: u8,
    ) -> (Vec<&'static str>, crate::event::EventFields) {
        let (before, sent) = recorded_until_introduce_sent_to(
            events,
            &short_peer_id(seed),
            |_fields| true,
            HELD_INTRODUCE_SENT_WITHIN,
        )
        .await;
        (
            before.into_iter().map(|(name, _fields)| name).collect(),
            sent,
        )
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

    pub(in crate::node) const PROPERTY_CASES: u64 = 500;
    const SPEC_REANNOUNCE_INTERVAL_MILLIS: u64 = 2000;
    const SPEC_REANNOUNCE_INTERVAL: Duration =
        Duration::from_millis(SPEC_REANNOUNCE_INTERVAL_MILLIS);
    pub(in crate::node) const SPEC_RELAY_LOSS_WAIT: Duration = Duration::from_millis(500);
    pub(in crate::node) const RELAY_POOL: [&str; 3] = [
        "https://relay-a.estafette.invalid/",
        "https://relay-b.estafette.invalid/",
        "https://relay-c.estafette.invalid/",
    ];
    const DIRECT_POOL: [&str; 2] = ["192.0.2.1:4433", "198.51.100.2:4433"];
    pub(in crate::node) const LARGEST_STEP_MS: u64 = 700;

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
        let sender = DirectClient::new(
            second.endpoint.clone(),
            keys(),
            ClientTimeouts::SPEC,
            crate::net::direct::tests::unrecorded_register(),
        );

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
    async fn spec_3_6_a_message_the_node_drops_for_its_announced_handle_is_acknowledged_refused() {
        let scratch = Scratch::new("bad-handle-ack").await;
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
        let sender = DirectClient::new(
            second.endpoint.clone(),
            keys(),
            ClientTimeouts::SPEC,
            crate::net::direct::tests::unrecorded_register(),
        );

        let ack = sender
            .send_message(
                first.endpoint.id(),
                message_to(FIRST_SEED, NOT_AN_ANNOUNCED_HANDLE),
            )
            .await
            .unwrap();

        assert_eq!(
            ack,
            crate::wire::MessageAck {
                v: Version::CURRENT,
                id: MESSAGE_ID,
                accepted: false,
                attached: false,
                reason: Some(crate::errors::ErrorCode::Refused.code().to_owned()),
            }
        );
        assert_eq!(first.node.peers(), Vec::<Peer>::new());
        drop(scratch);
    }

    pub(in crate::node) const DUPLICATE_KEY_PUBLIC: &[u8] = br#"{"role":"first","role":"second"}"#;
    pub(in crate::node) const INTEGER_PAST_64_BITS_PUBLIC: &[u8] = br#"{"a":18446744073709551616}"#;

    #[tokio::test]
    async fn spec_3_5_an_introduce_whose_public_no_card_sends_changes_nothing_at_the_node() {
        let scratch = Scratch::new("introduce-public").await;
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
        let introducing = DirectClient::new(
            second.endpoint.clone(),
            keys(),
            ClientTimeouts::SPEC,
            crate::net::direct::tests::unrecorded_register(),
        );

        for public in [DUPLICATE_KEY_PUBLIC, INTEGER_PAST_64_BITS_PUBLIC] {
            let introduced = introducing
                .introduce(
                    first.endpoint.id(),
                    Introduce {
                        v: Version::CURRENT,
                        handle: Some(SECOND_HANDLE.to_owned()),
                        public: public.to_vec(),
                        attached: true,
                        addr: wire_addr(&second.endpoint.addr()),
                        mac: ZERO_MAC,
                    },
                )
                .await;

            assert_eq!(
                introduced,
                Err(DirectError::Refused),
                "{}",
                String::from_utf8_lossy(public)
            );
            assert_eq!(first.node.peers(), Vec::<Peer>::new());
        }
        drop(scratch);
    }

    fn public_nested(depth: usize) -> Vec<u8> {
        format!("{}1{}", r#"{"a":"#.repeat(depth), "}".repeat(depth)).into_bytes()
    }

    #[tokio::test]
    async fn spec_3_5_a_public_nested_at_the_deepest_bound_a_card_allows_is_received_and_one_deeper_is_not()
     {
        let scratch = Scratch::new("introduce-nesting").await;
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
        let introducing = DirectClient::new(
            second.endpoint.clone(),
            keys(),
            ClientTimeouts::SPEC,
            crate::net::direct::tests::unrecorded_register(),
        );
        let introduction_carrying = |public: Vec<u8>| Introduce {
            v: Version::CURRENT,
            handle: Some(SECOND_HANDLE.to_owned()),
            public,
            attached: true,
            addr: wire_addr(&second.endpoint.addr()),
            mac: ZERO_MAC,
        };
        let at_bound = format!(
            "handle: {SECOND_HANDLE}\npublic_key: abc\npublic: {}1{}\n",
            "{a: ".repeat(crate::card::PUBLIC_DEEPEST_NESTING),
            "}".repeat(crate::card::PUBLIC_DEEPEST_NESTING)
        )
        .parse::<crate::card::Card>()
        .unwrap()
        .public_json();

        let one_deeper = introducing
            .introduce(
                first.endpoint.id(),
                introduction_carrying(public_nested(crate::card::PUBLIC_DEEPEST_NESTING + 1)),
            )
            .await;
        let deepest_allowed = introducing
            .introduce(first.endpoint.id(), introduction_carrying(at_bound.clone()))
            .await;

        assert_eq!(one_deeper, Err(DirectError::Refused));
        assert_eq!(
            deepest_allowed.map(|answer| answer.handle),
            Ok(Some(FIRST_HANDLE.to_owned()))
        );
        let listed = listed_within(&first, SECOND_SEED, Duration::ZERO)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());
        assert_eq!(listed.public, Some(at_bound));
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

    #[tokio::test]
    async fn spec_3_8_an_unreachable_report_about_this_node_below_its_last_presence_seq_is_dropped()
    {
        let scratch = Scratch::new("rumor-seq").await;
        let (record, mut recorded) = recorded_events();
        let (_member, mut witness) = start_witnessed(
            &scratch,
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_clock(frozen_clock(FROZEN_CLOCK_MS))
                .with_report(record),
            None,
        )
        .await;
        witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await
            .expect("a Presence from the first member on the witness's topic");
        recorded_until_presence_broadcast(&mut recorded, FROZEN_CLOCK_MS).await;

        witness
            .broadcast_as(
                WITNESS_SEED,
                unreachable_about(FIRST_SEED, WITNESS_SEED, FROZEN_CLOCK_MS - 1),
            )
            .await;
        let mut after_the_first_presence =
            recorded_until_report_judged(&mut recorded, PAYLOAD_DROPPED, FROZEN_CLOCK_MS - 1).await;
        witness
            .broadcast_as(
                WITNESS_SEED,
                unreachable_about(FIRST_SEED, WITNESS_SEED, FROZEN_CLOCK_MS),
            )
            .await;
        after_the_first_presence.extend(
            recorded_until_report_judged(&mut recorded, PAYLOAD_ACCEPTED, FROZEN_CLOCK_MS).await,
        );
        let answered = Instant::now();
        let answer = witness
            .payload_where(PAYLOAD_WAIT, presence_from(FIRST_SEED))
            .await;
        after_the_first_presence.extend(recorded_until_the_interval_snapshot(&mut recorded).await);

        assert!(answered.elapsed() >= ANSWER_WINDOW);
        assert_eq!(presence_fields(answer), (FROZEN_CLOCK_MS + 1, false));
        assert_eq!(
            broadcasts_of(&after_the_first_presence, PRESENCE_KIND),
            vec![presence_broadcast_with(FROZEN_CLOCK_MS + 1)],
            "the member sent other than one Presence for the two reports before its counters at the interval: {after_the_first_presence:?}"
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_8_a_newer_record_of_a_known_host_is_held_whole_so_the_next_save_keeps_its_relay()
     {
        let scratch = Scratch::new("relay-move").await;
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
        host_saved_within(&member.config_path, HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());

        witness
            .broadcast_as(
                HOST_SEED,
                infrastructure_of(HOST_SEED, MOVED_HOST_RELAY, record_ts + 1),
            )
            .await;
        witness
            .broadcast_as(
                SECOND_HOST_SEED,
                infrastructure_of(SECOND_HOST_SEED, SECOND_HOST_RELAY, record_ts),
            )
            .await;
        host_saved_within(&member.config_path, SECOND_HOST_SEED, PAYLOAD_WAIT)
            .await
            .unwrap_or_else(|missed| missed.fail_the_test());

        let saved = saved_host(&member.config_path, HOST_SEED).unwrap();
        assert_eq!(saved.relay, MOVED_HOST_RELAY);
        assert_eq!(saved.ts, Some(record_ts + 1));
        drop(scratch);
    }

    #[test]
    fn spec_3_3_a_network_whose_secret_or_owner_is_not_a_key_is_refused_naming_the_field() {
        let unhexed_secret = NetworkConfig {
            secret: NOT_A_KEY.to_owned(),
            ..config_with_hosts(Vec::new())
        };
        let unhexed_owner = NetworkConfig {
            owner: NOT_A_KEY.to_owned(),
            ..config_with_hosts(Vec::new())
        };

        let secret_refusal = Network::new(unhexed_secret, PathBuf::new()).err();
        let owner_refusal = Network::new(unhexed_owner, PathBuf::new()).err();
        let accepted = Network::new(config_with_hosts(Vec::new()), PathBuf::new()).unwrap();

        assert!(matches!(
            secret_refusal,
            Some(NodeError::NetworkKeyInvalid {
                field: "secret",
                ..
            })
        ));
        assert!(matches!(
            owner_refusal,
            Some(NodeError::NetworkKeyInvalid { field: "owner", .. })
        ));
        assert_eq!(accepted.keys(), &keys());
    }

    #[tokio::test]
    async fn spec_3_8_the_observer_wait_runs_on_the_monotonic_clock_the_composition_root_hands_in()
    {
        let scratch = Scratch::new("monotonic").await;
        let clock = StandingClock::standing_an_hour_ahead();
        let started = clock.now();
        let (first_record, mut first_events) = recorded_events();
        let (second_record, mut second_events) = recorded_events();

        let (_members, before_the_wait) = tokio::join!(
            start_members(
                &scratch,
                vec![
                    MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                        .with_hosts(&[SECOND_SEED])
                        .with_monotonic(clock.monotonic())
                        .with_report(first_record),
                    MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC)
                        .with_hosts(&[FIRST_SEED])
                        .with_monotonic(clock.monotonic())
                        .with_report(second_record),
                ],
            ),
            async {
                let mut before_the_wait = joined_and_asked_for_presence(&mut first_events).await;
                before_the_wait.extend(joined_and_asked_for_presence(&mut second_events).await);
                clock
                    .asked_exactly(
                        "a member's observer wait",
                        &BTreeSet::from([
                            later_by(started, SPEC_OBSERVER_WAIT),
                            later_by(started, SPEC_JOIN_TIMEOUT),
                            later_by(started, SPEC_DEAF_TIMEOUT),
                            later_by(started, SPEC_STATE_SNAPSHOT_INTERVAL),
                        ]),
                    )
                    .await;
                for events in [&mut first_events, &mut second_events] {
                    before_the_wait.extend(std::iter::from_fn(|| events.try_recv().ok()));
                }
                clock.move_forward(SPEC_OBSERVER_WAIT);
                for events in [&mut first_events, &mut second_events] {
                    next_recorded_meeting(
                        events,
                        OWN_HANDLE_ASSIGNED,
                        |_fields| true,
                        ASSIGNED_ONCE_THE_CLOCK_READS_THE_WAIT_WITHIN,
                    )
                    .await;
                }
                before_the_wait
            }
        );

        let assigned_before_the_wait: Vec<&RecordedEvent> = before_the_wait
            .iter()
            .filter(|(name, _fields)| *name == OWN_HANDLE_ASSIGNED)
            .collect();
        assert!(
            assigned_before_the_wait.is_empty(),
            "{assigned_before_the_wait:?}"
        );
        drop(scratch);
    }

    #[cfg(feature = "relay")]
    async fn wait_until_home_relay_is(
        endpoint: &BoundEndpoint,
        awaited: impl Fn(&RelayStatus) -> bool + Send + Sync,
        awaited_state: &str,
    ) {
        let mut statuses = endpoint.home_relay_status().stream();
        let reached = tokio::time::timeout(HOME_RELAY_STATUS_WITHIN, async {
            while let Some(current) = statuses.next().await {
                if current.iter().any(&awaited) {
                    return true;
                }
            }
            false
        })
        .await;
        assert!(
            matches!(reached, Ok(true)),
            "the home relay never became {awaited_state}"
        );
    }

    #[cfg(feature = "relay")]
    async fn wait_until_home_relay_connected(endpoint: &BoundEndpoint) {
        wait_until_home_relay_is(endpoint, RelayStatus::is_connected, "connected").await;
    }

    #[cfg(feature = "relay")]
    async fn wait_until_home_relay_disconnected(endpoint: &BoundEndpoint) {
        wait_until_home_relay_is(endpoint, |status| !status.is_connected(), "disconnected").await;
    }

    #[cfg(feature = "relay")]
    #[tokio::test]
    async fn spec_3_8_a_home_relay_lost_for_the_relay_loss_wait_leaves_the_relay_map() {
        let scratch = Scratch::new("relay-loss").await;
        let (relay, relay_address) = local_relay().await;
        let endpoint = NodeEndpoint::bind(
            secret_key(FIRST_SEED),
            *keys().gate_key(),
            vec![relay_address.clone()],
            move |nonce: &mut [u8; KEY_LEN]| *nonce = [FIRST_SEED; KEY_LEN],
            |_ignored| {},
            crate::net::direct::tests::unrecorded_register(),
        )
        .await
        .unwrap();
        let relay_map = endpoint.relay_map();
        let member = start_member(
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
            endpoint,
            (
                scratch.inbox(FIRST_SEED),
                scratch.directories.inbox_socket_file(u32::from(FIRST_SEED)),
            ),
            scratch.network_file(FIRST_SEED),
        )
        .await;
        wait_until_home_relay_connected(&member.endpoint).await;

        relay.shutdown().await.unwrap();
        wait_until_home_relay_disconnected(&member.endpoint).await;
        found_within(
            SPEC_RELAY_LOSS_WAIT + RELAY_LOSS_REMOVAL_SLACK,
            "lost home relay removed from the relay map",
            async || (!relay_map.contains(&relay_address)).then_some(()),
        )
        .await
        .unwrap_or_else(|missed| missed.fail_the_test());

        assert!(
            member
                .endpoint
                .remove_relay_past_the_guard(&relay_address)
                .await
                .is_none()
        );
        drop(scratch);
    }

    #[cfg(feature = "relay")]
    const HOME_RELAY: &str = "transport.home_relay";
    #[cfg(feature = "relay")]
    const RELAY_MAP_EDITED: &str = "transport.relay_map_edited";
    #[cfg(feature = "relay")]
    const PRESENCE_DEFERRED: &str = "gossip.presence_deferred";
    /// The longest a member takes to announce once its handed clock reads the end of the reannounce interval.
    #[cfg(feature = "relay")]
    const ANNOUNCED_ONCE_THE_CLOCK_READS_IT_WITHIN: Duration = Duration::from_secs(5);

    /// A relay server on a local port, and its URL.
    #[cfg(feature = "relay")]
    async fn local_relay() -> (iroh_relay::server::Server, RelayUrl) {
        use iroh_relay::server::{RelayConfig, Server, ServerConfig};

        let mut relay_config = ServerConfig::default();
        relay_config.relay = Some(RelayConfig::new(std::net::SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            0,
        ))));
        let relay = Server::spawn(relay_config).await.unwrap();
        let relay_address = relay_url(&format!("http://{}/", relay.http_addr().unwrap()));
        (relay, relay_address)
    }

    /// The first seed's endpoint on the relays, its register on the runtime's clock handing its transport events to the record.
    #[cfg(feature = "relay")]
    async fn endpoint_recording_to(relays: Vec<RelayUrl>, record: &RunRecord) -> NodeEndpoint {
        NodeEndpoint::bind(
            secret_key(FIRST_SEED),
            *keys().gate_key(),
            relays,
            move |nonce: &mut [u8; KEY_LEN]| *nonce = [FIRST_SEED; KEY_LEN],
            |_ignored| {},
            HeldConnections::new(
                Arc::new(RuntimeClock),
                crate::net::endpoint::TransportRecord::new(
                    Arc::clone(record),
                    crate::net::direct::tests::test_salt(),
                ),
            ),
        )
        .await
        .unwrap()
    }

    /// A member alone on the standing clock, with the local relay it reached as its home relay before its init announced it, the relay map it dials through, and the events it records.
    #[cfg(feature = "relay")]
    struct MemberOnALocalRelay {
        member: Member,
        relay: iroh_relay::server::Server,
        relay_address: RelayUrl,
        relay_map: iroh::RelayMap,
        events: UnboundedReceiver<RecordedEvent>,
    }

    /// Starts a member alone on the clock whose only relay is a local one, ending its init by moving the clock past the join timeout once the relay is its home relay.
    /// The deadlines a member alone on a standing clock with a local home relay holds once the clock has read the join timeout and the member took its relay for lost: the relay loss wait of the start, the join timeout, the relay loss wait now asked, and the snapshot.
    #[cfg(feature = "relay")]
    fn deadlines_of_a_member_on_a_local_relay(
        started: tokio::time::Instant,
        now: tokio::time::Instant,
    ) -> BTreeSet<tokio::time::Instant> {
        BTreeSet::from([
            later_by(started, SPEC_RELAY_LOSS_WAIT),
            later_by(started, SPEC_JOIN_TIMEOUT),
            later_by(now, SPEC_RELAY_LOSS_WAIT),
            later_by(started, SPEC_STATE_SNAPSHOT_INTERVAL),
        ])
    }

    /// The deadlines a member alone with a configured host on a standing clock holds once the clock has read the join timeout, a witness has spoken and the member took its relay for lost: the observer wait, the join timeout, the relay loss wait now asked, the end of the reannounce interval, the deaf timeout from the start and from now, and the snapshot.
    #[cfg(feature = "relay")]
    fn deadlines_of_a_member_with_a_host_relay(
        started: tokio::time::Instant,
        now: tokio::time::Instant,
    ) -> BTreeSet<tokio::time::Instant> {
        BTreeSet::from([
            later_by(started, SPEC_OBSERVER_WAIT),
            later_by(started, SPEC_JOIN_TIMEOUT),
            later_by(now, SPEC_RELAY_LOSS_WAIT),
            later_by(now, SPEC_REANNOUNCE_INTERVAL),
            later_by(started, SPEC_DEAF_TIMEOUT),
            later_by(now, SPEC_DEAF_TIMEOUT),
            later_by(started, SPEC_STATE_SNAPSHOT_INTERVAL),
        ])
    }

    /// The deadlines an announced member alone on a standing clock holds once its address changed: the join timeout, the relay loss wait now asked, the end of the reannounce interval, and the snapshot.
    #[cfg(feature = "relay")]
    fn deadlines_of_an_announced_member(
        started: tokio::time::Instant,
        now: tokio::time::Instant,
    ) -> BTreeSet<tokio::time::Instant> {
        BTreeSet::from([
            later_by(started, SPEC_JOIN_TIMEOUT),
            later_by(now, SPEC_RELAY_LOSS_WAIT),
            later_by(now, SPEC_REANNOUNCE_INTERVAL),
            later_by(started, SPEC_STATE_SNAPSHOT_INTERVAL),
        ])
    }

    #[cfg(feature = "relay")]
    async fn member_on_a_local_relay(
        scratch: &Scratch,
        clock: &StandingClock,
    ) -> MemberOnALocalRelay {
        let (relay, relay_address) = local_relay().await;
        let (record, mut events) = recorded_events();
        let endpoint = endpoint_recording_to(vec![relay_address.clone()], &record).await;
        let relay_map = endpoint.relay_map();
        let (member, ()) = tokio::join!(
            start_member(
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_monotonic(clock.monotonic())
                    .with_report(record),
                endpoint,
                (
                    scratch.inbox(FIRST_SEED),
                    scratch.directories.inbox_socket_file(u32::from(FIRST_SEED)),
                ),
                scratch.network_file(FIRST_SEED),
            ),
            async {
                let before_connected = recorded_until(
                    &mut events,
                    HOME_RELAY,
                    |fields| fields.contains(&("connected", serde_json::Value::from(true))),
                    HOME_RELAY_STATUS_WITHIN,
                )
                .await
                .0;
                if !before_connected
                    .iter()
                    .any(|(name, _fields)| *name == STATE_COUNTERS)
                {
                    next_recorded_meeting(
                        &mut events,
                        STATE_COUNTERS,
                        |_fields| true,
                        PAYLOAD_WAIT,
                    )
                    .await;
                }
                clock.move_forward(SPEC_JOIN_TIMEOUT);
                next_recorded_meeting(
                    &mut events,
                    OWN_HANDLE_ASSIGNED,
                    |_fields| true,
                    PAYLOAD_WAIT,
                )
                .await;
            }
        );
        MemberOnALocalRelay {
            member,
            relay,
            relay_address,
            relay_map,
            events,
        }
    }

    /// Every event the member recorded until it took its home relay for lost, by when it has read its clock for the loss, failing by name when that does not come within the wait for the status.
    #[cfg(feature = "relay")]
    async fn home_relay_taken_for_lost(
        events: &mut UnboundedReceiver<RecordedEvent>,
    ) -> Vec<RecordedEvent> {
        recorded_until(
            events,
            HOME_RELAY,
            |fields| fields.contains(&("connected", serde_json::Value::from(false))),
            HOME_RELAY_STATUS_WITHIN,
        )
        .await
        .0
    }

    /// Waits until the member's loop has taken the change its home relay made to its own address, which the loop reconsiders, holding that relay, in the same step that records the relay connected, failing by name when that record does not come within the wait for the status.
    #[cfg(feature = "relay")]
    async fn own_address_change_taken_by_the_loop(events: &mut UnboundedReceiver<RecordedEvent>) {
        next_recorded_meeting(
            events,
            HOME_RELAY,
            |fields| fields.contains(&("connected", serde_json::Value::from(true))),
            HOME_RELAY_STATUS_WITHIN,
        )
        .await;
    }

    #[cfg(feature = "relay")]
    #[tokio::test]
    async fn spec_3_8_a_lost_home_relay_leaves_the_relay_map_once_the_handed_clock_reads_the_relay_loss_wait()
     {
        let scratch = Scratch::new("loss-on-clock").await;
        let clock = StandingClock::standing_an_hour_ahead();
        let started = clock.now();
        let MemberOnALocalRelay {
            member: _member,
            relay,
            relay_address,
            relay_map,
            mut events,
        } = member_on_a_local_relay(&scratch, &clock).await;

        relay.shutdown().await.unwrap();
        home_relay_taken_for_lost(&mut events).await;
        clock
            .asked_exactly(
                "the relay loss wait",
                &deadlines_of_a_member_on_a_local_relay(started, clock.now()),
            )
            .await;
        clock.move_forward(SPEC_RELAY_LOSS_WAIT);
        found_within(
            RELAY_LOSS_REMOVAL_SLACK,
            "lost home relay removed from the relay map after the clock read the relay loss wait",
            async || (!relay_map.contains(&relay_address)).then_some(()),
        )
        .await
        .unwrap_or_else(|missed| missed.fail_the_test());
        drop(scratch);
    }

    #[cfg(feature = "relay")]
    #[tokio::test]
    async fn spec_3_8_a_home_relay_the_relay_loss_wait_removes_is_recorded_as_a_removal_at_its_salted_location()
     {
        use crate::net::endpoint::LocationHash;

        let scratch = Scratch::new("loss-record").await;
        let clock = StandingClock::standing_an_hour_ahead();
        let started = clock.now();
        let MemberOnALocalRelay {
            member: _member,
            relay,
            relay_address,
            mut events,
            ..
        } = member_on_a_local_relay(&scratch, &clock).await;
        relay.shutdown().await.unwrap();
        home_relay_taken_for_lost(&mut events).await;
        clock
            .asked_exactly(
                "the relay loss wait",
                &deadlines_of_a_member_on_a_local_relay(started, clock.now()),
            )
            .await;

        clock.move_forward(SPEC_RELAY_LOSS_WAIT);
        let removal = next_recorded_meeting(
            &mut events,
            RELAY_MAP_EDITED,
            |_fields| true,
            RELAY_LOSS_REMOVAL_SLACK,
        )
        .await;

        assert_eq!(
            removal,
            [
                ("edit", serde_json::Value::from("removed")),
                ("kind", serde_json::Value::from("relay")),
                (
                    "port",
                    serde_json::Value::from(relay_address.port_or_known_default())
                ),
                (
                    "location",
                    serde_json::Value::from(
                        LocationHash::of(
                            &crate::net::direct::tests::test_salt(),
                            relay_address.as_str()
                        )
                        .hex()
                    )
                ),
            ]
        );
        drop(scratch);
    }

    /// A member's run on the standing clock whose relay map gained the relay of a host record and lost the relay of a configured host, with every event the member recorded from its start to the removal.
    #[cfg(feature = "relay")]
    struct RelayMapGainedAndLostARelay {
        added_relay: RelayUrl,
        removed_relay: RelayUrl,
        events: Vec<RecordedEvent>,
    }

    /// Appends to `taken` every event recorded until the next of that name meeting the condition, that one included, failing by name when none comes within the wait.
    #[cfg(feature = "relay")]
    async fn take_until_recorded(
        events: &mut UnboundedReceiver<RecordedEvent>,
        taken: &mut Vec<RecordedEvent>,
        name: &'static str,
        meets: impl Fn(&crate::event::EventFields) -> bool + Send + Sync,
        within: Duration,
    ) {
        let (before, found) = recorded_until(events, name, meets, within).await;
        taken.extend(before);
        taken.push((name, found));
    }

    /// Runs a member whose configured host owns the local relay it is bound to, adds the relay of a witnessed host record, then stops the local relay and moves the clock past the relay loss wait.
    #[cfg(feature = "relay")]
    async fn relay_map_gaining_and_losing_a_relay(
        scratch: &Scratch,
    ) -> RelayMapGainedAndLostARelay {
        let clock = StandingClock::standing_an_hour_ahead();
        let started = clock.now();
        let (relay, removed_relay) = local_relay().await;
        let added_relay = relay_url(SECOND_HOST_RELAY);
        let (record, mut events) = recorded_events();
        let endpoint = endpoint_recording_to(vec![removed_relay.clone()], &record).await;
        let member_endpoint = endpoint.endpoint().clone();
        let starting = tokio::spawn(start_member(
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_monotonic(clock.monotonic())
                .with_host_relays(&[(HOST_SEED, removed_relay.as_str())])
                .with_report(record),
            endpoint,
            (
                scratch.inbox(FIRST_SEED),
                scratch.directories.inbox_socket_file(u32::from(FIRST_SEED)),
            ),
            scratch.network_file(FIRST_SEED),
        ));
        let witness = Witness::joined_through(WITNESS_SEED, None, &member_endpoint).await;
        let mut recorded = Vec::new();
        let (_member, ()) = tokio::join!(async { starting.await.unwrap() }, async {
            take_until_recorded(
                &mut events,
                &mut recorded,
                STATE_COUNTERS,
                |_fields| true,
                PAYLOAD_WAIT,
            )
            .await;
            clock.move_forward(SPEC_JOIN_TIMEOUT);
            take_until_recorded(
                &mut events,
                &mut recorded,
                OWN_HANDLE_ASSIGNED,
                |_fields| true,
                PAYLOAD_WAIT,
            )
            .await;
        });
        witness
            .broadcast_as(
                SECOND_HOST_SEED,
                infrastructure_of(
                    SECOND_HOST_SEED,
                    added_relay.as_str(),
                    wall_clock_ms() / 1000,
                ),
            )
            .await;
        take_until_recorded(
            &mut events,
            &mut recorded,
            RELAY_MAP_EDITED,
            |fields| fields.contains(&("edit", serde_json::Value::from("added"))),
            HOME_RELAY_STATUS_WITHIN,
        )
        .await;
        take_until_recorded(
            &mut events,
            &mut recorded,
            HOME_RELAY,
            |fields| fields.contains(&("connected", serde_json::Value::from(true))),
            HOME_RELAY_STATUS_WITHIN,
        )
        .await;
        relay.shutdown().await.unwrap();
        take_until_recorded(
            &mut events,
            &mut recorded,
            HOME_RELAY,
            |fields| fields.contains(&("connected", serde_json::Value::from(false))),
            HOME_RELAY_STATUS_WITHIN,
        )
        .await;
        clock
            .asked_exactly(
                "the relay loss wait",
                &deadlines_of_a_member_with_a_host_relay(started, clock.now()),
            )
            .await;
        clock.move_forward(SPEC_RELAY_LOSS_WAIT);
        take_until_recorded(
            &mut events,
            &mut recorded,
            RELAY_MAP_EDITED,
            |fields| fields.contains(&("edit", serde_json::Value::from("removed"))),
            RELAY_LOSS_REMOVAL_SLACK,
        )
        .await;
        RelayMapGainedAndLostARelay {
            added_relay,
            removed_relay,
            events: recorded,
        }
    }

    #[cfg(feature = "relay")]
    #[tokio::test]
    async fn spec_3_8_a_removed_relay_a_known_host_owns_is_recorded_with_that_host_as_its_short_id()
    {
        let scratch = Scratch::new("loss-host").await;

        let RelayMapGainedAndLostARelay { events, .. } =
            relay_map_gaining_and_losing_a_relay(&scratch).await;

        let removal = events
            .iter()
            .rfind(|(name, _fields)| *name == RELAY_MAP_EDITED)
            .map(|(_name, fields)| fields.clone())
            .unwrap();
        assert!(
            removal.contains(&(
                "host",
                serde_json::Value::from(
                    crate::event::ShortId::from(secret_key(HOST_SEED).public()).text()
                )
            )),
            "{removal:?}"
        );
        drop(scratch);
    }

    #[cfg(feature = "relay")]
    #[tokio::test]
    async fn observability_decision_6_no_event_of_a_run_that_adds_and_removes_a_relay_writes_the_relay_host_in_clear()
     {
        let scratch = Scratch::new("loss-clear").await;

        let RelayMapGainedAndLostARelay {
            added_relay,
            removed_relay,
            events,
        } = relay_map_gaining_and_losing_a_relay(&scratch).await;

        let edits: Vec<&serde_json::Value> = events
            .iter()
            .filter(|(name, _fields)| *name == RELAY_MAP_EDITED)
            .filter_map(|(_name, fields)| fields.iter().find(|(key, _value)| *key == "edit"))
            .map(|(_key, value)| value)
            .collect();
        assert_eq!(
            edits,
            [
                &serde_json::Value::from("added"),
                &serde_json::Value::from("removed")
            ]
        );
        for relay in [&added_relay, &removed_relay] {
            let host = relay.host_str().unwrap();
            for (name, fields) in &events {
                assert!(
                    !format!("{fields:?}").contains(host),
                    "{name} writes the relay host {host} in clear: {fields:?}"
                );
            }
        }
        drop(scratch);
    }

    /// A member alone on the standing clock whose init is done and whose Presence went out once a client attached to its inbox, with the local relay it may gain and the events it records.
    #[cfg(feature = "relay")]
    struct AnnouncedMember {
        member: Member,
        _client: tokio::net::UnixStream,
        relay_address: RelayUrl,
        events: UnboundedReceiver<RecordedEvent>,
        _relay: iroh_relay::server::Server,
    }

    #[cfg(feature = "relay")]
    async fn member_announced_on_the_standing_clock(
        scratch: &Scratch,
        clock: &StandingClock,
    ) -> AnnouncedMember {
        let (relay, relay_address) = local_relay().await;
        let (record, mut events) = recorded_events();
        let endpoint = endpoint_recording_to(Vec::new(), &record).await;
        let (member, ()) = tokio::join!(
            start_member(
                MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                    .with_monotonic(clock.monotonic())
                    .with_report(record),
                endpoint,
                (
                    scratch.inbox(FIRST_SEED),
                    scratch.directories.inbox_socket_file(u32::from(FIRST_SEED)),
                ),
                scratch.network_file(FIRST_SEED),
            ),
            async {
                next_recorded_meeting(&mut events, STATE_COUNTERS, |_fields| true, PAYLOAD_WAIT)
                    .await;
                clock.move_forward(SPEC_JOIN_TIMEOUT);
                next_recorded_meeting(
                    &mut events,
                    OWN_HANDLE_ASSIGNED,
                    |_fields| true,
                    PAYLOAD_WAIT,
                )
                .await;
            }
        );
        let client = tokio::net::UnixStream::connect(&member.inbox_path)
            .await
            .unwrap();
        next_recorded_meeting(
            &mut events,
            PAYLOAD_BROADCAST,
            |fields| fields.contains(&(KIND, serde_json::Value::from(PRESENCE_KIND))),
            PAYLOAD_WAIT,
        )
        .await;
        AnnouncedMember {
            member,
            _client: client,
            relay_address,
            events,
            _relay: relay,
        }
    }

    #[cfg(feature = "relay")]
    #[tokio::test]
    async fn spec_3_8_an_address_changed_inside_the_reannounce_interval_is_announced_once_the_handed_clock_reads_its_end()
     {
        let scratch = Scratch::new("reannounce-on-clock").await;
        let clock = StandingClock::standing_an_hour_ahead();
        let started = clock.now();
        let AnnouncedMember {
            member,
            relay_address,
            mut events,
            _client,
            _relay,
        } = member_announced_on_the_standing_clock(&scratch, &clock).await;
        member
            .endpoint
            .insert_relay_past_the_guard(
                relay_address.clone(),
                Arc::new(iroh::RelayConfig::from(relay_address)),
            )
            .await;
        own_address_change_taken_by_the_loop(&mut events).await;
        clock
            .asked_exactly(
                "the end of the reannounce interval",
                &deadlines_of_an_announced_member(started, clock.now()),
            )
            .await;
        let before_the_interval_ends: Vec<RecordedEvent> =
            std::iter::from_fn(|| events.try_recv().ok()).collect();

        clock.move_forward(SPEC_REANNOUNCE_INTERVAL);
        next_recorded_meeting(
            &mut events,
            PAYLOAD_BROADCAST,
            |fields| fields.contains(&(KIND, serde_json::Value::from(PRESENCE_KIND))),
            ANNOUNCED_ONCE_THE_CLOCK_READS_IT_WITHIN,
        )
        .await;

        assert_eq!(
            broadcasts_of(&before_the_interval_ends, PRESENCE_KIND),
            Vec::<crate::event::EventFields>::new()
        );
        drop(scratch);
    }

    #[cfg(feature = "relay")]
    #[tokio::test]
    async fn spec_3_8_an_announcement_put_off_by_the_reannounce_interval_is_recorded_once_with_the_milliseconds_it_is_put_off()
     {
        let scratch = Scratch::new("deferral-record").await;
        let clock = StandingClock::standing_an_hour_ahead();
        let started = clock.now();
        let AnnouncedMember {
            member,
            relay_address,
            mut events,
            _client,
            _relay,
        } = member_announced_on_the_standing_clock(&scratch, &clock).await;

        member
            .endpoint
            .insert_relay_past_the_guard(
                relay_address.clone(),
                Arc::new(iroh::RelayConfig::from(relay_address)),
            )
            .await;
        let first_deferral = next_recorded_meeting(
            &mut events,
            PRESENCE_DEFERRED,
            |_fields| true,
            HOME_RELAY_STATUS_WITHIN,
        )
        .await;
        clock
            .asked_exactly(
                "the end of the reannounce interval",
                &deadlines_of_an_announced_member(started, clock.now()),
            )
            .await;
        clock.move_forward(SPEC_REANNOUNCE_INTERVAL);
        let (recorded_before_the_announcement, _announcement) = recorded_until(
            &mut events,
            PAYLOAD_BROADCAST,
            |fields| fields.contains(&(KIND, serde_json::Value::from(PRESENCE_KIND))),
            ANNOUNCED_ONCE_THE_CLOCK_READS_IT_WITHIN,
        )
        .await;

        assert_eq!(
            first_deferral,
            [(
                "deferred_ms",
                serde_json::Value::from(SPEC_REANNOUNCE_INTERVAL_MILLIS)
            )]
        );
        assert_eq!(
            recorded_before_the_announcement
                .iter()
                .filter(|(name, _fields)| *name == PRESENCE_DEFERRED)
                .count(),
            0
        );
        drop(scratch);
    }

    #[cfg(feature = "relay")]
    #[tokio::test]
    async fn observability_decision_9_the_home_relays_status_is_recorded_by_port_and_salted_hash_connected_then_lost()
     {
        use crate::net::direct::tests::test_salt;
        use crate::net::endpoint::LocationHash;
        use crate::test_waits::next_recorded;

        let scratch = Scratch::new("home-relay-record").await;
        let (relay, relay_address) = local_relay().await;
        let (record, mut events) = recorded_events();
        let endpoint = endpoint_recording_to(vec![relay_address.clone()], &record).await;
        let _member = start_member(
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
            endpoint,
            (
                scratch.inbox(FIRST_SEED),
                scratch.directories.inbox_socket_file(u32::from(FIRST_SEED)),
            ),
            scratch.network_file(FIRST_SEED),
        )
        .await;
        let home_relay = |connected: bool| {
            vec![
                ("kind", serde_json::Value::from("relay")),
                (
                    "port",
                    serde_json::Value::from(relay_address.port().unwrap()),
                ),
                (
                    "location",
                    serde_json::Value::from(
                        LocationHash::of(&test_salt(), relay_address.as_str()).hex(),
                    ),
                ),
                ("connected", serde_json::Value::from(connected)),
            ]
        };

        let mut connected = next_recorded(&mut events, "transport.home_relay").await;
        while connected != home_relay(true) {
            connected = next_recorded(&mut events, "transport.home_relay").await;
        }
        relay.shutdown().await.unwrap();
        let mut lost = next_recorded(&mut events, "transport.home_relay").await;
        while lost != home_relay(false) {
            lost = next_recorded(&mut events, "transport.home_relay").await;
        }

        assert_eq!(lost, home_relay(false));
        drop(scratch);
    }

    const RESTARTED_SECOND_INBOX: u8 = 162;
    const FIRST_TO_THE_RESTARTED_SECOND: &str = "amy to the restarted bob";
    const THE_RESTARTED_SECOND_TO_FIRST: &str = "the restarted bob to amy";
    const RESTARTED_LISTED_WAIT: Duration = Duration::from_secs(5);

    /// A member running on a runtime of its own, as a process does, until it vanishes.
    struct MemberOnItsOwnRuntime {
        vanish: tokio::sync::oneshot::Sender<()>,
        vanished: tokio::task::JoinHandle<(tokio::runtime::Runtime, Member)>,
    }

    /// A member whose runtime is no longer driven, gone the way a killed process is: its connections fall silent and none is closed.
    struct VanishedMember {
        runtime: tokio::runtime::Runtime,
        member: Member,
    }

    impl MemberOnItsOwnRuntime {
        /// Starts the member of the plan on a runtime of its own, joined through the running nodes, and returns once it has assigned its handle.
        async fn start(
            scratch: &Scratch,
            plan: MemberPlan,
            running: Vec<(u8, iroh::EndpointAddr)>,
        ) -> Self {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let inbox = {
                let _on_its_own_runtime = runtime.enter();
                scratch.inbox(plan.seed)
            };
            let inbox_path = scratch.directories.inbox_socket_file(u32::from(plan.seed));
            let config_path = scratch.network_file(plan.seed);
            let (started, started_here) = tokio::sync::oneshot::channel();
            let (vanish, vanish_here) = tokio::sync::oneshot::channel::<()>();
            let vanished = tokio::task::spawn_blocking(move || {
                let member = runtime.block_on(async move {
                    let endpoint = bind(plan.seed).await;
                    for (_seed, address) in running {
                        endpoint.replace_address(address);
                    }
                    let member =
                        start_member(plan, endpoint, (inbox, inbox_path), config_path).await;
                    started.send(()).unwrap();
                    drop(vanish_here.await);
                    member
                });
                (runtime, member)
            });
            started_here.await.unwrap();
            Self { vanish, vanished }
        }

        /// Stops driving the member's runtime and returns it gone.
        async fn vanish(self) -> VanishedMember {
            self.vanish.send(()).unwrap();
            let (runtime, member) = self.vanished.await.unwrap();
            VanishedMember { runtime, member }
        }
    }

    impl VanishedMember {
        /// Drops the member inside its runtime, then the runtime, off the test's own runtime.
        async fn forget(self) {
            tokio::task::spawn_blocking(move || {
                let Self { runtime, member } = self;
                runtime.block_on(async move { drop(member) });
                drop(runtime);
            })
            .await
            .unwrap();
        }
    }

    /// The member's entry for the card of the seed once it meets the condition, waited for without a sleep, or the awaited words and the bound when it does not within the wait.
    pub(in crate::node) async fn entry_meeting_within(
        member: &Member,
        seed: u8,
        awaited: &str,
        condition: impl Fn(&Peer) -> bool + Send + Sync,
        within: Duration,
    ) -> Result<Peer, WaitMissed> {
        found_within(within, awaited, async || {
            member
                .node
                .peers()
                .into_iter()
                .find(|peer| peer.card == card(seed) && condition(peer))
        })
        .await
    }

    /// The member's entry for the card of the seed as it stands, or none when the member holds none.
    pub(in crate::node) fn entry_held_now(member: &Member, seed: u8) -> Option<Peer> {
        member
            .node
            .peers()
            .into_iter()
            .find(|peer| peer.card == card(seed))
    }

    /// The longest a direct request waits for its answer: a connection, then the acknowledgement.
    const ANY_DIRECT_ANSWER_WITHIN: Duration = ClientTimeouts::SPEC
        .connect
        .saturating_add(ClientTimeouts::SPEC.ack);
    /// The longest the requests to one card take to settle: one answered, then one its answer called for, no shorter than the two ten-second event waits the settle replaced.
    const DIRECT_ANSWERS_SETTLED_WITHIN: Duration =
        ANY_DIRECT_ANSWER_WITHIN.saturating_add(ANY_DIRECT_ANSWER_WITHIN);

    /// How many requests of the kind the member made to the seed's card, and the outcome and request cause of each answer to them it applied, in order.
    fn exchanges_with(
        member: &Member,
        seed: u8,
        kind: DirectContact,
    ) -> (usize, Vec<(DirectOutcome, Option<RequestCause>)>) {
        let exchanges = member.node.state.rows.direct_exchanges();
        let made = exchanges
            .iter()
            .filter(|exchange| match **exchange {
                DirectExchange::Made {
                    card: to,
                    kind: made_kind,
                    ..
                } => to == card(seed) && made_kind == kind,
                DirectExchange::Answered { .. } => false,
            })
            .count();
        let answered = exchanges
            .iter()
            .filter_map(|exchange| match *exchange {
                DirectExchange::Answered {
                    card: from,
                    kind: answered_kind,
                    outcome,
                    cause,
                } if from == card(seed) && answered_kind == kind => Some((outcome, cause)),
                DirectExchange::Made { .. } | DirectExchange::Answered { .. } => None,
            })
            .collect();
        (made, answered)
    }

    /// How many answers to its requests of the kind to the seed's card the member has applied, read once none of those requests awaits its answer, failing when one still does after the longest those requests take to settle.
    pub(in crate::node) async fn settled_direct_answers(
        member: &Member,
        seed: u8,
        kind: DirectContact,
    ) -> usize {
        let awaited = format!("{kind:?} to the card of seed {seed} answered");
        found_within(DIRECT_ANSWERS_SETTLED_WITHIN, &awaited, async || {
            let (made, answered) = exchanges_with(member, seed, kind);
            (answered.len() == made).then_some(made)
        })
        .await
        .unwrap_or_else(|missed| {
            panic!(
                "{missed}; made and answered: {:?}",
                exchanges_with(member, seed, kind)
            )
        })
    }

    /// Waits until the member applies an answer of the outcome to a request of the kind to the seed's card that the cause made, after its settled answers, failing with every answer it applied when none comes within the wait.
    pub(in crate::node) async fn direct_answer_after(
        member: &Member,
        seed: u8,
        kind: DirectContact,
        settled: usize,
        cause: RequestCause,
        outcome: DirectOutcome,
        within: Duration,
    ) {
        let awaited = format!(
            "{outcome:?} answer to a {kind:?} to the card of seed {seed} made by {cause:?} after its {settled} settled answers"
        );
        found_within(within, &awaited, async || {
            exchanges_with(member, seed, kind)
                .1
                .iter()
                .skip(settled)
                .any(|answered| *answered == (outcome, Some(cause)))
                .then_some(())
        })
        .await
        .unwrap_or_else(|missed| {
            panic!(
                "{missed}; it applied {:?}",
                exchanges_with(member, seed, kind).1
            )
        });
    }

    const LONGER_THAN_ANY_SILENCE: Duration = Duration::from_secs(3600);
    const INTRODUCED_WAIT: Duration = Duration::from_secs(10);

    /// A peer holding the witness card that queues every Message it accepts and, at the first, has its register close every connection of the sender before the ack is written, as the rule closes a connection with a Message in flight.
    struct ClosingAtTheFirstMessage {
        addr: Addr,
        register: HeldConnections,
        register_clock: ShiftedClock,
        closed_once: AtomicBool,
        queued: Arc<Mutex<Vec<Message>>>,
        introductions: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl DirectHost for ClosingAtTheFirstMessage {
        fn is_stopping(&self) -> bool {
            false
        }

        fn own_card(&self) -> Option<crate::wire::PublicKey> {
            Some(card(WITNESS_SEED))
        }

        fn is_attached(&self) -> bool {
            false
        }

        fn receive_message(&self, sender: crate::wire::PublicKey, message: Message) -> bool {
            self.queued.lock().unwrap().push(message);
            if !self.closed_once.swap(true, Ordering::SeqCst) {
                self.register_clock.move_forward(LONGER_THAN_ANY_SILENCE);
                self.register.close_silent_of(sender);
                self.register_clock.move_back(LONGER_THAN_ANY_SILENCE);
            }
            false
        }

        fn receive_introduce(
            &self,
            _sender: crate::wire::PublicKey,
            _introduce: Introduce,
        ) -> IntroduceAck {
            self.introductions.fetch_add(1, Ordering::SeqCst);
            IntroduceAck {
                v: Version::CURRENT,
                handle: None,
                public: Vec::new(),
                attached: false,
                addr: self.addr.clone(),
                mac: ZERO_MAC,
            }
        }
    }

    #[tokio::test]
    async fn spec_3_10_a_message_whose_connection_its_recipient_closes_after_queueing_it_fails_at_its_sender_as_unreachable_and_its_resend_is_queued_twice()
     {
        let scratch = Scratch::new("cut").await;
        let register_clock = ShiftedClock::default();
        let queued = Arc::new(Mutex::new(Vec::new()));
        let introductions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let endpoint = bind_holding(
            WITNESS_SEED,
            HeldConnections::new(
                register_clock.monotonic(),
                crate::net::direct::tests::dropping_transport(),
            ),
        )
        .await;
        let first = start_members_reaching(
            &scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).reaching(&[WITNESS_SEED])],
            &[(WITNESS_SEED, endpoint.endpoint().addr())],
        )
        .await
        .pop()
        .unwrap();
        endpoint.replace_address(first.endpoint.addr());
        let _router = endpoint
            .endpoint()
            .router_builder()
            .accept(
                DIRECT_ALPN,
                DirectServer::new(
                    keys(),
                    ClosingAtTheFirstMessage {
                        addr: wire_addr(&endpoint.endpoint().addr()),
                        register: endpoint.held_connections(),
                        register_clock,
                        closed_once: AtomicBool::new(false),
                        queued: Arc::clone(&queued),
                        introductions: Arc::clone(&introductions),
                    },
                    &endpoint,
                ),
            )
            .spawn();
        DirectClient::new(
            endpoint.endpoint().clone(),
            keys(),
            ClientTimeouts::SPEC,
            crate::net::direct::tests::unrecorded_register(),
        )
        .send_message(first.endpoint.id(), message_to(FIRST_SEED, WITNESS_HANDLE))
        .await
        .unwrap();
        let introduced = tokio::time::timeout(INTRODUCED_WAIT, async {
            while introductions.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            introduced.is_ok(),
            "the first member did not introduce itself to the peer within {INTRODUCED_WAIT:?}"
        );

        let failed = tokio::time::timeout(
            ClientTimeouts::SPEC.ack,
            first.node.send(WITNESS_HANDLE, MESSAGE_BODY),
        )
        .await;
        let status_after_the_failure = entry_held_now(&first, WITNESS_SEED).map(|peer| peer.status);
        let resent = first.node.send(WITNESS_HANDLE, MESSAGE_BODY).await;
        let status_after_the_resend = entry_held_now(&first, WITNESS_SEED).map(|peer| peer.status);
        let queued = queued.lock().unwrap().clone();

        assert_eq!(
            failed.map(|sent| sent.map_err(crate::errors::ErrorCode::from)),
            Ok(Err(crate::errors::ErrorCode::Unreachable))
        );
        assert_eq!(status_after_the_failure, Some(Status::Unreachable));
        assert!(
            matches!(resent, Ok(crate::node::Delivery { .. })),
            "{resent:?}"
        );
        assert_eq!(status_after_the_resend, Some(Status::Online));
        assert_eq!(
            queued
                .iter()
                .map(|message| message.body.as_str())
                .collect::<Vec<_>>(),
            vec![MESSAGE_BODY, MESSAGE_BODY]
        );
        assert_ne!(queued[0].id, queued[1].id);
        drop(scratch);
    }

    /// When the second member's process restarts after its earlier one vanished.
    enum RestartTiming {
        /// At once, so its gossip connection and Introduce reach the first member before the earlier process's connections read as gone.
        AtOnce,
        /// Once a witness member has joined the first member, which takes the spec 3.14 observer wait, so its Introduce reaches the first member after the earlier process's connections read as gone.
        AfterAWitnessJoins,
    }

    #[tokio::test]
    async fn spec_3_15_test_18_a_restarted_member_whose_connections_its_sender_closed_at_its_presence_and_that_sender_list_each_other_online_and_exchange_messages_both_ways()
     {
        a_restarted_second_and_the_first_list_each_other_and_exchange_both_ways(
            "restart-both-ways",
            RestartTiming::AtOnce,
        )
        .await;
    }

    #[tokio::test]
    async fn spec_3_15_test_18_a_restarted_member_whose_introduce_comes_after_its_earlier_process_fell_silent_and_its_sender_exchange_messages_both_ways()
     {
        a_restarted_second_and_the_first_list_each_other_and_exchange_both_ways(
            "late-introduce",
            RestartTiming::AfterAWitnessJoins,
        )
        .await;
    }

    /// Starts the member of the plan on this runtime, its endpoint reaching the address, with its inbox and network file under the scratch index given.
    async fn member_reaching(
        scratch: &Scratch,
        plan: MemberPlan,
        reaching: &iroh::EndpointAddr,
        files_index: u8,
    ) -> Member {
        let endpoint = bind(plan.seed).await;
        endpoint.replace_address(reaching.clone());
        start_member(
            plan,
            endpoint,
            (
                scratch.inbox(files_index),
                scratch
                    .directories
                    .inbox_socket_file(u32::from(files_index)),
            ),
            scratch.network_file(files_index),
        )
        .await
    }

    /// The witness member the timing has join the first member before the restart, none for a restart at once.
    async fn witness_joined_at(
        timing: RestartTiming,
        scratch: &Scratch,
        first_address: &iroh::EndpointAddr,
    ) -> Option<Member> {
        match timing {
            RestartTiming::AtOnce => None,
            RestartTiming::AfterAWitnessJoins => Some(
                member_reaching(
                    scratch,
                    MemberPlan::new(WITNESS_SEED, WITNESS_HANDLE, WITNESS_PUBLIC)
                        .with_hosts(&[FIRST_SEED]),
                    first_address,
                    WITNESS_SEED,
                )
                .await,
            ),
        }
    }

    /// Runs the second member, vanishes it, restarts it under its card at the timing given, and requires the first member and the restarted one to list each other online and deliver a message each way (spec 3.15 test 18).
    async fn a_restarted_second_and_the_first_list_each_other_and_exchange_both_ways(
        tag: &str,
        timing: RestartTiming,
    ) {
        let scratch = Scratch::new(tag).await;
        let first_endpoint = bind(FIRST_SEED).await;
        let first_address = first_endpoint.endpoint().addr();
        let first_session = MemberOnItsOwnRuntime::start(
            &scratch,
            MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC).with_hosts(&[FIRST_SEED]),
            vec![(FIRST_SEED, first_address.clone())],
        )
        .await;
        let first = start_member(
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC),
            first_endpoint,
            (
                scratch.inbox(FIRST_SEED),
                scratch.directories.inbox_socket_file(u32::from(FIRST_SEED)),
            ),
            scratch.network_file(FIRST_SEED),
        )
        .await;
        let listed_first_session = entry_meeting_within(
            &first,
            SECOND_SEED,
            "second member listed online by the first",
            |peer| peer.status == Status::Online,
            RESTARTED_LISTED_WAIT,
        )
        .await;
        let sent_to_the_first_session = first.node.send(SECOND_HANDLE, MESSAGE_BODY).await;

        let first_session = first_session.vanish().await;
        let vanished = tokio::time::Instant::now();
        let vanished_ms = wall_clock_ms();
        let _witness = witness_joined_at(timing, &scratch, &first_address).await;
        let restarted = member_reaching(
            &scratch,
            MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC).with_hosts(&[FIRST_SEED]),
            &first_address,
            RESTARTED_SECOND_INBOX,
        )
        .await;
        let silent_for = vanished.elapsed();
        let listed_restarted = entry_meeting_within(
            &first,
            SECOND_SEED,
            "restarted second member listed online by the first",
            |peer| peer.status == Status::Online && peer.last_seen > vanished_ms,
            RESTARTED_LISTED_WAIT,
        )
        .await;
        let sent_to_the_restarted = first
            .node
            .send(SECOND_HANDLE, FIRST_TO_THE_RESTARTED_SECOND)
            .await;
        let listed_by_the_restarted = entry_meeting_within(
            &restarted,
            FIRST_SEED,
            "first member listed online by the restarted second",
            |peer| peer.status == Status::Online,
            RESTARTED_LISTED_WAIT,
        )
        .await;
        let sent_by_the_restarted = restarted
            .node
            .send(FIRST_HANDLE, THE_RESTARTED_SECOND_TO_FIRST)
            .await;

        listed_first_session.unwrap_or_else(|missed| {
            panic!("amy listing the first session of bob online: {missed}")
        });
        assert!(
            sent_to_the_first_session.is_ok(),
            "{sent_to_the_first_session:?}"
        );
        assert!(
            silent_for >= GONE_PROCESS_SILENCE.saturating_add(HEARING_SAMPLE_INTERVAL),
            "the first session was silent only {silent_for:?} when the restarted member announced itself, under the {GONE_PROCESS_SILENCE:?} close bound plus the {HEARING_SAMPLE_INTERVAL:?} sample that judges it"
        );
        listed_restarted
            .unwrap_or_else(|missed| panic!("amy listing the restarted bob online: {missed}"));
        assert!(sent_to_the_restarted.is_ok(), "{sent_to_the_restarted:?}");
        listed_by_the_restarted
            .unwrap_or_else(|missed| panic!("the restarted bob listing amy online: {missed}"));
        assert!(sent_by_the_restarted.is_ok(), "{sent_by_the_restarted:?}");
        let received_by_the_restarted = inbox_frames(&restarted, 1).await;
        let received_by_first = inbox_frames(&first, 1).await;
        assert_eq!(
            received_by_the_restarted[0]["body"],
            FIRST_TO_THE_RESTARTED_SECOND
        );
        assert_eq!(received_by_first[0]["body"], THE_RESTARTED_SECOND_TO_FIRST);
        first_session.forget().await;
        drop(scratch);
    }
}
