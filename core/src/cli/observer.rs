use std::fmt;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use clap::Subcommand;
use data_encoding::HEXLOWER;
use iroh::SecretKey;
use iroh::endpoint::BindError;
use iroh::protocol::Router;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinError;
use tokio::time::Instant;

use crate::card::{Card, CardInvalid};
use crate::cli::network::NetworkFilesystem;
use crate::config::{ConfigError, NetworkConfig};
use crate::crypto::GateKey;
use crate::dirs::{Directories, DirectoriesError};
use crate::errors::ErrorCode;
use crate::handle::{AnnouncedHandle, Handle, HandleError, LocalHandle};
use crate::inbox::socket::{InboxSocket, InboxSocketError};
use crate::keys::{KeyError, parse_public_key};
use crate::net::direct::{ClientTimeouts, DirectClient, DirectError, DirectHost};
use crate::net::endpoint::NodeEndpoint;
use crate::net::gossip::{GossipError, GossipEvent, NetworkTopic};
use crate::node::{
    DEPARTURE_WAIT, Delivery, Identity, KnownHostAddress, Network, Node, NodeEffects, NodeError,
    NodeRecord, RelayProbe, Session, endpoint_addr, endpoint_id, known_host_addresses,
    observer_wait_over_at, seed_lookup_with_known_hosts, serve, sleep_until_deadline, wire_addr,
};
use crate::payload::ZERO_MAC;
use crate::peers::{Effect, Peer, PeerTable, Status};
use crate::sidecar::{HeldSidecar, Pause, SEED_BYTES, SidecarError, claim, create_card};
use crate::wire::{self, Addr, Introduce, IntroduceAck, Message, Nonce, Payload, Version};

const OBSERVER_PUBLIC: &[u8] = b"{}";
const OPERATOR_CARD: &str = "operator";
const DELIVERED_KIND: &str = "delivered";
const NO_PUBLIC_FIELDS: &str = "-";
const ONLINE_STATUS: &str = "online";
const UNREACHABLE_STATUS: &str = "unreachable";
const GONE_STATUS: &str = "gone";
const PRESENCE_KIND: &str = "presence";
const DEPARTURE_KIND: &str = "departure";
const UNREACHABLE_KIND: &str = "unreachable";
const INFRASTRUCTURE_KIND: &str = "infrastructure";
const OWNER_FIELD: &str = "owner";
const FIELD_SEPARATOR: char = '\t';
const LINE_END: &str = "\n";

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
type MonotonicClock = Arc<dyn Fn() -> Instant + Send + Sync>;
type ProbeNonceSource = Arc<dyn Fn(&mut Nonce) + Send + Sync>;
type RunRecord = Arc<dyn Fn(NodeRecord) + Send + Sync>;
type SeedSource = Arc<dyn Fn(&mut [u8; SEED_BYTES]) + Send + Sync>;
type EndpointBinding = Pin<Box<dyn Future<Output = Result<NodeEndpoint, BindError>> + Send>>;
type BindEndpoint =
    Box<dyn FnOnce(SecretKey, GateKey, Vec<iroh::RelayUrl>) -> EndpointBinding + Send>;
type StartInbox = Box<dyn FnOnce() -> Result<Arc<InboxSocket>, InboxSocketError> + Send>;
type Interrupted = Pin<Box<dyn Future<Output = ()> + Send>>;

#[derive(Debug, Clone, Subcommand)]
pub enum ObserverCommand {
    /// Join the network without a card, wait out the observer wait, and print each peer as handle, status, attached and public, refusing a network without a configuration file
    ShowPeers {
        /// The network whose members to list
        network: Handle,
    },
    /// Join the network without a card and print each gossip payload as it arrives until interrupted, refusing a network without a configuration file
    Tail {
        /// The network whose gossip to print
        network: Handle,
    },
    /// Send one message as the reserved operator card, creating the card when absent, and print the delivery, refusing a network without a configuration file or a card another process holds
    TestSend {
        /// The network to send on
        network: Handle,
        /// The handle of the member to send to
        handle: LocalHandle,
        /// The text of the message
        body: String,
    },
}

/// The side effects every observer command runs on, initialized by the composition root.
pub struct ObserverEffects {
    pub clock: Clock,
    pub monotonic: MonotonicClock,
    pub fill_seed: SeedSource,
    pub bind_endpoint: BindEndpoint,
    pub interrupted: Interrupted,
}

/// The side effects only a node that announces itself needs, which `test-send` alone runs on.
pub struct OperatorEffects {
    pub fill_probe_nonce: ProbeNonceSource,
    pub report: RunRecord,
    pub probe_relay: RelayProbe,
    pub start_inbox: StartInbox,
}

/// Why an observer command failed, printed as its message alone.
#[derive(Debug)]
enum Failure {
    Directories(DirectoriesError),
    Config(ConfigError),
    Node(NodeError),
    Bind(BindError),
    Topic(GossipError),
    Inbox(InboxSocketError),
    Io { path: PathBuf, source: io::Error },
    Sidecar(SidecarError),
    CardInvalid { path: PathBuf, reason: CardInvalid },
    CardKey { path: PathBuf, source: KeyError },
    CardHandle { path: PathBuf, source: HandleError },
    Session(ErrorCode),
    NotDelivered(ErrorCode),
    TopicClosed,
    RouterShutdown(JoinError),
    Output(io::Error),
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directories(source) => write!(formatter, "{source}"),
            Self::Config(source) => write!(formatter, "{source}"),
            Self::Node(source) => write!(formatter, "{source}"),
            Self::Bind(source) => write!(formatter, "cannot bind an endpoint: {source}"),
            Self::Topic(source) => write!(formatter, "the gossip topic failed: {source}"),
            Self::Inbox(source) => write!(formatter, "{source}"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Sidecar(source) => write!(formatter, "{source}"),
            Self::CardInvalid { path, reason } => {
                write!(
                    formatter,
                    "{} is not a valid card: {reason}",
                    path.display()
                )
            }
            Self::CardKey { path, source } => write!(
                formatter,
                "{} holds a public key that is not usable: {source}",
                path.display()
            ),
            Self::CardHandle { path, source } => write!(
                formatter,
                "{} holds a handle that is not usable: {source}",
                path.display()
            ),
            Self::Session(code) => write!(formatter, "the session did not take the node: {code}"),
            Self::TopicClosed => formatter.write_str("the gossip topic closed"),
            Self::RouterShutdown(source) => {
                write!(
                    formatter,
                    "the listeners did not shut down cleanly: {source}"
                )
            }
            Self::NotDelivered(code) => write!(formatter, "the message was not delivered: {code}"),
            Self::Output(source) => write!(formatter, "cannot write the result: {source}"),
        }
    }
}

/// An observer command that could not run, stated for whoever ran it.
#[derive(Debug)]
pub struct ObserverCommandError(Failure);

impl fmt::Display for ObserverCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for ObserverCommandError {}

/// Runs one observer command that joins a network as a node, writing the result to `stdout`.
///
/// # Errors
///
/// Returns [`ObserverCommandError`] naming what the operator must change.
pub async fn run(
    command: &ObserverCommand,
    directories: &Directories,
    filesystem: &(impl NetworkFilesystem + Sync),
    pause: &(impl Pause + Sync),
    effects: ObserverEffects,
    operator_effects: OperatorEffects,
    stdout: &mut (impl Write + Send),
) -> Result<(), ObserverCommandError> {
    run_command(
        command,
        directories,
        filesystem,
        pause,
        effects,
        operator_effects,
        stdout,
    )
    .await
    .map_err(ObserverCommandError)
}

/// Runs the command, naming each failure by its own variant.
async fn run_command(
    command: &ObserverCommand,
    directories: &Directories,
    filesystem: &(impl NetworkFilesystem + Sync),
    pause: &(impl Pause + Sync),
    effects: ObserverEffects,
    operator_effects: OperatorEffects,
    stdout: &mut (impl Write + Send),
) -> Result<(), Failure> {
    match command {
        ObserverCommand::ShowPeers { network } => {
            let setup = load_network_setup(directories, network)?;
            let peers = start_observation(&setup, effects)
                .await?
                .collect_peers()
                .await?;
            for peer in &peers {
                write_line(stdout, &peer_line(peer))?;
            }
            Ok(())
        }
        ObserverCommand::Tail { network } => {
            let setup = load_network_setup(directories, network)?;
            start_observation(&setup, effects).await?.tail(stdout).await
        }
        ObserverCommand::TestSend {
            network,
            handle,
            body,
        } => {
            let setup = load_network_setup(directories, network)?;
            let operator =
                claim_operator_card(directories, filesystem, pause, network, &effects.fill_seed)?;
            let delivery =
                send_as_operator(setup, operator, handle, body, (effects, operator_effects))
                    .await?;
            write_line(stdout, &delivery_line(&delivery))
        }
    }
}

/// The reserved operator card as claimed: its held lock, its requested handle and its public mapping.
struct OperatorCard {
    held: HeldSidecar,
    handle: AnnouncedHandle,
    public: Vec<u8>,
}

/// Joins the network as the operator card, sends the body once and releases the card.
async fn send_as_operator(
    setup: NetworkSetup,
    operator: OperatorCard,
    to: &LocalHandle,
    body: &str,
    (effects, operator_effects): (ObserverEffects, OperatorEffects),
) -> Result<Delivery, Failure> {
    let ObserverEffects {
        clock,
        monotonic,
        bind_endpoint,
        ..
    } = effects;
    let OperatorEffects {
        fill_probe_nonce,
        report,
        probe_relay,
        start_inbox,
    } = operator_effects;
    let card_key = operator.held.secret_key().clone();
    let endpoint = bind_endpoint(
        card_key.clone(),
        *setup.network.keys().gate_key(),
        setup.hosts.iter().map(|host| host.relay.clone()).collect(),
    )
    .await
    .map_err(Failure::Bind)?;
    seed_lookup_with_known_hosts(&endpoint, &setup.hosts);
    let inbox = start_inbox().map_err(Failure::Inbox)?;
    let node = Node::start(
        endpoint,
        Identity {
            card_key,
            requested_handle: operator.handle,
            public: operator.public,
        },
        setup.network,
        NodeEffects {
            clock,
            monotonic,
            fill_probe_nonce,
            report,
            inbox,
            probe_relay,
        },
    )
    .await
    .map_err(Failure::Node)?;
    let session = Session::new();
    let node = session
        .claim(node, operator.held)
        .map_err(Failure::Session)?;
    let delivery = node.send(to.as_str(), body).await;
    release_card(&session).await;
    delivery.map_err(Failure::NotDelivered)
}

/// Claims the reserved operator card from the registry, creating it first when absent.
fn claim_operator_card(
    directories: &Directories,
    filesystem: &impl NetworkFilesystem,
    pause: &impl Pause,
    network: &Handle,
    fill_seed: &SeedSource,
) -> Result<OperatorCard, Failure> {
    let card_file = directories
        .card_file(network.as_str(), OPERATOR_CARD)
        .map_err(Failure::Directories)?;
    let sidecar_file = directories
        .card_sidecar_file(network.as_str(), OPERATOR_CARD)
        .map_err(Failure::Directories)?;
    let unreadable = |source: io::Error| Failure::Io {
        path: card_file.clone(),
        source,
    };
    if !filesystem.path_exists(&card_file).map_err(unreadable)? {
        create_operator_card(
            directories,
            filesystem,
            network,
            fill_seed,
            &card_file,
            &sidecar_file,
        )?;
    }
    let text = filesystem.read_to_string(&card_file).map_err(unreadable)?;
    let card = Card::from_str(&text).map_err(|reason| Failure::CardInvalid {
        path: card_file.clone(),
        reason,
    })?;
    let public_key = parse_public_key(&card.public_key).map_err(|source| Failure::CardKey {
        path: card_file.clone(),
        source,
    })?;
    let handle = card
        .handle
        .parse::<AnnouncedHandle>()
        .map_err(|source| Failure::CardHandle {
            path: card_file.clone(),
            source,
        })?;
    let held = claim(&sidecar_file, public_key.as_bytes(), pause).map_err(Failure::Sidecar)?;
    Ok(OperatorCard {
        held,
        handle,
        public: card.public_json(),
    })
}

/// Writes the operator card and its sidecar from a fresh seed.
fn create_operator_card(
    directories: &Directories,
    filesystem: &impl NetworkFilesystem,
    network: &Handle,
    fill_seed: &SeedSource,
    card_file: &Path,
    sidecar_file: &Path,
) -> Result<(), Failure> {
    let cards_directory = directories
        .cards_directory(network.as_str())
        .map_err(Failure::Directories)?;
    filesystem
        .create_private_directory(&cards_directory)
        .map_err(|source| Failure::Io {
            path: cards_directory,
            source,
        })?;
    let mut fresh_seed = [0; SEED_BYTES];
    fill_seed(&mut fresh_seed);
    create_card(card_file, sidecar_file, OPERATOR_CARD, fresh_seed).map_err(Failure::Sidecar)?;
    Ok(())
}

/// Has the session depart and release the card as when standard input ends.
async fn release_card(session: &Session) {
    let (end_of_input, ended) = oneshot::channel();
    end_of_input.send(()).unwrap_or_default();
    let (_signals_sender, signals) = mpsc::unbounded_channel();
    session.run_until_exit(ended, signals, DEPARTURE_WAIT).await;
}

/// The result line of a delivered message.
fn delivery_line(delivery: &Delivery) -> String {
    line_of(&[DELIVERED_KIND, &delivery.id, &delivery.attached.to_string()])
}

/// Writes the line to the result stream and flushes it.
fn write_line(stdout: &mut impl Write, line: &str) -> Result<(), Failure> {
    stdout
        .write_all(line.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(Failure::Output)
}

/// What a network command needs from its configuration file: the keys, the owner and the hosts.
struct NetworkSetup {
    network: Network,
    owner: iroh::PublicKey,
    hosts: Vec<KnownHostAddress>,
}

/// Reads and validates the configuration file of the network.
fn load_network_setup(
    directories: &Directories,
    network: &Handle,
) -> Result<NetworkSetup, Failure> {
    let path = directories
        .network_config_file(network.as_str())
        .map_err(Failure::Directories)?;
    let config = NetworkConfig::load(&path).map_err(Failure::Config)?;
    let hosts = known_host_addresses(&config).map_err(Failure::Node)?;
    let owner = parse_public_key(&config.owner).map_err(|source| {
        Failure::Node(NodeError::NetworkKeyInvalid {
            field: OWNER_FIELD,
            source,
        })
    })?;
    let network = Network::new(config, path).map_err(Failure::Node)?;
    Ok(NetworkSetup {
        network,
        owner,
        hosts,
    })
}

/// Joins the network as an observer without a card on a fresh key, ready to be driven by `collect_peers` or `tail`.
async fn start_observation(
    setup: &NetworkSetup,
    effects: ObserverEffects,
) -> Result<Observation, Failure> {
    let mut seed = [0; SEED_BYTES];
    (effects.fill_seed)(&mut seed);
    let secret_key = SecretKey::from_bytes(&seed);
    let keys = setup.network.keys();
    let endpoint = (effects.bind_endpoint)(
        secret_key.clone(),
        *keys.gate_key(),
        setup.hosts.iter().map(|host| host.relay.clone()).collect(),
    )
    .await
    .map_err(Failure::Bind)?;
    seed_lookup_with_known_hosts(&endpoint, &setup.hosts);
    let own_card = *secret_key.public().as_bytes();
    let (to_loop, from_loop) = mpsc::unbounded_channel();
    let host = Arc::new(ObserverHost {
        endpoint: endpoint.endpoint().clone(),
        table: Mutex::new(PeerTable::new(own_card)),
        clock: effects.clock,
        to_loop,
    });
    let (router, gossip) = serve(&endpoint, keys, &host);
    let topic = NetworkTopic::join(
        &gossip,
        keys.clone(),
        setup.owner,
        setup.hosts.iter().map(|host| host.id).collect(),
        (effects.monotonic)(),
    )
    .await;
    match topic {
        Ok(topic) => Ok(Observation {
            client: Arc::new(DirectClient::new(
                endpoint.endpoint().clone(),
                keys.clone(),
                ClientTimeouts::SPEC,
            )),
            host,
            endpoint,
            router,
            topic,
            from_loop,
            monotonic: effects.monotonic,
            interrupted: effects.interrupted,
            wait_over: None,
        }),
        Err(failure) => {
            drop(router.shutdown().await);
            Err(Failure::Topic(failure))
        }
    }
}

/// What reaches the observer loop from the direct server and from the requests it spawned.
enum ObserverEvent {
    Effects(Vec<Effect>),
    IntroduceAnswered {
        card: wire::PublicKey,
        answer: Result<IntroduceAck, DirectError>,
    },
}

/// The direct protocol host of an observer: no card, no inbox, and an introduction without a handle.
struct ObserverHost {
    endpoint: iroh::Endpoint,
    table: Mutex<PeerTable>,
    clock: Clock,
    to_loop: mpsc::UnboundedSender<ObserverEvent>,
}

impl ObserverHost {
    /// Milliseconds since the Unix epoch by the clock the composition root handed in.
    fn now_ms(&self) -> u64 {
        (self.clock)()
    }

    /// The peer table, locked for one operation.
    fn table(&self) -> MutexGuard<'_, PeerTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands an event to the observer loop, which is gone only once the run is.
    fn hand_to_loop(&self, event: ObserverEvent) {
        drop(self.to_loop.send(event));
    }

    /// This observer's introduction: no handle, an empty public mapping, not attached, and its current address.
    fn introduction(&self) -> Introduce {
        let own_address = self.endpoint.addr();
        Introduce {
            v: Version::CURRENT,
            handle: None,
            public: OBSERVER_PUBLIC.to_vec(),
            attached: false,
            addr: wire_addr(&own_address),
            mac: ZERO_MAC,
        }
    }
}

impl DirectHost for Arc<ObserverHost> {
    fn is_stopping(&self) -> bool {
        false
    }

    fn own_card(&self) -> Option<wire::PublicKey> {
        None
    }

    fn is_attached(&self) -> bool {
        false
    }

    fn receive_message(&self, _sender: wire::PublicKey, _message: Message) {}

    fn receive_introduce(&self, sender: wire::PublicKey, introduce: Introduce) -> IntroduceAck {
        let effects = self
            .table()
            .receive_introduce(sender, &introduce, self.now_ms());
        self.hand_to_loop(ObserverEvent::Effects(effects));
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

/// The topic, the direct client and the peer table of one observer run.
struct Observation {
    host: Arc<ObserverHost>,
    endpoint: NodeEndpoint,
    client: Arc<DirectClient>,
    topic: NetworkTopic,
    router: Router,
    from_loop: mpsc::UnboundedReceiver<ObserverEvent>,
    monotonic: MonotonicClock,
    interrupted: Interrupted,
    wait_over: Option<Instant>,
}

/// What an observer loop wakes for.
enum Step {
    Topic(Option<Result<GossipEvent, GossipError>>),
    Loop(ObserverEvent),
    WaitOver,
    Interrupted,
}

impl Observation {
    /// Handles events until the observer wait is over, the topic closes or the run is interrupted, then returns the peers.
    async fn collect_peers(mut self) -> Result<Vec<Peer>, Failure> {
        let outcome = self.collect_until_done().await;
        let shutdown = self.router.shutdown().await;
        let peers = outcome?;
        shutdown.map_err(Failure::RouterShutdown)?;
        Ok(peers)
    }

    async fn collect_until_done(&mut self) -> Result<Vec<Peer>, Failure> {
        loop {
            match self.next_step().await {
                Step::Topic(None) | Step::WaitOver | Step::Interrupted => break,
                Step::Topic(Some(Err(failure))) => return Err(Failure::Topic(failure)),
                Step::Topic(Some(Ok(GossipEvent::Joined | GossipEvent::JoinTimedOut))) => {
                    let joined_at = (self.monotonic)();
                    self.wait_over
                        .get_or_insert_with(|| observer_wait_over_at(joined_at));
                }
                Step::Topic(Some(Ok(event))) => self.take_gossip_event(event),
                Step::Loop(event) => self.take_loop_event(event),
            }
        }
        let now_ms = self.host.now_ms();
        Ok(self.host.table().peers(now_ms).cloned().collect())
    }

    /// Prints each payload as it arrives until the run is interrupted, ending in failure when the topic closes by itself.
    async fn tail(mut self, stdout: &mut impl Write) -> Result<(), Failure> {
        let outcome = self.tail_until_done(stdout).await;
        let shutdown = self.router.shutdown().await;
        outcome?;
        shutdown.map_err(Failure::RouterShutdown)
    }

    async fn tail_until_done(&mut self, stdout: &mut impl Write) -> Result<(), Failure> {
        loop {
            match self.next_step().await {
                Step::Interrupted => return Ok(()),
                Step::WaitOver
                | Step::Topic(Some(Ok(GossipEvent::Joined | GossipEvent::JoinTimedOut))) => {}
                Step::Topic(None) => return Err(Failure::TopicClosed),
                Step::Topic(Some(Err(failure))) => return Err(Failure::Topic(failure)),
                Step::Topic(Some(Ok(event))) => {
                    if let GossipEvent::Payload(payload) = &event {
                        write_line(stdout, &payload_line(payload))?;
                    }
                    self.take_gossip_event(event);
                }
                Step::Loop(event) => self.take_loop_event(event),
            }
        }
    }

    /// Waits for the next topic event, loop event, observer wait deadline or interrupt.
    async fn next_step(&mut self) -> Step {
        tokio::select! {
            event = self.topic.next_event() => Step::Topic(event),
            Some(event) = self.from_loop.recv() => Step::Loop(event),
            () = sleep_until_deadline(self.wait_over) => Step::WaitOver,
            () = &mut self.interrupted => Step::Interrupted,
        }
    }

    /// Applies the rows of spec 3.8 a topic event after the join triggers.
    fn take_gossip_event(&self, event: GossipEvent) {
        let now_ms = self.host.now_ms();
        match event {
            GossipEvent::Joined | GossipEvent::JoinTimedOut => {}
            GossipEvent::NeighborUp(node) => {
                let effects = self.host.table().neighbor_up(*node.as_bytes(), now_ms);
                self.perform(effects);
            }
            GossipEvent::NeighborDown(node) => {
                let effects = self.host.table().neighbor_down(*node.as_bytes(), now_ms);
                self.perform(effects);
            }
            GossipEvent::Payload(payload) => self.take_payload(*payload, now_ms),
        }
    }

    /// Applies a verified payload to the peer table.
    fn take_payload(&self, payload: Payload, now_ms: u64) {
        match payload {
            Payload::Presence(presence) => {
                let effects = self.host.table().receive_presence(&presence, now_ms);
                self.perform(effects);
            }
            Payload::Departure(departure) => {
                self.host.table().receive_departure(&departure, now_ms);
            }
            Payload::Unreachable(report) => {
                self.host.table().receive_unreachable(&report, now_ms);
            }
            Payload::Infrastructure(_record) => {}
        }
    }

    /// Applies the effects the direct server handed over and the answer of each Introduce this node sent.
    fn take_loop_event(&self, event: ObserverEvent) {
        let now_ms = self.host.now_ms();
        match event {
            ObserverEvent::Effects(effects) => self.perform(effects),
            ObserverEvent::IntroduceAnswered {
                card,
                answer: Ok(ack),
            } => {
                let effects = self.host.table().receive_introduce_ack(card, &ack, now_ms);
                self.perform(effects);
            }
            ObserverEvent::IntroduceAnswered {
                card,
                answer: Err(_failure),
            } => {
                let effects = self.host.table().direct_contact_failed(card, now_ms);
                self.perform(effects);
            }
        }
    }

    /// Performs the lookup and Introduce effects and leaves every effect that sends a payload or probes undone.
    fn perform(&self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::ReplaceLookupEntry { card, addr } => {
                    self.replace_lookup_entry(&card, &addr);
                }
                Effect::SendIntroduce(card) => self.introduce_in_background(card),
                Effect::DropHeldConnection(_card)
                | Effect::SendProbe(_card)
                | Effect::BroadcastUnreachable(_card) => {}
            }
        }
    }

    /// Replaces the card's lookup entry with the carried address.
    fn replace_lookup_entry(&self, card: &wire::PublicKey, addr: &Addr) {
        if let Some(id) = endpoint_id(card) {
            self.endpoint.replace_address(endpoint_addr(id, addr));
        }
    }

    /// Sends this observer's Introduce to the card in a task and hands the answer to the loop.
    fn introduce_in_background(&self, card: wire::PublicKey) {
        let Some(id) = endpoint_id(&card) else {
            return;
        };
        let client = Arc::clone(&self.client);
        let host = Arc::clone(&self.host);
        let introduction = host.introduction();
        tokio::spawn(async move {
            let answer = client.introduce(id, introduction).await;
            host.hand_to_loop(ObserverEvent::IntroduceAnswered { card, answer });
        });
    }
}

/// The result line of a peer: handle, status, attached and public fields.
fn peer_line(peer: &Peer) -> String {
    let public = peer
        .public
        .as_deref()
        .map_or_else(|| NO_PUBLIC_FIELDS.to_owned(), public_text);
    line_of(&[
        peer.effective_handle.as_str(),
        status_text(peer.status),
        &peer.attached.to_string(),
        &public,
    ])
}

/// The result line of a gossip payload.
fn payload_line(payload: &Payload) -> String {
    match payload {
        Payload::Presence(presence) => line_of(&[
            PRESENCE_KIND,
            &card_text(&presence.card),
            &presence.handle,
            &presence.attached.to_string(),
            &public_text(&presence.public),
        ]),
        Payload::Departure(departure) => line_of(&[DEPARTURE_KIND, &card_text(&departure.card)]),
        Payload::Unreachable(report) => line_of(&[
            UNREACHABLE_KIND,
            &card_text(&report.card),
            &card_text(&report.reporter),
        ]),
        Payload::Infrastructure(record) => line_of(&[
            INFRASTRUCTURE_KIND,
            &card_text(&record.host),
            record.relay.as_str(),
        ]),
    }
}

/// Joins the fields into one tab-separated line.
fn line_of(fields: &[&str]) -> String {
    let escaped: Vec<String> = fields.iter().map(|field| escape_controls(field)).collect();
    escaped.join(&FIELD_SEPARATOR.to_string()) + LINE_END
}

/// The field with every control character, tab and newline included, spelled out so it cannot end a column or a line.
fn escape_controls(field: &str) -> String {
    field
        .chars()
        .flat_map(|character| {
            let spelled: Vec<char> = if character.is_control() {
                character.escape_default().collect()
            } else {
                vec![character]
            };
            spelled
        })
        .collect()
}

/// A card public key as lowercase hexadecimal.
fn card_text(card: &wire::PublicKey) -> String {
    HEXLOWER.encode(card)
}

/// A public mapping as the text of its JSON bytes.
fn public_text(public: &[u8]) -> String {
    String::from_utf8_lossy(public).into_owned()
}

/// The word a peer status prints as.
const fn status_text(status: Status) -> &'static str {
    match status {
        Status::Online => ONLINE_STATUS,
        Status::Unreachable => UNREACHABLE_STATUS,
        Status::Gone { .. } => GONE_STATUS,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use iroh::EndpointAddr;

    use super::*;
    use crate::config::{Host, HostOrigin, NetworkConfig};
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use crate::dirs::OperatingSystemFilesystem;
    use crate::handle::AnnouncedHandle;
    use crate::inbox::socket::ProcessTable;
    use crate::keys::render_public_key;
    use crate::node::{Identity, Network, Node, NodeEffects};

    const NETWORK_NAME: &str = "lab";
    const NETWORK_SECRET: [u8; KEY_LEN] = [33; KEY_LEN];
    const OWNER_SEED: u8 = 60;
    const FIRST_SEED: u8 = 61;
    const SECOND_SEED: u8 = 62;
    const OBSERVER_SEED: u8 = 70;
    const OPERATOR_SEED: u8 = 71;
    const NEIGHBOR_SEED: u8 = 72;
    const NEIGHBOR_HANDLE: &str = "zed";
    const NEIGHBOR_PUBLIC: &[u8] = br#"{"role":"neighbor"}"#;
    const FIRST_HANDLE: &str = "amy";
    const SECOND_HANDLE: &str = "bob";
    const FIRST_PUBLIC: &[u8] = br#"{"role":"first"}"#;
    const SECOND_PUBLIC: &[u8] = br#"{"role":"second"}"#;
    const UNREACHABLE_RELAY: &str = "https://relay.estafette.invalid/";
    const PROBE_NONCE: Nonce = [7; 16];
    const PROBE_NONCE_AS_MESSAGE_ID: &str = "07070707-0707-0707-0707-070707070707";
    const OPERATOR_CARD: &str = "operator";
    const OPERATOR_BODY: &str = "a test message from the operator";
    const FRAME_WAIT: Duration = Duration::from_secs(2);
    const POLL_INTERVAL: Duration = Duration::from_millis(20);
    const ATTACH_WAIT: Duration = Duration::from_secs(2);
    const DETACH_WAIT: Duration = Duration::from_millis(300);
    const PAYLOAD_WAIT: Duration = Duration::from_secs(20);

    struct EveryProcessRuns;

    impl ProcessTable for EveryProcessRuns {
        fn is_running(&self, _process_id: u32) -> io::Result<bool> {
            Ok(true)
        }
    }

    struct NoPause;

    impl Pause for NoPause {
        fn pause(&self, _duration: Duration) {}
    }

    static LAB_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

    struct Lab {
        root: PathBuf,
        directories: Arc<Directories>,
        _slot: tokio::sync::SemaphorePermit<'static>,
    }

    struct Member {
        node: Node,
        inbox: Arc<InboxSocket>,
        inbox_path: PathBuf,
        address: EndpointAddr,
    }

    struct MemberPlan {
        seed: u8,
        handle: &'static str,
        public: &'static [u8],
    }

    fn secret_key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; KEY_LEN])
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

    fn network_keys() -> NetworkKeys {
        NetworkKeys::derive(&NetworkSecret::from_bytes(NETWORK_SECRET))
    }

    async fn bind_endpoint(key: SecretKey) -> NodeEndpoint {
        NodeEndpoint::bind(
            key,
            *network_keys().gate_key(),
            Vec::new(),
            |nonce: &mut [u8; KEY_LEN]| *nonce = [OBSERVER_SEED; KEY_LEN],
            |_ignored| {},
        )
        .await
        .unwrap()
    }

    impl Lab {
        async fn new(tag: &str) -> Self {
            let slot = LAB_SLOT.acquire().await.unwrap();
            let root = std::env::temp_dir().join(format!("u27-{tag}-{}", std::process::id()));
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
                directories: Arc::new(directories),
                _slot: slot,
            }
        }

        fn write_network_config(&self, host_seeds: &[u8]) {
            let path = self.directories.network_config_file(NETWORK_NAME).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            NetworkConfig {
                secret: data_encoding::HEXLOWER.encode(&NETWORK_SECRET),
                owner: render_public_key(&secret_key(OWNER_SEED).public()),
                hosts: host_seeds
                    .iter()
                    .map(|&seed| Host {
                        id: render_public_key(&secret_key(seed).public()),
                        relay: UNREACHABLE_RELAY.to_owned(),
                        ts: None,
                        origin: HostOrigin::Configured,
                    })
                    .collect(),
            }
            .rewrite(&path, wall_clock_ms() / 1000)
            .unwrap();
        }

        async fn start_members(&self, plans: Vec<MemberPlan>) -> Vec<Member> {
            let mut endpoints = Vec::new();
            for plan in &plans {
                endpoints.push(bind_endpoint(secret_key(plan.seed)).await);
            }
            let addresses: Vec<EndpointAddr> = endpoints
                .iter()
                .map(|endpoint| endpoint.endpoint().addr())
                .collect();
            for endpoint in &endpoints {
                for address in &addresses {
                    endpoint.replace_address(address.clone());
                }
            }
            let starting: Vec<_> = plans
                .into_iter()
                .zip(endpoints)
                .map(|(plan, endpoint)| {
                    let inbox = Arc::new(
                        InboxSocket::start(
                            &self.directories,
                            u32::from(plan.seed),
                            &EveryProcessRuns,
                        )
                        .unwrap(),
                    );
                    let inbox_path = self.directories.inbox_socket_file(u32::from(plan.seed));
                    let config_path = self.root.join(format!("{}-network.toml", plan.seed));
                    tokio::spawn(start_member(plan, endpoint, inbox, inbox_path, config_path))
                })
                .collect();
            let mut members = Vec::new();
            for member in starting {
                members.push(member.await.unwrap());
            }
            members
        }

        fn effects(
            &self,
            member_addresses: Vec<EndpointAddr>,
            interrupted: Interrupted,
        ) -> (ObserverEffects, OperatorEffects) {
            let directories = Arc::clone(&self.directories);
            let effects = ObserverEffects {
                clock: Arc::new(wall_clock_ms),
                monotonic: Arc::new(Instant::now),
                fill_seed: Arc::new(|seed| *seed = [OBSERVER_SEED; SEED_BYTES]),
                bind_endpoint: Box::new(move |key, gate_key, relays| {
                    Box::pin(async move {
                        let endpoint = NodeEndpoint::bind(
                            key,
                            gate_key,
                            relays,
                            |nonce: &mut [u8; KEY_LEN]| *nonce = [OBSERVER_SEED; KEY_LEN],
                            |_ignored| {},
                        )
                        .await?;
                        for address in member_addresses {
                            endpoint.replace_address(address);
                        }
                        Ok(endpoint)
                    })
                }),
                interrupted,
            };
            let operator_effects = OperatorEffects {
                fill_probe_nonce: Arc::new(|nonce: &mut Nonce| *nonce = PROBE_NONCE),
                report: Arc::new(|_record: NodeRecord| {}),
                probe_relay: Arc::new(|_relay| Box::pin(async { true })),
                start_inbox: Box::new(move || {
                    InboxSocket::start(&directories, u32::from(OBSERVER_SEED), &EveryProcessRuns)
                        .map(Arc::new)
                }),
            };
            (effects, operator_effects)
        }
    }

    async fn run(
        command: &ObserverCommand,
        directories: &Directories,
        filesystem: &(impl NetworkFilesystem + Sync),
        pause: &(impl Pause + Sync),
        (effects, operator_effects): (ObserverEffects, OperatorEffects),
        stdout: &mut (impl Write + Send),
    ) -> Result<(), ObserverCommandError> {
        super::run(
            command,
            directories,
            filesystem,
            pause,
            effects,
            operator_effects,
            stdout,
        )
        .await
    }

    impl Drop for Lab {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.root));
        }
    }

    async fn start_member(
        plan: MemberPlan,
        endpoint: NodeEndpoint,
        inbox: Arc<InboxSocket>,
        inbox_path: PathBuf,
        config_path: PathBuf,
    ) -> Member {
        let address = endpoint.endpoint().addr();
        let hosts = [FIRST_SEED, SECOND_SEED]
            .into_iter()
            .filter(|&seed| seed != plan.seed)
            .map(|seed| Host {
                id: render_public_key(&secret_key(seed).public()),
                relay: UNREACHABLE_RELAY.to_owned(),
                ts: None,
                origin: HostOrigin::Configured,
            })
            .collect();
        let node = Node::start(
            endpoint,
            Identity {
                card_key: secret_key(plan.seed),
                requested_handle: plan.handle.parse::<AnnouncedHandle>().unwrap(),
                public: plan.public.to_vec(),
            },
            Network::new(
                NetworkConfig {
                    secret: data_encoding::HEXLOWER.encode(&NETWORK_SECRET),
                    owner: render_public_key(&secret_key(OWNER_SEED).public()),
                    hosts,
                },
                config_path,
            )
            .unwrap(),
            NodeEffects {
                clock: Arc::new(wall_clock_ms),
                monotonic: Arc::new(Instant::now),
                fill_probe_nonce: Arc::new(|nonce: &mut Nonce| *nonce = PROBE_NONCE),
                report: Arc::new(|_record: NodeRecord| {}),
                inbox: Arc::clone(&inbox),
                probe_relay: Arc::new(|_relay| Box::pin(async { true })),
            },
        )
        .await
        .unwrap();
        Member {
            node,
            inbox,
            inbox_path,
            address,
        }
    }

    fn two_members() -> Vec<MemberPlan> {
        vec![
            MemberPlan {
                seed: FIRST_SEED,
                handle: FIRST_HANDLE,
                public: FIRST_PUBLIC,
            },
            MemberPlan {
                seed: SECOND_SEED,
                handle: SECOND_HANDLE,
                public: SECOND_PUBLIC,
            },
        ]
    }

    fn never_interrupted() -> Interrupted {
        Box::pin(std::future::pending())
    }

    async fn attach_client(member: &Member) -> tokio::net::UnixStream {
        let mut stream = tokio::net::UnixStream::connect(&member.inbox_path)
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"{\"since\": 0}\n")
            .await
            .unwrap();
        tokio::time::timeout(ATTACH_WAIT, async {
            while !member.inbox.attached().is_attached() {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .unwrap();
        stream
    }

    #[tokio::test]
    async fn spec_3_15_test_15_show_peers_lists_every_member_by_handle_status_attached_and_public()
    {
        let lab = Lab::new("peers").await;
        let members = lab.start_members(two_members()).await;
        let _attached_client = attach_client(&members[0]).await;
        lab.write_network_config(&[FIRST_SEED, SECOND_SEED]);
        let mut output = Vec::new();

        run(
            &ObserverCommand::ShowPeers {
                network: NETWORK_NAME.parse().unwrap(),
            },
            &lab.directories,
            &OperatingSystemFilesystem,
            &NoPause,
            lab.effects(
                members
                    .iter()
                    .map(|member| member.address.clone())
                    .collect(),
                never_interrupted(),
            ),
            &mut output,
        )
        .await
        .unwrap();

        let mut lines: Vec<String> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            vec![
                format!(
                    "{FIRST_HANDLE}\tonline\ttrue\t{}",
                    String::from_utf8_lossy(FIRST_PUBLIC)
                ),
                format!(
                    "{SECOND_HANDLE}\tonline\tfalse\t{}",
                    String::from_utf8_lossy(SECOND_PUBLIC)
                ),
            ]
        );
        for member in &members {
            assert_eq!(member.node.peers().len(), 1);
        }
        drop(lab);
    }

    #[derive(Clone, Default)]
    struct SharedOutput(Arc<Mutex<Vec<u8>>>);

    impl SharedOutput {
        fn text(&self) -> String {
            String::from_utf8(
                self.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            )
            .unwrap()
        }
    }

    impl Write for SharedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    async fn toggle_attachment_until_output_has(
        member: &Member,
        output: &SharedOutput,
        wanted: &str,
    ) {
        tokio::time::timeout(PAYLOAD_WAIT, async {
            while !output.text().contains(wanted) {
                drop(attach_client(member).await);
                tokio::time::sleep(DETACH_WAIT).await;
            }
        })
        .await
        .unwrap_or_else(|_elapsed| panic!("no payload line containing {wanted:?} arrived"));
    }

    #[tokio::test]
    async fn spec_3_13_tail_prints_each_presence_payload_as_it_arrives_until_interrupted() {
        let lab = Lab::new("tail").await;
        let members = lab.start_members(two_members()).await;
        lab.write_network_config(&[FIRST_SEED, SECOND_SEED]);
        let output = SharedOutput::default();
        let mut tail_output = output.clone();
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let first_presence_line = format!(
            "presence\t{}\t{FIRST_HANDLE}\ttrue\t{}\n",
            render_public_key(&secret_key(FIRST_SEED).public()),
            String::from_utf8_lossy(FIRST_PUBLIC)
        );
        let tail = ObserverCommand::Tail {
            network: NETWORK_NAME.parse().unwrap(),
        };

        let (tail_result, ()) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                &OperatingSystemFilesystem,
                &NoPause,
                lab.effects(
                    members
                        .iter()
                        .map(|member| member.address.clone())
                        .collect(),
                    Box::pin(async move { drop(interrupted.await) }),
                ),
                &mut tail_output,
            ),
            async {
                toggle_attachment_until_output_has(&members[0], &output, &first_presence_line)
                    .await;
                interrupt.send(()).unwrap_or_default();
            }
        );

        tail_result.unwrap();
        assert!(output.text().contains(&first_presence_line));
        drop(lab);
    }

    async fn first_inbox_frame(member: &Member) -> serde_json::Value {
        let mut stream = tokio::net::UnixStream::connect(&member.inbox_path)
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"{\"since\": 0}\n")
            .await
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            FRAME_WAIT,
            tokio::io::AsyncBufReadExt::read_line(
                &mut tokio::io::BufReader::new(stream),
                &mut line,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn spec_3_13_test_send_creates_the_operator_card_delivers_the_body_and_releases_the_card()
    {
        let lab = Lab::new("send").await;
        let members = lab.start_members(two_members()).await;
        lab.write_network_config(&[FIRST_SEED, SECOND_SEED]);
        let mut output = Vec::new();
        let test_send = ObserverCommand::TestSend {
            network: NETWORK_NAME.parse().unwrap(),
            handle: SECOND_HANDLE.parse().unwrap(),
            body: OPERATOR_BODY.to_owned(),
        };

        run(
            &test_send,
            &lab.directories,
            &OperatingSystemFilesystem,
            &NoPause,
            lab.effects(
                members
                    .iter()
                    .map(|member| member.address.clone())
                    .collect(),
                never_interrupted(),
            ),
            &mut output,
        )
        .await
        .unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("delivered\t{PROBE_NONCE_AS_MESSAGE_ID}\tfalse\n")
        );
        let frame = first_inbox_frame(&members[1]).await;
        assert_eq!(frame["from"], OPERATOR_CARD);
        assert_eq!(frame["body"], OPERATOR_BODY);
        let sidecar = lab
            .directories
            .card_sidecar_file(NETWORK_NAME, OPERATOR_CARD)
            .unwrap();
        assert!(!crate::sidecar::is_held(&sidecar).unwrap());
        let operator = members[1]
            .node
            .peers()
            .into_iter()
            .find(|peer| peer.effective_handle.as_str() == OPERATOR_CARD)
            .unwrap();
        assert!(matches!(operator.status, Status::Gone { .. }));
        drop(lab);
    }

    fn register_operator_card(lab: &Lab) -> PathBuf {
        let card_file = lab
            .directories
            .card_file(NETWORK_NAME, OPERATOR_CARD)
            .unwrap();
        let sidecar_file = lab
            .directories
            .card_sidecar_file(NETWORK_NAME, OPERATOR_CARD)
            .unwrap();
        fs::create_dir_all(card_file.parent().unwrap()).unwrap();
        crate::sidecar::create_card(
            &card_file,
            &sidecar_file,
            OPERATOR_CARD,
            [OPERATOR_SEED; SEED_BYTES],
        )
        .unwrap();
        sidecar_file
    }

    fn operator_test_send() -> ObserverCommand {
        ObserverCommand::TestSend {
            network: NETWORK_NAME.parse().unwrap(),
            handle: SECOND_HANDLE.parse().unwrap(),
            body: OPERATOR_BODY.to_owned(),
        }
    }

    #[tokio::test]
    async fn spec_3_13_test_send_claims_an_operator_card_already_in_the_registry() {
        let lab = Lab::new("send-existing").await;
        let members = lab.start_members(two_members()).await;
        lab.write_network_config(&[FIRST_SEED, SECOND_SEED]);
        let sidecar_file = register_operator_card(&lab);
        let mut output = Vec::new();

        run(
            &operator_test_send(),
            &lab.directories,
            &OperatingSystemFilesystem,
            &NoPause,
            lab.effects(
                members
                    .iter()
                    .map(|member| member.address.clone())
                    .collect(),
                never_interrupted(),
            ),
            &mut output,
        )
        .await
        .unwrap();

        assert!(
            String::from_utf8(output)
                .unwrap()
                .starts_with("delivered\t")
        );
        assert_eq!(first_inbox_frame(&members[1]).await["from"], OPERATOR_CARD);
        assert!(!crate::sidecar::is_held(&sidecar_file).unwrap());
        drop(lab);
    }

    #[tokio::test]
    async fn spec_3_13_test_send_refuses_while_another_process_holds_the_operator_card() {
        let lab = Lab::new("send-held").await;
        lab.write_network_config(&[]);
        let sidecar_file = register_operator_card(&lab);
        let card_file = lab
            .directories
            .card_file(NETWORK_NAME, OPERATOR_CARD)
            .unwrap();
        let _holder = crate::sidecar::claim(
            &sidecar_file,
            secret_key(OPERATOR_SEED).public().as_bytes(),
            &NoPause,
        )
        .unwrap();
        assert!(card_file.exists());
        let mut output = Vec::new();

        let refusal = run(
            &operator_test_send(),
            &lab.directories,
            &OperatingSystemFilesystem,
            &NoPause,
            lab.effects(Vec::new(), never_interrupted()),
            &mut output,
        )
        .await
        .unwrap_err();

        assert_eq!(refusal.to_string(), "another process holds this card");
        assert_eq!(String::from_utf8(output).unwrap(), "");
        drop(lab);
    }

    #[derive(Clone)]
    struct RecordingNeighbor {
        card: wire::PublicKey,
        introduces: Arc<Mutex<Vec<Introduce>>>,
    }

    impl DirectHost for RecordingNeighbor {
        fn is_stopping(&self) -> bool {
            false
        }

        fn own_card(&self) -> Option<wire::PublicKey> {
            Some(self.card)
        }

        fn is_attached(&self) -> bool {
            false
        }

        fn receive_message(&self, _sender: wire::PublicKey, _message: Message) {}

        fn receive_introduce(
            &self,
            _sender: wire::PublicKey,
            introduce: Introduce,
        ) -> IntroduceAck {
            self.introduces
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(introduce);
            IntroduceAck {
                v: Version::CURRENT,
                handle: Some(NEIGHBOR_HANDLE.to_owned()),
                public: NEIGHBOR_PUBLIC.to_vec(),
                attached: true,
                addr: Addr::from_endpoint_report(Vec::new(), Vec::new()),
                mac: ZERO_MAC,
            }
        }
    }

    struct SilentNeighbor {
        _router: iroh::protocol::Router,
        _topic: NetworkTopic,
        address: EndpointAddr,
        introduces: Arc<Mutex<Vec<Introduce>>>,
    }

    async fn start_silent_neighbor() -> SilentNeighbor {
        let endpoint = bind_endpoint(secret_key(NEIGHBOR_SEED)).await;
        let introduces = Arc::new(Mutex::new(Vec::new()));
        let neighbor = RecordingNeighbor {
            card: *secret_key(NEIGHBOR_SEED).public().as_bytes(),
            introduces: Arc::clone(&introduces),
        };
        let (router, gossip) = serve(&endpoint, &network_keys(), &neighbor);
        let topic = NetworkTopic::join(
            &gossip,
            network_keys(),
            secret_key(OWNER_SEED).public(),
            Vec::new(),
            Instant::now(),
        )
        .await
        .unwrap();
        SilentNeighbor {
            _router: router,
            _topic: topic,
            address: endpoint.endpoint().addr(),
            introduces,
        }
    }

    #[tokio::test]
    async fn spec_3_13_show_peers_introduces_the_observer_without_a_handle_and_lists_what_the_ack_carries()
     {
        let lab = Lab::new("introduce").await;
        let neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let mut output = Vec::new();
        let show_peers = ObserverCommand::ShowPeers {
            network: NETWORK_NAME.parse().unwrap(),
        };

        run(
            &show_peers,
            &lab.directories,
            &OperatingSystemFilesystem,
            &NoPause,
            lab.effects(vec![neighbor.address.clone()], never_interrupted()),
            &mut output,
        )
        .await
        .unwrap();

        let introduces = neighbor.introduces.lock().unwrap().clone();
        assert_ne!(introduces.len(), 0);
        assert!(
            introduces
                .iter()
                .all(|introduce| introduce.handle.is_none() && introduce.public == b"{}")
        );
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!(
                "{NEIGHBOR_HANDLE}\tonline\ttrue\t{}\n",
                String::from_utf8_lossy(NEIGHBOR_PUBLIC)
            )
        );
        drop(lab);
    }

    #[tokio::test]
    async fn spec_3_13_a_network_without_a_configuration_file_is_refused_naming_the_file() {
        let lab = Lab::new("no-network").await;
        let mut output = Vec::new();
        let show_peers = ObserverCommand::ShowPeers {
            network: NETWORK_NAME.parse().unwrap(),
        };

        let refusal = run(
            &show_peers,
            &lab.directories,
            &OperatingSystemFilesystem,
            &NoPause,
            lab.effects(Vec::new(), never_interrupted()),
            &mut output,
        )
        .await
        .unwrap_err();

        let configuration_file = lab.directories.network_config_file(NETWORK_NAME).unwrap();
        assert!(
            refusal
                .to_string()
                .starts_with(&configuration_file.display().to_string())
        );
        assert_eq!(String::from_utf8(output).unwrap(), "");
        drop(lab);
    }

    const LINE_CLOCK_MS: u64 = 1_000_000;
    const PROPERTY_CASES: usize = 64;
    const HOSTILE_STRIDE: usize = 3;
    const HOSTILE_LENGTHS: usize = 12;
    const HOSTILE_BYTES: [u8; 7] = [b'a', b'\t', b'\n', b'\r', 0x1b, 0xff, b'{'];
    const SENDER_PUBLIC_FIELD_COUNT: usize = 5;
    const PEER_FIELD_COUNT: usize = 4;

    fn card_of(seed: u8) -> wire::PublicKey {
        [seed; 32]
    }

    fn unsigned() -> wire::SignatureBytes {
        wire::SignatureBytes([0; 64])
    }

    fn message_from(handle: &str, attached: bool) -> Message {
        Message {
            v: Version::CURRENT,
            id: [0; 16],
            to: card_of(0),
            handle: handle.to_owned(),
            attached,
            sent_at: LINE_CLOCK_MS,
            body: String::new(),
            mac: ZERO_MAC,
        }
    }

    fn presence_of(seed: u8, handle: &str, public: Vec<u8>) -> wire::Presence {
        wire::Presence {
            v: Version::CURRENT,
            card: card_of(seed),
            handle: handle.to_owned(),
            public,
            attached: true,
            addr: Addr::from_endpoint_report(Vec::new(), Vec::new()),
            seq: LINE_CLOCK_MS,
            mac: ZERO_MAC,
            sig: unsigned(),
        }
    }

    fn hostile_text(index: usize) -> Vec<u8> {
        HOSTILE_BYTES
            .iter()
            .cycle()
            .skip(index)
            .step_by(HOSTILE_STRIDE)
            .take(index % HOSTILE_LENGTHS)
            .copied()
            .collect()
    }

    #[test]
    fn spec_3_13_a_peer_line_names_its_status_and_prints_a_dash_for_an_unknown_public() {
        let mut table = PeerTable::new(card_of(0));
        let sent = [(1, "amy", true), (2, "bob", false), (3, "cat", true)];
        for (seed, handle, attached) in sent {
            drop(table.receive_message(
                card_of(seed),
                &message_from(handle, attached),
                LINE_CLOCK_MS,
            ));
        }
        drop(table.direct_contact_failed(card_of(2), LINE_CLOCK_MS));
        table.receive_departure(
            &wire::Departure {
                v: Version::CURRENT,
                card: card_of(3),
                seq: LINE_CLOCK_MS,
                mac: ZERO_MAC,
                sig: unsigned(),
            },
            LINE_CLOCK_MS,
        );

        let lines: Vec<String> = table.peers(LINE_CLOCK_MS).map(peer_line).collect();

        assert_eq!(
            lines,
            [
                "amy\tonline\ttrue\t-\n",
                "bob\tunreachable\tfalse\t-\n",
                "cat\tgone\ttrue\t-\n"
            ]
        );
    }

    #[test]
    fn spec_3_13_a_departure_an_unreachable_report_and_an_infrastructure_record_print_their_cards()
    {
        let departure = Payload::Departure(wire::Departure {
            v: Version::CURRENT,
            card: card_of(1),
            seq: LINE_CLOCK_MS,
            mac: ZERO_MAC,
            sig: unsigned(),
        });
        let unreachable = Payload::Unreachable(wire::Unreachable {
            v: Version::CURRENT,
            card: card_of(2),
            reporter: card_of(3),
            ts: LINE_CLOCK_MS,
            mac: ZERO_MAC,
            sig: unsigned(),
        });
        let infrastructure = Payload::Infrastructure(wire::Infrastructure {
            v: Version::CURRENT,
            host: card_of(4),
            relay: wire::RelayUrl::try_from("https://relay.example".to_owned()).unwrap(),
            addr: Addr::from_endpoint_report(Vec::new(), Vec::new()),
            ts: LINE_CLOCK_MS,
            cert: unsigned(),
            mac: ZERO_MAC,
            sig: unsigned(),
        });

        assert_eq!(
            payload_line(&departure),
            format!("departure\t{}\n", "01".repeat(32))
        );
        assert_eq!(
            payload_line(&unreachable),
            format!("unreachable\t{}\t{}\n", "02".repeat(32), "03".repeat(32))
        );
        assert_eq!(
            payload_line(&infrastructure),
            format!(
                "infrastructure\t{}\thttps://relay.example\n",
                "04".repeat(32)
            )
        );
    }

    #[test]
    fn spec_3_13_no_field_of_a_presence_or_peer_line_breaks_its_columns_or_its_line() {
        for index in 0..PROPERTY_CASES {
            let hostile = hostile_text(index);
            let handle = String::from_utf8_lossy(&hostile).into_owned();
            let presence = presence_of(1, &handle, hostile.clone());
            let presence_line = payload_line(&Payload::Presence(presence.clone()));
            let mut table = PeerTable::new(card_of(0));
            drop(table.receive_presence(&presence_of(1, "amy", hostile), LINE_CLOCK_MS));
            let peer_lines: Vec<String> = table.peers(LINE_CLOCK_MS).map(peer_line).collect();

            assert_eq!(
                presence_line.matches('\t').count(),
                SENDER_PUBLIC_FIELD_COUNT - 1
            );
            assert_eq!(presence_line.matches('\n').count(), 1);
            assert!(presence_line.ends_with('\n'));
            assert_eq!(peer_lines.len(), 1);
            assert_eq!(peer_lines[0].matches('\t').count(), PEER_FIELD_COUNT - 1);
            assert_eq!(peer_lines[0].matches('\n').count(), 1);
            assert!(peer_lines[0].ends_with('\n'));
        }
    }
}
