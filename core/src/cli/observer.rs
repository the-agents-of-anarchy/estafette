use std::fmt;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, MutexGuard};

use clap::Subcommand;
use data_encoding::HEXLOWER;
use iroh::SecretKey;
use iroh::endpoint::BindError;
use iroh::protocol::Router;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinError;
use tokio::time::Instant;

use crate::card::{Card, CardInvalid, CardUnusable};
use crate::cli::debug::DebugArgs;
use crate::cli::lines::tab_separated_line;
use crate::cli::network::NetworkFilesystem;
use crate::config::{ConfigError, NetworkConfig};
use crate::crypto::GateKey;
use crate::dirs::{Directories, DirectoriesError};
use crate::errors::ErrorCode;
use crate::event::RunRecord;
use crate::handle::{AnnouncedHandle, Handle, LocalHandle};
use crate::inbox::socket::{InboxSocket, InboxSocketError};
use crate::keys::parse_public_key;
use crate::net::direct::{ClientTimeouts, DirectClient, DirectHost, MonotonicClock};
use crate::net::endpoint::NodeEndpoint;
use crate::net::gossip::{GossipError, GossipEvent, NetworkTopic};
use crate::node::{
    DEPARTURE_WAIT, Delivery, DirectAnswer, DirectRequest, DirectRequester, Identity,
    KnownHostAddress, Network, Node, NodeEffects, NodeError, PeerRows, RelayProbe, Session,
    acknowledgement_of, known_host_addresses, observer_wait_over_at, presence_request,
    replace_lookup_entry, seed_lookup_with_known_hosts, send_in_background, serve,
    sleep_until_deadline, wire_addr,
};
use crate::payload::{OpenedPayload, ZERO_MAC};
use crate::peers::{Effect, Peer, PeerTable};
use crate::sidecar::{HeldSidecar, Pause, SEED_BYTES, SidecarError, claim, create_card};
use crate::wire::{self, Introduce, IntroduceAck, Message, Nonce, Version};

const OBSERVER_PUBLIC: &[u8] = b"{}";
const OPERATOR_CARD: &str = "operator";
const DELIVERED_KIND: &str = "delivered";
const NO_PUBLIC_FIELDS: &str = "-";
const PRESENCE_KIND: &str = "presence";
const DEPARTURE_KIND: &str = "departure";
const UNREACHABLE_KIND: &str = "unreachable";
const INFRASTRUCTURE_KIND: &str = "infrastructure";
const PRESENCE_REQUEST_KIND: &str = "presence-request";
const OWNER_FIELD: &str = "owner";

type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;
type ProbeNonceSource = Arc<dyn Fn(&mut Nonce) + Send + Sync>;
type SeedSource = Arc<dyn Fn(&mut [u8; SEED_BYTES]) + Send + Sync>;
type EndpointBinding = Pin<Box<dyn Future<Output = Result<NodeEndpoint, BindError>> + Send>>;
type BindEndpoint = Box<
    dyn FnOnce(SecretKey, GateKey, Vec<iroh::RelayUrl>, MonotonicClock) -> EndpointBinding + Send,
>;
type StartInbox = Box<dyn FnOnce() -> Result<Arc<InboxSocket>, InboxSocketError> + Send>;
type Interrupted = Pin<Box<dyn Future<Output = ()> + Send>>;

/// What SIGINT does while a runtime runs an observer command or the doctor, each command naming its own.
#[derive(Debug, Clone, Copy)]
pub enum Interruption {
    /// The command listens for SIGINT and ends its own run when it arrives.
    EndsTheCommand,
    /// SIGINT keeps its default and ends the process, for a command that never waits for it.
    EndsTheProcess,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ObserverCommand {
    /// Join the network without a card, wait out the observer wait, and print each peer as handle, status, attached and public, refusing a network without a configuration file
    ShowPeers {
        /// The network whose members to list
        network: Handle,
        /// The cards directory, replacing the default one
        #[arg(long)]
        cards_dir: Option<PathBuf>,
        #[command(flatten)]
        debug: DebugArgs,
    },
    /// Join the network without a card and print each gossip payload as it arrives until interrupted, refusing a network without a configuration file
    Tail {
        /// The network whose gossip to print
        network: Handle,
        /// The cards directory, replacing the default one
        #[arg(long)]
        cards_dir: Option<PathBuf>,
        #[command(flatten)]
        debug: DebugArgs,
    },
    /// Send one message as the reserved operator card, creating the card when absent, and print the delivery, refusing a network without a configuration file or a card another process holds
    TestSend {
        /// The network to send on
        network: Handle,
        /// The handle of the member to send to
        handle: LocalHandle,
        /// The text of the message
        body: String,
        /// The cards directory, replacing the default one
        #[arg(long)]
        cards_dir: Option<PathBuf>,
        #[command(flatten)]
        debug: DebugArgs,
    },
}

impl ObserverCommand {
    /// How the command takes SIGINT: show peers and tail end their observation, test send leaves SIGINT to end the process.
    #[must_use]
    pub const fn interruption(&self) -> Interruption {
        match self {
            Self::ShowPeers { .. } | Self::Tail { .. } => Interruption::EndsTheCommand,
            Self::TestSend { .. } => Interruption::EndsTheProcess,
        }
    }

    /// The cards directory override the command was given.
    #[must_use]
    pub fn cards_dir(&self) -> Option<PathBuf> {
        match self {
            Self::ShowPeers { cards_dir, .. }
            | Self::Tail { cards_dir, .. }
            | Self::TestSend { cards_dir, .. } => cards_dir.clone(),
        }
    }

    /// The `--debug` and `--no-debug` pair the command was given.
    #[must_use]
    pub const fn debug(&self) -> &DebugArgs {
        match self {
            Self::ShowPeers { debug, .. }
            | Self::Tail { debug, .. }
            | Self::TestSend { debug, .. } => debug,
        }
    }
}

/// The side effects every observer command runs on, initialized by the composition root.
pub struct ObserverEffects {
    pub clock: Clock,
    pub monotonic: MonotonicClock,
    pub fill_seed: SeedSource,
    pub bind_endpoint: BindEndpoint,
    pub interrupted: Interrupted,
}

/// The side effects only a node that announces itself needs; `test-send` alone reads the nonce source, the run record and the inbox start, and the doctor reads the relay probe.
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
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Sidecar(SidecarError),
    CardInvalid {
        path: PathBuf,
        reason: CardInvalid,
    },
    CardUnusable {
        path: PathBuf,
        reason: CardUnusable,
    },
    Session(ErrorCode),
    NotDelivered(ErrorCode),
    TopicClosed,
    RouterShutdown(JoinError),
    Output(io::Error),
    PayloadRender {
        kind: &'static str,
        source: serde_json::Error,
    },
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
            Self::CardUnusable { path, reason } => {
                write!(formatter, "{} is not usable: {reason}", path.display())
            }
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
            Self::PayloadRender { kind, source } => {
                write!(formatter, "cannot print a {kind} payload: {source}")
            }
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
        ObserverCommand::ShowPeers { network, .. } => {
            let peers = observed_peers(directories, network, effects).await?;
            for peer in &peers {
                write_line(stdout, &peer_line(peer))?;
            }
            Ok(())
        }
        ObserverCommand::Tail { network, .. } => {
            let setup = load_network_setup(directories, network)?;
            start_observation(&setup, effects).await?.tail(stdout).await
        }
        ObserverCommand::TestSend {
            network,
            handle,
            body,
            ..
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

/// Joins the network as an observer without a card and returns the peers it holds once the observer wait is over.
async fn observed_peers(
    directories: &Directories,
    network: &Handle,
    effects: ObserverEffects,
) -> Result<Vec<Peer>, Failure> {
    let setup = load_network_setup(directories, network)?;
    start_observation(&setup, effects)
        .await?
        .collect_peers()
        .await
}

/// Joins the network as an observer without a card, waits out the observer wait and counts the peers it sees.
///
/// # Errors
///
/// Returns [`ObserverCommandError`] naming what the operator must change.
pub async fn count_visible_peers(
    directories: &Directories,
    network: &Handle,
    effects: ObserverEffects,
) -> Result<usize, ObserverCommandError> {
    observed_peers(directories, network, effects)
        .await
        .map(|peers| peers.len())
        .map_err(ObserverCommandError)
}

/// The reserved operator card as claimed: its held lock, its requested handle and its public mapping.
struct OperatorCard {
    name: Handle,
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
        Arc::clone(&monotonic),
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
        .claim(operator.name, node, operator.held)
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
    let usable =
        card.check_usable_on(network.as_str())
            .map_err(|reason| Failure::CardUnusable {
                path: card_file.clone(),
                reason,
            })?;
    let name = operator_name(&card_file)?;
    let held =
        claim(&sidecar_file, usable.public_key.as_bytes(), pause).map_err(Failure::Sidecar)?;
    Ok(OperatorCard {
        name,
        held,
        handle: usable.requested_handle,
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
    create_card(
        card_file,
        sidecar_file,
        &operator_name(card_file)?,
        fresh_seed,
    )
    .map_err(Failure::Sidecar)?;
    Ok(())
}

/// Returns the operator card's name as a handle, refusing it for the card file when it is no handle.
fn operator_name(card_file: &Path) -> Result<Handle, Failure> {
    OPERATOR_CARD
        .parse::<Handle>()
        .map_err(|source| Failure::CardUnusable {
            path: card_file.to_path_buf(),
            reason: CardUnusable::Handle(source),
        })
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
    tab_separated_line(&[DELIVERED_KIND, &delivery.id, &delivery.attached.to_string()])
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
        Arc::clone(&effects.monotonic),
    )
    .await
    .map_err(Failure::Bind)?;
    seed_lookup_with_known_hosts(&endpoint, &setup.hosts);
    let own_card = *secret_key.public().as_bytes();
    let (to_loop, from_loop) = mpsc::unbounded_channel();
    let host = Arc::new(ObserverHost {
        endpoint: endpoint.endpoint().clone(),
        rows: PeerRows::new(own_card, endpoint.held_connections()),
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
                endpoint.held_connections(),
            )),
            host,
            endpoint,
            router,
            topic,
            from_loop,
            monotonic: effects.monotonic,
            interrupted: effects.interrupted,
            wait_over: None,
            key: secret_key,
        }),
        Err(failure) => {
            drop(router.shutdown().await);
            Err(Failure::Topic(failure))
        }
    }
}

/// What reaches the observer loop from the direct server and from the requests it sent.
enum ObserverEvent {
    Effects(Vec<Effect>),
    Answered(DirectAnswer),
}

/// The direct protocol host of an observer: no card, no inbox, and an introduction without a handle.
struct ObserverHost {
    endpoint: iroh::Endpoint,
    rows: PeerRows,
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
        self.rows.table()
    }

    /// Hands an event to the observer loop, which is gone only once the run is.
    fn hand_to_loop(&self, event: ObserverEvent) {
        drop(self.to_loop.send(event));
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
        let now_ms = self.now_ms();
        let effects = self
            .rows
            .change(|table| table.receive_introduce(sender, &introduce, now_ms));
        self.hand_to_loop(ObserverEvent::Effects(effects));
        acknowledgement_of(self.introduction())
    }
}

impl DirectRequester for Arc<ObserverHost> {
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

    fn hand_answer(&self, answer: DirectAnswer) {
        self.hand_to_loop(ObserverEvent::Answered(answer));
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
    key: SecretKey,
}

/// What an observer loop wakes for.
enum Step {
    Topic(Option<Result<GossipEvent, GossipError>>),
    Loop(ObserverEvent),
    WaitOver,
    Interrupted,
}

impl Observation {
    /// Handles events until the observer wait is over, the topic closes or the run is interrupted, shuts the listeners down, then returns the peers.
    async fn collect_peers(mut self) -> Result<Vec<Peer>, Failure> {
        let outcome = self.collect_until_done().await;
        let shutdown = self.router.shutdown().await;
        let peers = outcome?;
        shutdown.map_err(Failure::RouterShutdown)?;
        Ok(peers)
    }

    /// Returns the peers the table holds once the loop ends at the observer wait, the closed topic or the interrupt.
    async fn collect_until_done(&mut self) -> Result<Vec<Peer>, Failure> {
        loop {
            match self.next_step().await {
                Step::Topic(None) | Step::WaitOver | Step::Interrupted => break,
                Step::Topic(Some(Err(failure))) => return Err(Failure::Topic(failure)),
                Step::Topic(Some(Ok(GossipEvent::Joined))) => {
                    self.start_observer_wait();
                    self.request_presence_at_join().await?;
                }
                Step::Topic(Some(Ok(GossipEvent::JoinTimedOut))) => self.start_observer_wait(),
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

    /// Prints each payload until the run is interrupted or the topic closes.
    async fn tail_until_done(&mut self, stdout: &mut impl Write) -> Result<(), Failure> {
        loop {
            match self.next_step().await {
                Step::Interrupted => return Ok(()),
                Step::WaitOver | Step::Topic(Some(Ok(GossipEvent::JoinTimedOut))) => {}
                Step::Topic(Some(Ok(GossipEvent::Joined))) => {
                    self.request_presence_at_join().await?;
                }
                Step::Topic(None) => return Err(Failure::TopicClosed),
                Step::Topic(Some(Err(failure))) => return Err(Failure::Topic(failure)),
                Step::Topic(Some(Ok(event))) => {
                    if let GossipEvent::Payload(payload) = &event {
                        write_line(stdout, &payload_line(payload)?)?;
                    }
                    self.take_gossip_event(event);
                }
                Step::Loop(event) => self.take_loop_event(event),
            }
        }
    }

    /// Asks the members to announce themselves, once, at the join the topic reports.
    fn request_presence_at_join(&self) -> impl Future<Output = Result<(), Failure>> + Send + '_ {
        request_presence(&self.topic, &self.key, self.host.now_ms())
    }

    /// Starts the observer wait at the first join or join timeout and keeps one already started.
    fn start_observer_wait(&mut self) {
        let joined_at = (self.monotonic)();
        self.wait_over
            .get_or_insert_with(|| observer_wait_over_at(joined_at));
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
                let effects = self.host.rows.take_neighbor_up(node, now_ms);
                self.perform(effects);
            }
            GossipEvent::NeighborDown(node) => {
                let effects = self.host.rows.take_neighbor_down(node, now_ms);
                self.perform(effects);
            }
            GossipEvent::Payload(payload) => self.take_payload(*payload, now_ms),
        }
    }

    /// Applies a verified payload to the peer table.
    fn take_payload(&self, payload: OpenedPayload, now_ms: u64) {
        match payload {
            OpenedPayload::Presence(presence) => {
                let effects = self.host.rows.take_presence(&presence, now_ms);
                self.perform(effects);
            }
            OpenedPayload::Departure(departure) => {
                let effects = self.host.rows.take_departure(&departure, now_ms);
                self.perform(effects);
            }
            OpenedPayload::Unreachable(report) => {
                self.host.rows.take_unreachable(&report, now_ms);
            }
            OpenedPayload::Infrastructure(_record) => {}
            OpenedPayload::PresenceRequest(_request) => {}
        }
    }

    /// Applies the effects the direct server handed over and the answer of each direct request this node sent.
    fn take_loop_event(&self, event: ObserverEvent) {
        match event {
            ObserverEvent::Effects(effects) => self.perform(effects),
            ObserverEvent::Answered(answer) => {
                let effects = self
                    .host
                    .rows
                    .take_direct_answer(answer, self.host.now_ms());
                self.perform(effects);
            }
        }
    }

    /// Performs the effects as a member does, but sends no Probe and no Unreachable report, since an observer sends Introduce alone and no payload (spec 3.13).
    fn perform(&self, effects: Vec<Effect>) {
        let mut requests = Vec::new();
        for effect in effects {
            match effect {
                Effect::ReplaceLookupEntry { card, addr } => {
                    replace_lookup_entry(&self.endpoint, &card, &addr);
                }
                Effect::DropHeldConnection(card) => {
                    requests.push(DirectRequest::DropHeldConnection(card));
                }
                Effect::SendIntroduce(card) => requests.push(DirectRequest::SendIntroduce(card)),
                Effect::SendProbe(_card) | Effect::BroadcastUnreachable(_card) => {}
            }
        }
        send_in_background(Arc::clone(&self.host), &self.client, requests);
    }
}

/// Broadcasts the one payload an observation sends, a Presence request signed by the observer's key, at the one join its topic reports.
async fn request_presence(
    topic: &NetworkTopic,
    key: &SecretKey,
    now_ms: u64,
) -> Result<(), Failure> {
    topic
        .broadcast(key, presence_request(*key.public().as_bytes(), now_ms))
        .await
        .map_err(Failure::Topic)
}

/// The result line of a peer: handle, status, attached and public fields.
fn peer_line(peer: &Peer) -> String {
    let public = peer
        .public
        .as_deref()
        .map_or_else(|| NO_PUBLIC_FIELDS.to_owned(), public_text);
    tab_separated_line(&[
        peer.effective_handle.as_str(),
        peer.status.wire_word(),
        &peer.attached.to_string(),
        &public,
    ])
}

/// The result line of a gossip payload: its kind, then every field of the payload in the order spec 3.7 declares them, a `seq` or an Unreachable `ts` in milliseconds and an Infrastructure `ts` in seconds.
fn payload_line(payload: &OpenedPayload) -> Result<String, Failure> {
    Ok(match payload {
        OpenedPayload::Presence(presence) => tab_separated_line(&[
            PRESENCE_KIND,
            &json_text(PRESENCE_KIND, &presence.v)?,
            &hex_text(&presence.card),
            &presence.handle,
            &public_text(&presence.public),
            &presence.attached.to_string(),
            &json_text(PRESENCE_KIND, &presence.addr)?,
            &presence.seq.to_string(),
            &hex_text(&presence.mac),
            &hex_text(&presence.sig.0),
        ]),
        OpenedPayload::Departure(departure) => tab_separated_line(&[
            DEPARTURE_KIND,
            &json_text(DEPARTURE_KIND, &departure.v)?,
            &hex_text(&departure.card),
            &departure.seq.to_string(),
            &hex_text(&departure.mac),
            &hex_text(&departure.sig.0),
        ]),
        OpenedPayload::Unreachable(report) => tab_separated_line(&[
            UNREACHABLE_KIND,
            &json_text(UNREACHABLE_KIND, &report.v)?,
            &hex_text(&report.card),
            &hex_text(&report.reporter),
            &report.ts.to_string(),
            &hex_text(&report.mac),
            &hex_text(&report.sig.0),
        ]),
        OpenedPayload::Infrastructure(valid) => {
            let record = valid.record();
            tab_separated_line(&[
                INFRASTRUCTURE_KIND,
                &json_text(INFRASTRUCTURE_KIND, &record.v)?,
                &hex_text(&record.host),
                record.relay.as_str(),
                &json_text(INFRASTRUCTURE_KIND, &record.addr)?,
                &record.ts.to_string(),
                &hex_text(&record.cert.0),
                &hex_text(&record.mac),
                &hex_text(&record.sig.0),
            ])
        }
        OpenedPayload::PresenceRequest(request) => tab_separated_line(&[
            PRESENCE_REQUEST_KIND,
            &json_text(PRESENCE_REQUEST_KIND, &request.v)?,
            &hex_text(&request.requester),
            &request.seq.to_string(),
            &hex_text(&request.mac),
            &hex_text(&request.sig.0),
        ]),
    })
}

/// Key, mac, certificate or signature bytes as lowercase hexadecimal.
fn hex_text(bytes: &[u8]) -> String {
    HEXLOWER.encode(bytes)
}

/// A wire field of a payload of that kind as compact JSON, every key and element in its declared order.
fn json_text(kind: &'static str, field: &impl serde::Serialize) -> Result<String, Failure> {
    serde_json::to_string(field).map_err(|source| Failure::PayloadRender { kind, source })
}

/// A public mapping as the text of its JSON bytes.
fn public_text(public: &[u8]) -> String {
    String::from_utf8_lossy(public).into_owned()
}

#[cfg(test)]
mod tests {
    use crate::peers::{ContactFailed, Status};
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Mutex, PoisonError};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use iroh::EndpointAddr;

    use super::*;
    use crate::config::{Host, HostOrigin, NetworkConfig};
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use crate::dirs::OperatingSystemFilesystem;
    use crate::event::Event;
    use crate::handle::AnnouncedHandle;
    use crate::inbox::socket::ProcessTable;
    use crate::keys::render_public_key;
    use crate::net::direct::DirectServer;
    use crate::node::{Identity, Network, Node, NodeEffects};
    use crate::payload::ZERO_SIGNATURE;
    use crate::stream_items::StreamExt;
    use crate::wire::{Addr, Payload};

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
    const THIRD_SEED: u8 = 63;
    const THIRD_HANDLE: &str = "cal";
    const THIRD_PUBLIC: &[u8] = br#"{"role":"third"}"#;
    const PRESENCE_REQUEST_LINE_START: &str = "presence-request\t1\t";
    const UNREACHABLE_RELAY: &str = "https://relay.estafette.invalid/";
    const PROBE_NONCE: Nonce = [7; 16];
    const OPERATOR_CARD: &str = "operator";
    const OPERATOR_BODY: &str = "a test message from the operator";
    const FRAME_WAIT: Duration = Duration::from_secs(2);
    const POLL_INTERVAL: Duration = Duration::from_millis(20);
    const ATTACH_WAIT: Duration = Duration::from_secs(2);
    const DETACH_WAIT: Duration = Duration::from_millis(300);
    const PAYLOAD_WAIT: Duration = Duration::from_secs(20);
    const SPEC_OBSERVER_WAIT: Duration = Duration::from_millis(3000);

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
        endpoint: iroh::Endpoint,
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
            crate::net::direct::tests::unrecorded_register(),
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
            self.start_members_reaching(plans, &[]).await
        }

        /// Starts the members, each reaching the others and the running members at their addresses.
        async fn start_members_reaching(
            &self,
            plans: Vec<MemberPlan>,
            running: &[EndpointAddr],
        ) -> Vec<Member> {
            let mut endpoints = Vec::new();
            for plan in &plans {
                endpoints.push(bind_endpoint(secret_key(plan.seed)).await);
            }
            let addresses: Vec<EndpointAddr> = endpoints
                .iter()
                .map(|endpoint| endpoint.endpoint().addr())
                .chain(running.iter().cloned())
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
                            |_failure| {},
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
                bind_endpoint: Box::new(move |key, gate_key, relays, monotonic| {
                    Box::pin(async move {
                        let endpoint = NodeEndpoint::bind(
                            key,
                            gate_key,
                            relays,
                            |nonce: &mut [u8; KEY_LEN]| *nonce = [OBSERVER_SEED; KEY_LEN],
                            |_ignored| {},
                            crate::net::direct::HeldConnections::new(
                                monotonic,
                                crate::net::direct::tests::dropping_transport(),
                            ),
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
                report: Arc::new(|_event: Event| {}),
                probe_relay: Arc::new(|_relay| Box::pin(async { true })),
                start_inbox: Box::new(move || {
                    InboxSocket::start(
                        &directories,
                        u32::from(OBSERVER_SEED),
                        &EveryProcessRuns,
                        |_failure| {},
                    )
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
        let lookup_endpoint = endpoint.endpoint().clone();
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
                report: Arc::new(|_event: Event| {}),
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
            endpoint: lookup_endpoint,
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
        let mut attached = member.inbox.attached();
        let mut stream = tokio::net::UnixStream::connect(&member.inbox_path)
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut stream, b"{\"since\": 0}\n")
            .await
            .unwrap();
        tokio::time::timeout(ATTACH_WAIT, async {
            while !attached.is_attached() {
                attached.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_elapsed| {
            panic!("the inbox has no client attached within {ATTACH_WAIT:?}")
        });
        stream
    }

    /// Closes the client and waits until the member's inbox has no client attached.
    async fn detach_client(member: &Member, client: tokio::net::UnixStream) {
        let mut attached = member.inbox.attached();
        drop(client);
        tokio::time::timeout(ATTACH_WAIT, async {
            while attached.is_attached() {
                attached.changed().await.unwrap();
            }
        })
        .await
        .unwrap_or_else(|_elapsed| {
            panic!("the inbox still has a client attached after {ATTACH_WAIT:?}")
        });
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
                cards_dir: None,
                debug: DebugArgs::default(),
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
    struct SharedOutput(Arc<tokio::sync::watch::Sender<Vec<u8>>>);

    impl SharedOutput {
        fn text(&self) -> String {
            String::from_utf8(self.0.borrow().clone()).unwrap()
        }

        /// Waits until the text written so far satisfies `wanted`.
        async fn holds(&self, wanted: impl Fn(&str) -> bool + Send + Sync) {
            drop(
                self.0
                    .subscribe()
                    .wait_for(|bytes| wanted(&String::from_utf8_lossy(bytes)))
                    .await,
            );
        }
    }

    impl Write for SharedOutput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .send_modify(|written| written.extend_from_slice(bytes));
            Ok(bytes.len())
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
                detach_client(member, attach_client(member).await).await;
                drop(
                    tokio::time::timeout(DETACH_WAIT, output.holds(|text| text.contains(wanted)))
                        .await,
                );
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
        let first_presence_line_start = format!(
            "presence\t1\t{}\t{FIRST_HANDLE}\t{}\ttrue\t",
            render_public_key(&secret_key(FIRST_SEED).public()),
            String::from_utf8_lossy(FIRST_PUBLIC)
        );
        let tail = ObserverCommand::Tail {
            network: NETWORK_NAME.parse().unwrap(),
            cards_dir: None,
            debug: DebugArgs::default(),
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
                toggle_attachment_until_output_has(
                    &members[0],
                    &output,
                    &first_presence_line_start,
                )
                .await;
                interrupt.send(()).unwrap_or_default();
            }
        );

        tail_result.unwrap();
        assert!(output.text().contains(&first_presence_line_start));
        drop(lab);
    }

    /// The card of the seed as a payload line prints it.
    fn card_text(seed: u8) -> String {
        render_public_key(&secret_key(seed).public())
    }

    /// The start of the line tail prints for a Presence of the seed's card under the handle.
    fn presence_line_start(seed: u8, handle: &str) -> String {
        format!("presence\t1\t{}\t{handle}\t", card_text(seed))
    }

    /// Waits until the tail has printed a line starting with each of the starts, requiring that within the payload wait.
    async fn printed_within_payload_wait(output: &SharedOutput, starts: &[String]) {
        tokio::time::timeout(
            PAYLOAD_WAIT,
            output.holds(|text| starts.iter().all(|start| text.contains(start.as_str()))),
        )
        .await
        .unwrap_or_else(|_elapsed| {
            panic!("tail printed no line starting {starts:?} within {PAYLOAD_WAIT:?}")
        });
    }

    #[tokio::test]
    async fn spec_3_13_tail_prints_one_presence_request_per_member_join_and_none_from_the_members_that_answer()
     {
        let lab = Lab::new("tail-joins").await;
        let members = lab.start_members(two_members()).await;
        lab.write_network_config(&[FIRST_SEED, SECOND_SEED]);
        let member_addresses: Vec<EndpointAddr> = members
            .iter()
            .map(|member| member.address.clone())
            .collect();
        let output = SharedOutput::default();
        let mut tail_output = output.clone();
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let tail = ObserverCommand::Tail {
            network: NETWORK_NAME.parse().unwrap(),
            cards_dir: None,
            debug: DebugArgs::default(),
        };

        let (tail_result, _third) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                &OperatingSystemFilesystem,
                &NoPause,
                lab.effects(
                    member_addresses.clone(),
                    Box::pin(async move { drop(interrupted.await) }),
                ),
                &mut tail_output,
            ),
            async {
                printed_within_payload_wait(
                    &output,
                    &[
                        presence_line_start(FIRST_SEED, FIRST_HANDLE),
                        presence_line_start(SECOND_SEED, SECOND_HANDLE),
                    ],
                )
                .await;
                let third = lab
                    .start_members_reaching(
                        vec![MemberPlan {
                            seed: THIRD_SEED,
                            handle: THIRD_HANDLE,
                            public: THIRD_PUBLIC,
                        }],
                        &member_addresses,
                    )
                    .await;
                printed_within_payload_wait(
                    &output,
                    &[presence_line_start(THIRD_SEED, THIRD_HANDLE)],
                )
                .await;
                interrupt.send(()).unwrap_or_default();
                third
            }
        );

        tail_result.unwrap();
        let requesters: Vec<String> = output
            .text()
            .lines()
            .filter_map(|line| line.strip_prefix(PRESENCE_REQUEST_LINE_START))
            .filter_map(|fields| fields.split('\t').next())
            .map(str::to_owned)
            .collect();
        assert_eq!(requesters, vec![card_text(THIRD_SEED)]);
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
            cards_dir: None,
            handle: SECOND_HANDLE.parse().unwrap(),
            body: OPERATOR_BODY.to_owned(),
            debug: DebugArgs::default(),
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

        let frame = first_inbox_frame(&members[1]).await;
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("delivered\t{}\tfalse\n", frame["id"].as_str().unwrap())
        );
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
            &OPERATOR_CARD.parse().unwrap(),
            [OPERATOR_SEED; SEED_BYTES],
        )
        .unwrap();
        sidecar_file
    }

    fn operator_test_send() -> ObserverCommand {
        ObserverCommand::TestSend {
            network: NETWORK_NAME.parse().unwrap(),
            cards_dir: None,
            handle: SECOND_HANDLE.parse().unwrap(),
            body: OPERATOR_BODY.to_owned(),
            debug: DebugArgs::default(),
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
    async fn spec_3_5_test_send_refuses_an_operator_card_that_names_another_network() {
        let lab = Lab::new("send-other-network").await;
        lab.write_network_config(&[]);
        register_operator_card(&lab);
        let card_file = lab
            .directories
            .card_file(NETWORK_NAME, OPERATOR_CARD)
            .unwrap();
        let mut text = fs::read_to_string(&card_file).unwrap();
        text.push_str("network: acme\n");
        fs::write(&card_file, text).unwrap();
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

        assert!(
            refusal.to_string().contains("names network acme"),
            "{refusal}"
        );
        assert_eq!(String::from_utf8(output).unwrap(), "");
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
        announced: Arc<Mutex<Addr>>,
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
                addr: self
                    .announced
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
                mac: ZERO_MAC,
            }
        }
    }

    /// The neighbor's direct server, keeping every connection it accepts for the test to watch.
    #[derive(Debug)]
    struct WatchedServer {
        server: DirectServer<RecordingNeighbor>,
        accepted: tokio::sync::watch::Sender<Vec<iroh::endpoint::Connection>>,
    }

    impl iroh::protocol::ProtocolHandler for WatchedServer {
        async fn accept(
            &self,
            connection: iroh::endpoint::Connection,
        ) -> Result<(), iroh::protocol::AcceptError> {
            self.accepted
                .send_modify(|accepted| accepted.push(connection.clone()));
            self.server.accept(connection).await
        }
    }

    struct SilentNeighbor {
        _router: iroh::protocol::Router,
        endpoint: iroh::Endpoint,
        topic: NetworkTopic,
        address: EndpointAddr,
        introduces: Arc<Mutex<Vec<Introduce>>>,
        accepted: tokio::sync::watch::Sender<Vec<iroh::endpoint::Connection>>,
        announced: Arc<Mutex<Addr>>,
    }

    async fn start_silent_neighbor() -> SilentNeighbor {
        start_silent_neighbor_joined_through(Vec::new()).await
    }

    async fn start_silent_neighbor_joined_through(bootstrap: Vec<EndpointAddr>) -> SilentNeighbor {
        let endpoint = bind_endpoint(secret_key(NEIGHBOR_SEED)).await;
        let introduces = Arc::new(Mutex::new(Vec::new()));
        let accepted = tokio::sync::watch::Sender::new(Vec::new());
        let announced = Arc::new(Mutex::new(Addr::from_endpoint_report(
            Vec::new(),
            Vec::new(),
        )));
        let neighbor = RecordingNeighbor {
            card: *secret_key(NEIGHBOR_SEED).public().as_bytes(),
            introduces: Arc::clone(&introduces),
            announced: Arc::clone(&announced),
        };
        let bootstrap_ids = bootstrap.iter().map(|address| address.id).collect();
        for address in bootstrap {
            endpoint.replace_address(address);
        }
        let gossip = iroh_gossip::Gossip::builder().spawn(endpoint.endpoint().clone());
        let router = iroh::protocol::Router::builder(endpoint.endpoint().clone())
            .accept(
                crate::net::endpoint::DIRECT_ALPN,
                WatchedServer {
                    server: DirectServer::new(network_keys(), neighbor, &endpoint),
                    accepted: accepted.clone(),
                },
            )
            .accept(iroh_gossip::ALPN, gossip.clone())
            .spawn();
        let topic = NetworkTopic::join(
            &gossip,
            network_keys(),
            secret_key(OWNER_SEED).public(),
            bootstrap_ids,
            Instant::now(),
        )
        .await
        .unwrap();
        SilentNeighbor {
            _router: router,
            endpoint: endpoint.endpoint().clone(),
            topic,
            address: endpoint.endpoint().addr(),
            introduces,
            accepted,
            announced,
        }
    }

    /// The first connection the neighbor accepted, failing by name when none arrives within the wait.
    async fn first_connection_accepted(
        neighbor: &SilentNeighbor,
        within: Duration,
    ) -> iroh::endpoint::Connection {
        let mut accepted = neighbor.accepted.subscribe();
        let accepted = tokio::time::timeout(
            within,
            accepted.wait_for(|connections| !connections.is_empty()),
        )
        .await
        .unwrap_or_else(|_elapsed| panic!("the neighbor accepted no connection within {within:?}"))
        .unwrap();
        accepted[0].clone()
    }

    /// Waits until the observer lists the neighbor, which it does once it has applied the neighbor's answer to its Introduce, failing with what it holds when the wait ends first.
    async fn neighbor_listed_by(observer: &ObserverHost, within: Duration) {
        let neighbor = *secret_key(NEIGHBOR_SEED).public().as_bytes();
        tokio::time::timeout(within, async {
            while !observer
                .table()
                .peers(wall_clock_ms())
                .any(|peer| peer.card == neighbor)
            {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .unwrap_or_else(|_elapsed| {
            let held: Vec<wire::PublicKey> = observer
                .table()
                .peers(wall_clock_ms())
                .map(|peer| peer.card)
                .collect();
            panic!("the observer did not list the neighbor within {within:?}; it holds {held:?}")
        });
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
            cards_dir: None,
            debug: DebugArgs::default(),
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

    async fn next_presence_request(
        topic: &mut NetworkTopic,
        within: Duration,
    ) -> Option<wire::PresenceRequest> {
        tokio::time::timeout(within, async {
            loop {
                if let Some(Ok(GossipEvent::Payload(payload))) = topic.next_event().await
                    && let Payload::PresenceRequest(request) = Payload::from(*payload)
                {
                    return request;
                }
            }
        })
        .await
        .ok()
    }

    #[tokio::test]
    async fn spec_3_15_test_15_show_peers_sends_one_presence_request_after_joining_and_answers_none()
     {
        let lab = Lab::new("presence-request").await;
        let mut neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let show_peers = ObserverCommand::ShowPeers {
            network: NETWORK_NAME.parse().unwrap(),
            cards_dir: None,
            debug: DebugArgs::default(),
        };
        let effects = lab.effects(vec![neighbor.address.clone()], never_interrupted());
        let mut output = Vec::new();

        let (outcome, (sent, sent_after_asked)) = tokio::join!(
            run(
                &show_peers,
                &lab.directories,
                &OperatingSystemFilesystem,
                &NoPause,
                effects,
                &mut output,
            ),
            async {
                let sent = next_presence_request(&mut neighbor.topic, SPEC_OBSERVER_WAIT).await;
                neighbor
                    .topic
                    .broadcast(
                        &secret_key(NEIGHBOR_SEED),
                        Payload::PresenceRequest(wire::PresenceRequest {
                            v: Version::CURRENT,
                            requester: *secret_key(NEIGHBOR_SEED).public().as_bytes(),
                            seq: wall_clock_ms(),
                            mac: ZERO_MAC,
                            sig: ZERO_SIGNATURE,
                        }),
                    )
                    .await
                    .unwrap();
                let sent_after_asked =
                    next_presence_request(&mut neighbor.topic, SPEC_OBSERVER_WAIT).await;
                (sent, sent_after_asked)
            }
        );

        outcome.unwrap();
        assert_eq!(
            sent.map(|request| request.requester),
            Some(*secret_key(OBSERVER_SEED).public().as_bytes())
        );
        assert_eq!(sent_after_asked, None);
        drop(lab);
    }

    /// The direct address the neighbor's presence announces.
    const NEIGHBOR_DIRECT_ADDRESS: &str = "203.0.113.7:4433";
    /// The seq of the neighbor's presence.
    const NEIGHBOR_PRESENCE_SEQ: u64 = 1_000_001;
    /// The seq of the neighbor's departure.
    const NEIGHBOR_DEPARTURE_SEQ: u64 = 1_000_002;
    /// The ts of the neighbor's unreachable report.
    const NEIGHBOR_REPORT_TS: u64 = 1_000_003;
    /// The ts of the neighbor's infrastructure record.
    const NEIGHBOR_RECORD_TS: u64 = 1_000_004;
    /// The seq of the neighbor's presence request.
    const NEIGHBOR_REQUEST_SEQ: u64 = 1_000_005;
    /// The card the neighbor reports unreachable.
    const REPORTED_CARD: wire::PublicKey = [8; 32];

    /// The address the neighbor's presence announces: one relay and one direct address.
    fn neighbor_address() -> Addr {
        Addr::from_endpoint_report(
            [UNREACHABLE_RELAY.to_owned()],
            [NEIGHBOR_DIRECT_ADDRESS.parse().unwrap()],
        )
    }

    /// One payload of each kind the neighbor signs, every field set to a value no other field holds.
    fn neighbor_payloads() -> [Payload; 5] {
        let card = *secret_key(NEIGHBOR_SEED).public().as_bytes();
        let certificate = crate::crypto::sign_host_certificate(
            &secret_key(OWNER_SEED),
            &secret_key(NEIGHBOR_SEED).public(),
        );
        [
            Payload::Presence(wire::Presence {
                v: Version::CURRENT,
                card,
                handle: NEIGHBOR_HANDLE.to_owned(),
                public: NEIGHBOR_PUBLIC.to_vec(),
                attached: true,
                addr: neighbor_address(),
                seq: NEIGHBOR_PRESENCE_SEQ,
                mac: ZERO_MAC,
                sig: ZERO_SIGNATURE,
            }),
            Payload::Departure(wire::Departure {
                v: Version::CURRENT,
                card,
                seq: NEIGHBOR_DEPARTURE_SEQ,
                mac: ZERO_MAC,
                sig: ZERO_SIGNATURE,
            }),
            Payload::Unreachable(wire::Unreachable {
                v: Version::CURRENT,
                card: REPORTED_CARD,
                reporter: card,
                ts: NEIGHBOR_REPORT_TS,
                mac: ZERO_MAC,
                sig: ZERO_SIGNATURE,
            }),
            Payload::Infrastructure(wire::Infrastructure {
                v: Version::CURRENT,
                host: card,
                relay: wire::RelayUrl::try_from(UNREACHABLE_RELAY.to_owned()).unwrap(),
                addr: Addr::from_endpoint_report([UNREACHABLE_RELAY.to_owned()], []),
                ts: NEIGHBOR_RECORD_TS,
                cert: wire::SignatureBytes(certificate.to_bytes()),
                mac: ZERO_MAC,
                sig: ZERO_SIGNATURE,
            }),
            Payload::PresenceRequest(wire::PresenceRequest {
                v: Version::CURRENT,
                requester: card,
                seq: NEIGHBOR_REQUEST_SEQ,
                mac: ZERO_MAC,
                sig: ZERO_SIGNATURE,
            }),
        ]
    }

    /// The mac and signature the payload carries once the neighbor seals it, as lowercase hexadecimal.
    fn mac_and_signature_once_sealed(payload: &Payload) -> (String, String) {
        let sealed = crate::payload::seal_payload(
            &network_keys(),
            &secret_key(NEIGHBOR_SEED),
            payload.clone(),
        )
        .unwrap();
        let (mac, sig) = match wire::decode::<Payload>(&sealed).unwrap() {
            Payload::Presence(presence) => (presence.mac, presence.sig),
            Payload::Departure(departure) => (departure.mac, departure.sig),
            Payload::Unreachable(report) => (report.mac, report.sig),
            Payload::Infrastructure(record) => (record.mac, record.sig),
            Payload::PresenceRequest(request) => (request.mac, request.sig),
        };
        (HEXLOWER.encode(&mac), HEXLOWER.encode(&sig.0))
    }

    /// The line `tail` must print for each of the neighbor's payloads, every field in its spec 3.7 order.
    fn whole_payload_lines() -> Vec<String> {
        let card = HEXLOWER.encode(secret_key(NEIGHBOR_SEED).public().as_bytes());
        let payloads = neighbor_payloads();
        let [presence, departure, report, record, request] =
            payloads.each_ref().map(mac_and_signature_once_sealed);
        let certificate = HEXLOWER.encode(
            &crate::crypto::sign_host_certificate(
                &secret_key(OWNER_SEED),
                &secret_key(NEIGHBOR_SEED).public(),
            )
            .to_bytes(),
        );
        vec![
            format!(
                "presence\t1\t{card}\t{NEIGHBOR_HANDLE}\t{}\ttrue\t{{\"relays\":[\"{UNREACHABLE_RELAY}\"],\"direct\":[\"{NEIGHBOR_DIRECT_ADDRESS}\"]}}\t{NEIGHBOR_PRESENCE_SEQ}\t{}\t{}\n",
                String::from_utf8_lossy(NEIGHBOR_PUBLIC),
                presence.0,
                presence.1
            ),
            format!(
                "departure\t1\t{card}\t{NEIGHBOR_DEPARTURE_SEQ}\t{}\t{}\n",
                departure.0, departure.1
            ),
            format!(
                "unreachable\t1\t{}\t{card}\t{NEIGHBOR_REPORT_TS}\t{}\t{}\n",
                HEXLOWER.encode(&REPORTED_CARD),
                report.0,
                report.1
            ),
            format!(
                "infrastructure\t1\t{card}\t{UNREACHABLE_RELAY}\t{{\"relays\":[\"{UNREACHABLE_RELAY}\"],\"direct\":[]}}\t{NEIGHBOR_RECORD_TS}\t{certificate}\t{}\t{}\n",
                record.0, record.1
            ),
            format!(
                "presence-request\t1\t{card}\t{NEIGHBOR_REQUEST_SEQ}\t{}\t{}\n",
                request.0, request.1
            ),
        ]
    }

    /// Spec 3.13, tail: each gossip payload prints whole, `addr` and `relay` included, as it arrives.
    #[tokio::test]
    async fn spec_3_13_tail_prints_every_field_of_each_gossip_payload_as_it_arrives() {
        let lab = Lab::new("tail-whole").await;
        let mut neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let output = SharedOutput::default();
        let mut tail_output = output.clone();
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let wanted_lines = whole_payload_lines();
        let tail = ObserverCommand::Tail {
            network: NETWORK_NAME.parse().unwrap(),
            cards_dir: None,
            debug: DebugArgs::default(),
        };

        let (tail_result, ()) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                &OperatingSystemFilesystem,
                &NoPause,
                lab.effects(
                    vec![neighbor.address.clone()],
                    Box::pin(async move { drop(interrupted.await) }),
                ),
                &mut tail_output,
            ),
            async {
                next_presence_request(&mut neighbor.topic, PAYLOAD_WAIT).await;
                for payload in neighbor_payloads() {
                    neighbor
                        .topic
                        .broadcast(&secret_key(NEIGHBOR_SEED), payload)
                        .await
                        .unwrap();
                }
                tokio::time::timeout(
                    PAYLOAD_WAIT,
                    output.holds(|text| wanted_lines.iter().all(|line| text.contains(line))),
                )
                .await
                .unwrap_or_else(|_elapsed| {
                    panic!("not every payload line was printed within {PAYLOAD_WAIT:?}")
                });
                interrupt.send(()).unwrap_or_default();
            }
        );

        tail_result.unwrap();
        assert_eq!(output.text(), wanted_lines.concat());
        drop(lab);
    }

    const CLOCK_STEP_PAST_ANY_SILENCE: Duration = Duration::from_secs(3600);
    const SUPERSEDED_CLOSE_WAIT: Duration = Duration::from_secs(5);
    const ACK_DROP_WOULD_SHOW_WITHIN: Duration = Duration::from_millis(500);
    const GONE_COLUMN: &str = "\tgone\t";
    const UNREACHABLE_COLUMN: &str = "\tunreachable\t";
    const SAME_ROWS_WAIT: Duration = Duration::from_secs(5);
    const SPEC_GONE_PROCESS_SILENCE: Duration = Duration::from_millis(1500);
    const HEARING_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

    /// A monotonic clock that moves an hour on at every reading, so every connection the observer holds reads as long silent whenever it is judged.
    fn clock_moving_past_any_silence_at_each_reading() -> MonotonicClock {
        let started = Instant::now();
        let readings = Arc::new(std::sync::atomic::AtomicU32::new(0));
        Arc::new(move || {
            started
                .checked_add(
                    CLOCK_STEP_PAST_ANY_SILENCE
                        .saturating_mul(readings.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
                )
                .unwrap()
        })
    }

    async fn answer_on(connection: &iroh::endpoint::Connection, request: wire::Request) -> Vec<u8> {
        let sealed = crate::payload::seal_request(&network_keys(), request).unwrap();
        let (mut send, mut receive) = connection.open_bi().await.unwrap();
        send.write_all(&sealed).await.unwrap();
        send.finish().unwrap();
        receive.read_to_end(wire::WIRE_LIMIT_BYTES).await.unwrap()
    }

    fn introduce_from_neighbor() -> wire::Request {
        wire::Request::Introduce(Introduce {
            v: Version::CURRENT,
            handle: Some(NEIGHBOR_HANDLE.to_owned()),
            public: NEIGHBOR_PUBLIC.to_vec(),
            attached: true,
            addr: Addr::from_endpoint_report(Vec::new(), Vec::new()),
            mac: ZERO_MAC,
        })
    }

    fn probe_from_neighbor() -> wire::Request {
        wire::Request::Probe(wire::Probe {
            v: Version::CURRENT,
            nonce: PROBE_NONCE,
            mac: ZERO_MAC,
        })
    }

    #[tokio::test]
    async fn spec_3_8_an_observer_judges_the_silence_of_its_accepted_connections_on_the_monotonic_clock_it_is_handed()
     {
        let lab = Lab::new("observer-clock").await;
        let mut neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let (mut effects, operator_effects) = lab.effects(
            vec![neighbor.address.clone()],
            Box::pin(async move { drop(interrupted.await) }),
        );
        effects.monotonic = clock_moving_past_any_silence_at_each_reading();
        let tail = ObserverCommand::Tail {
            network: NETWORK_NAME.parse().unwrap(),
            cards_dir: None,
            debug: DebugArgs::default(),
        };
        let mut tail_output = Vec::new();

        let (tail_result, earlier_closed) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                &OperatingSystemFilesystem,
                &NoPause,
                (effects, operator_effects),
                &mut tail_output,
            ),
            async {
                next_presence_request(&mut neighbor.topic, PAYLOAD_WAIT).await;
                let observer = secret_key(OBSERVER_SEED).public();
                let earlier = neighbor
                    .endpoint
                    .connect(observer, crate::net::endpoint::DIRECT_ALPN)
                    .await
                    .unwrap();
                answer_on(&earlier, probe_from_neighbor()).await;
                let introducing = neighbor
                    .endpoint
                    .connect(observer, crate::net::endpoint::DIRECT_ALPN)
                    .await
                    .unwrap();
                answer_on(&introducing, introduce_from_neighbor()).await;
                let closed = tokio::time::timeout(SUPERSEDED_CLOSE_WAIT, earlier.closed()).await;
                interrupt.send(()).unwrap_or_default();
                closed
            }
        );

        tail_result.unwrap();
        let Ok(iroh::endpoint::ConnectionError::ApplicationClosed(close)) = earlier_closed else {
            panic!("the observer kept the earlier connection: {earlier_closed:?}");
        };
        assert_eq!(
            close.error_code,
            iroh::endpoint::VarInt::from_u32(crate::net::direct::SUPERSEDED_CONNECTION_CLOSE_CODE)
        );
        drop(lab);
    }

    /// A Presence of the neighbor carrying the address, newer than any it sent before.
    fn neighbor_presence_announcing(addr: Addr) -> Payload {
        Payload::Presence(wire::Presence {
            v: Version::CURRENT,
            card: *secret_key(NEIGHBOR_SEED).public().as_bytes(),
            handle: NEIGHBOR_HANDLE.to_owned(),
            public: NEIGHBOR_PUBLIC.to_vec(),
            attached: true,
            addr,
            seq: wall_clock_ms(),
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        })
    }

    /// An Unreachable report the neighbor signs about itself, stamped now.
    fn neighbor_reporting_itself_unreachable() -> Payload {
        let neighbor = *secret_key(NEIGHBOR_SEED).public().as_bytes();
        Payload::Unreachable(wire::Unreachable {
            v: Version::CURRENT,
            card: neighbor,
            reporter: neighbor,
            ts: wall_clock_ms(),
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        })
    }

    /// The neighbor's Departure, its seq stamped now.
    fn neighbor_departing() -> Payload {
        Payload::Departure(wire::Departure {
            v: Version::CURRENT,
            card: *secret_key(NEIGHBOR_SEED).public().as_bytes(),
            seq: wall_clock_ms(),
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        })
    }

    /// A process under the neighbor's card that is gone the way a killed process is: the runtime it ran on is no longer driven, so its connection falls silent and no close is ever sent.
    struct GoneProcess {
        runtime: tokio::runtime::Runtime,
        endpoint: NodeEndpoint,
        connection: iroh::endpoint::Connection,
    }

    impl GoneProcess {
        /// Runs a second process under the neighbor's card on a runtime of its own, introduces it to the observer on a direct connection, then stops driving it.
        async fn introduced_to(observer: EndpointAddr) -> Self {
            tokio::task::spawn_blocking(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let (endpoint, connection) = runtime.block_on(async {
                    let endpoint = bind_endpoint(secret_key(NEIGHBOR_SEED)).await;
                    let observer_id = observer.id;
                    endpoint.replace_address(observer);
                    let connection = endpoint
                        .endpoint()
                        .connect(observer_id, crate::net::endpoint::DIRECT_ALPN)
                        .await
                        .unwrap();
                    answer_on(&connection, introduce_from_neighbor()).await;
                    (endpoint, connection)
                });
                Self {
                    runtime,
                    endpoint,
                    connection,
                }
            })
            .await
            .unwrap()
        }

        /// Drives the process again only to read how the observer ended its connection, none when it did not end within the wait.
        async fn connection_end_within(
            self,
            within: Duration,
        ) -> Option<iroh::endpoint::ConnectionError> {
            tokio::task::spawn_blocking(move || {
                let Self {
                    runtime,
                    endpoint,
                    connection,
                } = self;
                let ended = runtime.block_on(async {
                    tokio::time::timeout(within, connection.closed()).await.ok()
                });
                drop(runtime);
                drop((endpoint, connection));
                ended
            })
            .await
            .unwrap()
        }
    }

    /// Starts two members, whose start outlasts the observer wait, so the gone process falls silent past the close bound and the sample that judges it; fails by name when it has not.
    async fn let_the_gone_process_fall_silent_past_the_close_bound(
        lab: &Lab,
        gone_since: Instant,
    ) -> Vec<Member> {
        let members_outlasting_the_silence = lab.start_members(two_members()).await;
        let silent_for = gone_since.elapsed();
        assert!(
            silent_for >= SPEC_GONE_PROCESS_SILENCE.saturating_add(HEARING_SAMPLE_INTERVAL),
            "the gone process was silent only {silent_for:?}, under the {SPEC_GONE_PROCESS_SILENCE:?} close bound plus the {HEARING_SAMPLE_INTERVAL:?} sample that judges it"
        );
        members_outlasting_the_silence
    }

    /// Waits until the observer has taken a Presence of the neighbor sent at or after `sent_ms`, failing by name when it has not within the payload wait.
    async fn wait_until_the_observer_takes_the_neighbors_presence_sent_at(
        observer: &ObserverHost,
        sent_ms: u64,
    ) {
        let neighbor = *secret_key(NEIGHBOR_SEED).public().as_bytes();
        let taken = tokio::time::timeout(PAYLOAD_WAIT, async {
            while !observer
                .rows
                .table()
                .peers(wall_clock_ms())
                .any(|peer| peer.card == neighbor && peer.last_seen >= sent_ms)
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            taken.is_ok(),
            "the observer took no presence of the neighbor sent at {sent_ms} within {PAYLOAD_WAIT:?}"
        );
    }

    #[tokio::test]
    async fn spec_3_8_an_observer_closes_the_connection_of_a_restarted_members_earlier_process_at_its_next_presence()
     {
        let lab = Lab::new("observer-restart").await;
        let mut neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let (effects, _operator_effects) = lab.effects(
            vec![neighbor.address.clone()],
            Box::pin(async move { drop(interrupted.await) }),
        );
        let setup = load_network_setup(&lab.directories, &NETWORK_NAME.parse().unwrap()).unwrap();
        let observation = start_observation(&setup, effects).await.unwrap();
        let observer_host = Arc::clone(&observation.host);
        let observer_address = observation.endpoint.endpoint().addr();
        let mut tail_output = Vec::new();

        let (tail_result, (earlier_closed, live_direct_closed, neighbor_events)) =
            tokio::join!(observation.tail(&mut tail_output), async {
                assert!(
                    next_presence_request(&mut neighbor.topic, PAYLOAD_WAIT)
                        .await
                        .is_some(),
                    "the observer sent no presence request within {PAYLOAD_WAIT:?}"
                );
                let observer = secret_key(OBSERVER_SEED).public();
                let earlier = GoneProcess::introduced_to(observer_address).await;
                let gone_since = Instant::now();
                let live_direct = neighbor
                    .endpoint
                    .connect(observer, crate::net::endpoint::DIRECT_ALPN)
                    .await
                    .unwrap();
                answer_on(&live_direct, probe_from_neighbor()).await;
                let _members_started_while_the_gone_process_fell_silent =
                    let_the_gone_process_fall_silent_past_the_close_bound(&lab, gone_since).await;
                let presence_sent_ms = wall_clock_ms();
                neighbor
                    .topic
                    .broadcast(
                        &secret_key(NEIGHBOR_SEED),
                        neighbor_presence_announcing(neighbor_address()),
                    )
                    .await
                    .unwrap();
                wait_until_the_observer_takes_the_neighbors_presence_sent_at(
                    &observer_host,
                    presence_sent_ms,
                )
                .await;
                let closed = earlier.connection_end_within(SUPERSEDED_CLOSE_WAIT).await;
                let live_direct_closed = live_direct.close_reason();
                let mut neighbor_events = Vec::new();
                while let Ok(Some(Ok(event))) =
                    tokio::time::timeout(Duration::ZERO, neighbor.topic.next_event()).await
                {
                    neighbor_events.push(event);
                }
                interrupt.send(()).unwrap_or_default();
                (closed, live_direct_closed, neighbor_events)
            });

        tail_result.unwrap();
        let Some(iroh::endpoint::ConnectionError::ApplicationClosed(close)) = earlier_closed else {
            panic!("the observer kept the earlier process's connection: {earlier_closed:?}");
        };
        assert_eq!(
            close.error_code,
            iroh::endpoint::VarInt::from_u32(crate::net::direct::SUPERSEDED_CONNECTION_CLOSE_CODE)
        );
        assert_eq!(live_direct_closed, None);
        assert!(
            !neighbor_events
                .iter()
                .any(|event| matches!(event, GossipEvent::NeighborDown(node) if *node == secret_key(OBSERVER_SEED).public())),
            "the observer closed its gossip connection to the restarted neighbor: {neighbor_events:?}"
        );
        drop(lab);
    }

    #[tokio::test]
    async fn spec_3_8_an_observer_keeps_its_connection_to_a_member_past_the_introduce_ack_and_drops_it_at_the_members_next_presence()
     {
        let lab = Lab::new("observer-held").await;
        let neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let (effects, _operator_effects) = lab.effects(
            vec![neighbor.address.clone()],
            Box::pin(async move { drop(interrupted.await) }),
        );
        let setup = load_network_setup(&lab.directories, &NETWORK_NAME.parse().unwrap()).unwrap();
        let observation = start_observation(&setup, effects).await.unwrap();
        let observer_host = Arc::clone(&observation.host);
        let mut tail_output = Vec::new();

        let (tail_result, (closed_past_the_ack, closed_at_the_presence)) =
            tokio::join!(observation.tail(&mut tail_output), async {
                let introduced_on = first_connection_accepted(&neighbor, PAYLOAD_WAIT).await;
                neighbor_listed_by(&observer_host, PAYLOAD_WAIT).await;
                let closed_past_the_ack =
                    tokio::time::timeout(ACK_DROP_WOULD_SHOW_WITHIN, introduced_on.closed()).await;
                neighbor
                    .topic
                    .broadcast(
                        &secret_key(NEIGHBOR_SEED),
                        neighbor_presence_announcing(neighbor_address()),
                    )
                    .await
                    .unwrap();
                let closed_at_the_presence =
                    tokio::time::timeout(SUPERSEDED_CLOSE_WAIT, introduced_on.closed()).await;
                interrupt.send(()).unwrap_or_default();
                (closed_past_the_ack, closed_at_the_presence)
            });

        tail_result.unwrap();
        assert!(
            closed_past_the_ack.is_err(),
            "the observer closed the introduction's connection within {ACK_DROP_WOULD_SHOW_WITHIN:?} of the ack, before any Presence: {closed_past_the_ack:?}"
        );
        assert!(
            matches!(
                closed_at_the_presence,
                Ok(iroh::endpoint::ConnectionError::ApplicationClosed(_))
            ),
            "the observer did not close the introduction's connection within {SUPERSEDED_CLOSE_WAIT:?} of the member's next Presence: {closed_at_the_presence:?}"
        );
        drop(lab);
    }

    /// What a node holds for the neighbor: its entry as show-peers lists it, the address it carries, and the lookup entry the node dials it by.
    #[derive(Debug, PartialEq, Eq)]
    struct HeldNeighbor {
        line: Option<String>,
        addr: Option<Addr>,
        lookup: Option<EndpointAddr>,
    }

    /// What a node listing the peers and dialing through the endpoint holds for the neighbor now.
    async fn held_neighbor(peers: &[Peer], endpoint: &iroh::Endpoint) -> HeldNeighbor {
        let neighbor = secret_key(NEIGHBOR_SEED).public();
        let entry = peers.iter().find(|peer| peer.card == *neighbor.as_bytes());
        let lookup = match endpoint.address_lookup() {
            Ok(lookup) => match lookup.resolve(neighbor).next().await {
                Some(Ok(Ok(item))) => Some(item.into_endpoint_addr()),
                _ => None,
            },
            Err(_closed) => None,
        };
        HeldNeighbor {
            line: entry.map(peer_line),
            addr: entry.and_then(|peer| peer.addr.clone()),
            lookup,
        }
    }

    /// What the member and the observer hold for the neighbor once both hold what `settled` asks, failing with `awaited` and both holdings when the wait ends first.
    async fn held_by_member_and_observer(
        member: &Member,
        observer: &ObserverHost,
        awaited: &str,
        settled: impl Fn(&HeldNeighbor) -> bool,
    ) -> (HeldNeighbor, HeldNeighbor) {
        let started = Instant::now();
        loop {
            let by_member = held_neighbor(&member.node.peers(), &member.endpoint).await;
            let observer_peers: Vec<Peer> =
                observer.table().peers(wall_clock_ms()).cloned().collect();
            let by_observer = held_neighbor(&observer_peers, &observer.endpoint).await;
            if settled(&by_member) && settled(&by_observer) {
                return (by_member, by_observer);
            }
            assert!(
                started.elapsed() <= SAME_ROWS_WAIT,
                "the member and the observer did not both hold {awaited} within {SAME_ROWS_WAIT:?}: member {by_member:?}, observer {by_observer:?}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Waits until the member holds the neighbor and the observer holds both, so each introduction a neighbor report started is answered, failing with what each holds when the wait ends first.
    async fn introduced_to_member_and_joined_observer(member: &Member, observer: &ObserverHost) {
        let neighbor = *secret_key(NEIGHBOR_SEED).public().as_bytes();
        let started = Instant::now();
        loop {
            let member_holds_neighbor =
                member.node.peers().iter().any(|peer| peer.card == neighbor);
            let held_by_observer: Vec<wire::PublicKey> = observer
                .table()
                .peers(wall_clock_ms())
                .map(|peer| peer.card)
                .collect();
            let observer_holds_neighbor = held_by_observer.contains(&neighbor);
            let observer_holds_member = held_by_observer.contains(member.address.id.as_bytes());
            if member_holds_neighbor && observer_holds_neighbor && observer_holds_member {
                return;
            }
            assert!(
                started.elapsed() <= SAME_ROWS_WAIT,
                "the neighbor was not introduced to the member and the joined observer within {SAME_ROWS_WAIT:?}: member holds the neighbor {member_holds_neighbor}, observer holds the neighbor {observer_holds_neighbor}, observer holds the member {observer_holds_member}"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// The address the neighbor announces: every direct address its endpoint reports, and one more when it has moved.
    fn neighbor_announcing(neighbor: &SilentNeighbor, moved: bool) -> Addr {
        let extra: Option<std::net::SocketAddr> =
            moved.then(|| NEIGHBOR_DIRECT_ADDRESS.parse().unwrap());
        Addr::from_endpoint_report(
            Vec::new(),
            neighbor.endpoint.addr().ip_addrs().copied().chain(extra),
        )
    }

    #[tokio::test]
    async fn spec_3_8_an_observer_and_a_member_given_the_same_presences_unreachable_report_and_departure_hold_the_same_entry_and_lookup()
     {
        let lab = Lab::new("same-rows").await;
        let members = lab.start_members(two_members()).await;
        let member_addresses: Vec<EndpointAddr> = members
            .iter()
            .map(|member| member.address.clone())
            .collect();
        let neighbor = start_silent_neighbor_joined_through(member_addresses.clone()).await;
        lab.write_network_config(&[FIRST_SEED, SECOND_SEED, NEIGHBOR_SEED]);
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let (effects, _operator_effects) = lab.effects(
            member_addresses
                .into_iter()
                .chain([neighbor.address.clone()])
                .collect(),
            Box::pin(async move { drop(interrupted.await) }),
        );
        let setup = load_network_setup(&lab.directories, &NETWORK_NAME.parse().unwrap()).unwrap();
        let observation = start_observation(&setup, effects).await.unwrap();
        let observer_host = Arc::clone(&observation.host);
        let neighbor_key = secret_key(NEIGHBOR_SEED);
        let mut tail_output = Vec::new();

        let (tail_result, held) = tokio::join!(observation.tail(&mut tail_output), async {
            introduced_to_member_and_joined_observer(&members[0], &observer_host).await;
            let mut held = Vec::new();
            for moved in [false, true] {
                let announced = neighbor_announcing(&neighbor, moved);
                *neighbor.announced.lock().unwrap() = announced.clone();
                neighbor
                    .topic
                    .broadcast(
                        &neighbor_key,
                        neighbor_presence_announcing(announced.clone()),
                    )
                    .await
                    .unwrap();
                held.push(
                    held_by_member_and_observer(
                        &members[0],
                        &observer_host,
                        "the announced address",
                        |held| held.addr.as_ref() == Some(&announced),
                    )
                    .await,
                );
            }
            neighbor
                .topic
                .broadcast(&neighbor_key, neighbor_reporting_itself_unreachable())
                .await
                .unwrap();
            held.push(
                held_by_member_and_observer(
                    &members[0],
                    &observer_host,
                    "the neighbor listed unreachable",
                    |held| {
                        held.line
                            .as_deref()
                            .is_some_and(|line| line.contains(UNREACHABLE_COLUMN))
                    },
                )
                .await,
            );
            neighbor
                .topic
                .broadcast(&neighbor_key, neighbor_departing())
                .await
                .unwrap();
            held.push(
                held_by_member_and_observer(
                    &members[0],
                    &observer_host,
                    "the neighbor listed gone",
                    |held| {
                        held.line
                            .as_deref()
                            .is_some_and(|line| line.contains(GONE_COLUMN))
                    },
                )
                .await,
            );
            interrupt.send(()).unwrap_or_default();
            held
        });

        tail_result.unwrap();
        for (by_member, by_observer) in held {
            assert_eq!(by_member, by_observer);
        }
        drop(lab);
    }

    #[tokio::test]
    async fn spec_3_13_a_network_without_a_configuration_file_is_refused_naming_the_file() {
        let lab = Lab::new("no-network").await;
        let mut output = Vec::new();
        let show_peers = ObserverCommand::ShowPeers {
            network: NETWORK_NAME.parse().unwrap(),
            cards_dir: None,
            debug: DebugArgs::default(),
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
    const PRESENCE_FIELD_COUNT: usize = 10;
    const PEER_FIELD_COUNT: usize = 4;

    fn card_of(seed: u8) -> wire::PublicKey {
        [seed; 32]
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
            sig: ZERO_SIGNATURE,
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
        drop(table.direct_contact_failed(card_of(2), LINE_CLOCK_MS, ContactFailed::Send));
        table
            .admit_departure(
                &wire::Departure {
                    v: Version::CURRENT,
                    card: card_of(3),
                    seq: LINE_CLOCK_MS,
                    mac: ZERO_MAC,
                    sig: ZERO_SIGNATURE,
                },
                LINE_CLOCK_MS,
            )
            .value
            .unwrap_or_default();

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
    fn spec_3_13_no_field_of_a_presence_or_peer_line_breaks_its_columns_or_its_line() {
        for index in 0..PROPERTY_CASES {
            let hostile = hostile_text(index);
            let handle = String::from_utf8_lossy(&hostile).into_owned();
            let presence = presence_of(1, &handle, hostile.clone());
            let presence_line = payload_line(&OpenedPayload::Presence(presence.clone())).unwrap();
            let mut table = PeerTable::new(card_of(0));
            drop(
                table
                    .admit_presence(&presence_of(1, "amy", hostile), LINE_CLOCK_MS)
                    .value
                    .unwrap_or_default(),
            );
            let peer_lines: Vec<String> = table.peers(LINE_CLOCK_MS).map(peer_line).collect();

            assert_eq!(
                presence_line.matches('\t').count(),
                PRESENCE_FIELD_COUNT - 1
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
