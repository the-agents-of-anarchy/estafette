use std::fmt;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, MutexGuard};

use clap::Subcommand;
use data_encoding::HEXLOWER;
use iroh::SecretKey;
use iroh::endpoint::BindError;
use iroh::protocol::Router;
use iroh_gossip::Gossip;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinError;
use tokio::time::Instant;

use crate::card::CardUnusable;
use crate::cli::cards::{CardCommandError, create_card_with_fresh_key, judge_card_text};
use crate::cli::debug::DebugArgs;
use crate::cli::lines::tab_separated_line;
use crate::cli::network::NetworkFilesystem;
use crate::clock::{MonotonicClock, deadline_reached};
use crate::config::{ConfigError, NetworkConfig};
use crate::crypto::GateKey;
use crate::dirs::{CardsDirectoryOverride, Directories};
use crate::errors::ErrorCode;
use crate::event::{Event, RunRecord};
use crate::handle::{AnnouncedHandle, Handle, LocalHandle};
use crate::inbox::socket::{InboxSocket, InboxSocketError};
use crate::mcp::EndpointBinding;
use crate::net::direct::{ClientTimeouts, DirectClient, DirectHost, message_id_text};
use crate::net::endpoint::{BoundEndpoint, NodeEndpoint, SnapshotMoment};
use crate::net::gossip::{GossipError, GossipEvent, NetworkTopic};
use crate::node::{
    Clock, DEPARTURE_WAIT, Delivery, DirectAnswer, DirectRequest, DirectRequester, Identity,
    KnownHostAddress, Network, Node, NodeEffects, NodeError, PeerRows, ProbeNonceSource,
    RelayProbe, Session, broadcast_recorded, known_host_addresses, next_recorded_gossip_event,
    observer_wait_over_at, presence_request, record_counters, replace_lookup_entry,
    seed_lookup_with_known_hosts, send_in_background, serve, snapshot_due_after,
    taken_without_a_row_record, wire_addr,
};
use crate::payload::{DescribedPayload, OpenedPayload, ZERO_MAC};
use crate::peers::{Effect, Peer, PeerTable};
use crate::sidecar::{HeldSidecar, Pause, SEED_BYTES, SidecarError, claim};
use crate::wire::{self, Introduce, IntroduceAck, Message, Version};

/// The name the command line parses the show-peers command by and its trace writes.
pub const SHOW_PEERS_COMMAND: &str = "show-peers";
/// The name the command line parses the tail command by and its trace writes.
pub const TAIL_COMMAND: &str = "tail";
/// The name the command line parses the test-send command by and its trace writes.
pub const TEST_SEND_COMMAND: &str = "test-send";
const OBSERVER_PUBLIC: &[u8] = b"{}";
const OPERATOR_CARD: &str = "operator";
const DELIVERED_KIND: &str = "delivered";
const NO_PUBLIC_FIELDS: &str = "-";
const PRESENCE_KIND: &str = "presence";
const DEPARTURE_KIND: &str = "departure";
const UNREACHABLE_KIND: &str = "unreachable";
const INFRASTRUCTURE_KIND: &str = "infrastructure";
const PRESENCE_REQUEST_KIND: &str = "presence-request";

type SeedSource = Arc<dyn Fn(&mut [u8; SEED_BYTES]) + Send + Sync>;
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
    #[command(name = SHOW_PEERS_COMMAND)]
    ShowPeers {
        /// The network whose members to list
        network: Handle,
        #[command(flatten)]
        debug: DebugArgs,
    },
    /// Join the network without a card and print each gossip payload as it arrives until interrupted, refusing a network without a configuration file
    #[command(name = TAIL_COMMAND)]
    Tail {
        /// The network whose gossip to print
        network: Handle,
        #[command(flatten)]
        debug: DebugArgs,
    },
    /// Send one message as the reserved operator card, creating the card when absent, and print the delivery, refusing a network without a configuration file or a card another process holds
    #[command(name = TEST_SEND_COMMAND)]
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
    /// The name the command line parses this command by.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ShowPeers { .. } => SHOW_PEERS_COMMAND,
            Self::Tail { .. } => TAIL_COMMAND,
            Self::TestSend { .. } => TEST_SEND_COMMAND,
        }
    }

    /// How the command takes SIGINT: show peers and tail end their observation, test send leaves SIGINT to end the process.
    #[must_use]
    pub const fn interruption(&self) -> Interruption {
        match self {
            Self::ShowPeers { .. } | Self::Tail { .. } => Interruption::EndsTheCommand,
            Self::TestSend { .. } => Interruption::EndsTheProcess,
        }
    }

    /// The cards directory override of the command: not offered by the commands that read no card, the flag when `test-send` was given one.
    #[must_use]
    pub fn cards_directory_override(&self) -> CardsDirectoryOverride {
        match self {
            Self::ShowPeers { .. } | Self::Tail { .. } => CardsDirectoryOverride::NotOffered,
            Self::TestSend { cards_dir, .. } => CardsDirectoryOverride::Offered(cards_dir.clone()),
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
    pub report: RunRecord,
    pub interrupted: Interrupted,
}

/// The side effects only a node that announces itself needs; `test-send` alone reads the nonce source and the inbox start, and the doctor reads the relay probe.
pub struct OperatorEffects {
    pub fill_probe_nonce: ProbeNonceSource,
    pub probe_relay: RelayProbe,
    pub start_inbox: StartInbox,
}

/// Why an observer command failed, printed as its message alone.
#[derive(Debug)]
enum Failure {
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
    Card(CardCommandError),
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
            Self::Config(source) => write!(formatter, "{source}"),
            Self::Node(source) => write!(formatter, "{source}"),
            Self::Bind(source) => write!(formatter, "cannot bind an endpoint: {source}"),
            Self::Topic(source) => write!(formatter, "the gossip topic failed: {source}"),
            Self::Inbox(source) => write!(formatter, "{source}"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Sidecar(source) => write!(formatter, "{source}"),
            Self::Card(source) => write!(formatter, "{source}"),
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
        report,
        ..
    } = effects;
    let OperatorEffects {
        fill_probe_nonce,
        probe_relay,
        start_inbox,
    } = operator_effects;
    let card_key = operator.held.secret_key().clone();
    let endpoint = bind_for_network(bind_endpoint, &setup, card_key.clone(), &monotonic).await?;
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
            monotonic: Arc::clone(&monotonic),
            fill_probe_nonce,
            report: Arc::clone(&report),
            inbox,
            probe_relay,
        },
    )
    .await
    .map_err(Failure::Node)?;
    let session = Session::new(Arc::clone(&report), monotonic);
    let node = session
        .claim(operator.name, node, operator.held)
        .map_err(Failure::Session)?;
    let delivery = node.send(to.as_str(), body).await;
    release_card(&session).await;
    delivery.map_err(|undelivered| Failure::NotDelivered(undelivered.into()))
}

/// Claims the reserved operator card from the registry, creating it first when absent.
fn claim_operator_card(
    directories: &Directories,
    filesystem: &impl NetworkFilesystem,
    pause: &impl Pause,
    network: &Handle,
    fill_seed: &SeedSource,
) -> Result<OperatorCard, Failure> {
    let name = operator_name(&directories.cards_directory(network))?;
    let card_file = directories.card_file(network, &name);
    let sidecar_file = directories.card_sidecar_file(network, &name);
    let unreadable = |source: io::Error| Failure::Io {
        path: card_file.clone(),
        source,
    };
    if !filesystem.path_exists(&card_file).map_err(unreadable)? {
        create_card_with_fresh_key(directories, filesystem, network, &name, &name, |seed| {
            fill_seed(seed);
        })
        .map_err(Failure::Card)?;
    }
    let text = filesystem.read_to_string(&card_file).map_err(unreadable)?;
    let (card, usable) = judge_card_text(&text, &card_file, network).map_err(Failure::Card)?;
    let held =
        claim(&sidecar_file, usable.public_key.as_bytes(), pause).map_err(Failure::Sidecar)?;
    Ok(OperatorCard {
        name,
        held,
        handle: usable.requested_handle,
        public: card.public_json(),
    })
}

/// Returns the operator card's name as a handle, refusing it for the path when it is no handle.
fn operator_name(path: &Path) -> Result<Handle, Failure> {
    OPERATOR_CARD.parse::<Handle>().map_err(|source| {
        Failure::Card(CardCommandError::CardUnusable {
            path: path.to_path_buf(),
            reason: CardUnusable::Handle(source),
        })
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
    tab_separated_line(&[
        DELIVERED_KIND,
        &message_id_text(delivery.id),
        &delivery.attached.to_string(),
    ])
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
    hosts: Vec<KnownHostAddress>,
}

/// Reads and validates the configuration file of the network.
fn load_network_setup(
    directories: &Directories,
    network: &Handle,
) -> Result<NetworkSetup, Failure> {
    let path = directories.network_config_file(network);
    let config = NetworkConfig::load(&path).map_err(Failure::Config)?;
    let hosts = known_host_addresses(&config).map_err(Failure::Node)?;
    let network = Network::new(config, path).map_err(Failure::Node)?;
    Ok(NetworkSetup { network, hosts })
}

/// Binds an endpoint under the key for the network's gate and the relays of its hosts, then seeds its lookup with those hosts.
async fn bind_for_network(
    bind_endpoint: BindEndpoint,
    setup: &NetworkSetup,
    key: SecretKey,
    monotonic: &MonotonicClock,
) -> Result<NodeEndpoint, Failure> {
    let endpoint = bind_endpoint(
        key,
        *setup.network.keys().gate_key(),
        setup.hosts.iter().map(|host| host.relay.clone()).collect(),
        Arc::clone(monotonic),
    )
    .await
    .map_err(Failure::Bind)?;
    seed_lookup_with_known_hosts(&endpoint, &setup.hosts);
    Ok(endpoint)
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
    let endpoint = bind_for_network(
        effects.bind_endpoint,
        setup,
        secret_key.clone(),
        &effects.monotonic,
    )
    .await?;
    let own_card = wire::PublicKey::from(secret_key.public());
    let (to_loop, from_loop) = mpsc::unbounded_channel();
    let host = Arc::new(ObserverHost {
        endpoint: endpoint.endpoint().clone(),
        rows: PeerRows::new(
            own_card,
            endpoint.held_connections(),
            Arc::clone(&effects.report),
        ),
        clock: effects.clock,
        to_loop,
    });
    let (router, gossip) = serve(&endpoint, keys, &host);
    let topic = NetworkTopic::join(
        &gossip,
        keys.clone(),
        setup.network.owner(),
        setup.hosts.iter().map(|host| host.id).collect(),
        Arc::clone(&effects.monotonic),
    )
    .await;
    match topic {
        Ok(topic) => {
            let snapshot_due = snapshot_due_after(effects.monotonic.now());
            record_counters(
                SnapshotMoment::Start,
                endpoint.endpoint(),
                &gossip,
                &*effects.report,
            );
            Ok(Observation {
                client: Arc::new(DirectClient::new(
                    endpoint.endpoint().clone(),
                    keys.clone(),
                    ClientTimeouts::SPEC,
                    endpoint.held_connections(),
                )),
                host,
                endpoint,
                router,
                gossip,
                topic,
                from_loop,
                monotonic: effects.monotonic,
                report: effects.report,
                interrupted: effects.interrupted,
                wait_over: None,
                snapshot_due,
                key: secret_key,
            })
        }
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
    endpoint: BoundEndpoint,
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

    fn receive_message(&self, _sender: wire::PublicKey, _message: Message) -> bool {
        false
    }

    fn receive_introduce(&self, sender: wire::PublicKey, introduce: Introduce) -> IntroduceAck {
        let now_ms = self.now_ms();
        let effects = self
            .rows
            .change(|table| table.receive_introduce(sender, &introduce, now_ms));
        self.hand_to_loop(ObserverEvent::Effects(effects));
        IntroduceAck::from(self.introduction())
    }
}

impl DirectRequester for Arc<ObserverHost> {
    fn is_stopping(&self) -> bool {
        DirectHost::is_stopping(self)
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
    gossip: Gossip,
    from_loop: mpsc::UnboundedReceiver<ObserverEvent>,
    monotonic: MonotonicClock,
    report: RunRecord,
    interrupted: Interrupted,
    wait_over: Option<Instant>,
    snapshot_due: Instant,
    key: SecretKey,
}

/// What an observer loop wakes for.
enum Step {
    Topic(Option<Result<GossipEvent, GossipError>>),
    Loop(ObserverEvent),
    WaitOver,
    SnapshotDue,
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
                Step::SnapshotDue => self.record_counters(),
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
                Step::SnapshotDue => self.record_counters(),
            }
        }
    }

    /// Asks the members to announce themselves, once, at the join the topic reports.
    fn request_presence_at_join(&self) -> impl Future<Output = Result<(), Failure>> + Send + '_ {
        request_presence(&self.topic, &self.key, self.host.now_ms(), &*self.report)
    }

    /// Starts the observer wait at the first join or join timeout and keeps one already started.
    fn start_observer_wait(&mut self) {
        let joined_at = self.monotonic.now();
        self.wait_over
            .get_or_insert_with(|| observer_wait_over_at(joined_at));
    }

    /// Records the socket and gossip counters at an interval, so the events file is written at least that often while the observer runs.
    fn record_counters(&mut self) {
        self.snapshot_due = snapshot_due_after(self.monotonic.now());
        record_counters(
            SnapshotMoment::Interval,
            self.endpoint.endpoint(),
            &self.gossip,
            &*self.report,
        );
    }

    /// Waits for the next topic event, loop event, observer wait deadline, counters snapshot or interrupt.
    async fn next_step(&mut self) -> Step {
        tokio::select! {
            event = next_recorded_gossip_event(
                &mut self.topic,
                Some((&self.host.rows, &*self.host.clock)),
                &*self.report,
            ) => Step::Topic(event),
            Some(event) = self.from_loop.recv() => Step::Loop(event),
            () = deadline_reached(&*self.monotonic, self.wait_over) => Step::WaitOver,
            () = self.monotonic.reached(self.snapshot_due) => Step::SnapshotDue,
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

    /// Applies a verified payload to the peer table, recording a host record or a presence request as taken without a row, since an observer holds no host and answers no request.
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
            unapplied @ (OpenedPayload::Infrastructure(_) | OpenedPayload::PresenceRequest(_)) => {
                (self.report)(taken_without_a_row_record(
                    unapplied.facts(),
                    Some((&self.host.rows, now_ms)),
                ));
            }
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

/// Broadcasts the one payload an observation sends, a Presence request signed by the observer's key, at the one join its topic reports, recording it once sent.
async fn request_presence(
    topic: &NetworkTopic,
    key: &SecretKey,
    now_ms: u64,
    report: &(dyn Fn(Event) + Send + Sync),
) -> Result<(), Failure> {
    broadcast_recorded(
        topic,
        key,
        presence_request(wire::PublicKey::from(key.public()), now_ms),
        report,
    )
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
            &hex_text(presence.card.as_bytes()),
            &presence.handle,
            &public_text(&presence.public),
            &presence.attached.to_string(),
            &json_text(PRESENCE_KIND, &presence.addr)?,
            &presence.seq.to_string(),
            &hex_text(presence.mac.as_bytes()),
            &hex_text(&presence.sig.0),
        ]),
        OpenedPayload::Departure(departure) => tab_separated_line(&[
            DEPARTURE_KIND,
            &json_text(DEPARTURE_KIND, &departure.v)?,
            &hex_text(departure.card.as_bytes()),
            &departure.seq.to_string(),
            &hex_text(departure.mac.as_bytes()),
            &hex_text(&departure.sig.0),
        ]),
        OpenedPayload::Unreachable(report) => tab_separated_line(&[
            UNREACHABLE_KIND,
            &json_text(UNREACHABLE_KIND, &report.v)?,
            &hex_text(report.card.as_bytes()),
            &hex_text(report.reporter.as_bytes()),
            &report.ts.to_string(),
            &hex_text(report.mac.as_bytes()),
            &hex_text(&report.sig.0),
        ]),
        OpenedPayload::Infrastructure(valid) => {
            let record = valid.record();
            tab_separated_line(&[
                INFRASTRUCTURE_KIND,
                &json_text(INFRASTRUCTURE_KIND, &record.v)?,
                &hex_text(record.host.as_bytes()),
                record.relay.as_str(),
                &json_text(INFRASTRUCTURE_KIND, &record.addr)?,
                &record.ts.to_string(),
                &hex_text(&record.cert.0),
                &hex_text(record.mac.as_bytes()),
                &hex_text(&record.sig.0),
            ])
        }
        OpenedPayload::PresenceRequest(request) => tab_separated_line(&[
            PRESENCE_REQUEST_KIND,
            &json_text(PRESENCE_REQUEST_KIND, &request.v)?,
            &hex_text(request.requester.as_bytes()),
            &request.seq.to_string(),
            &hex_text(request.mac.as_bytes()),
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
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Mutex, PoisonError};
    use std::time::Duration;

    use iroh::EndpointAddr;

    use super::*;
    use crate::clock::{MonotonicTime, RuntimeClock, later_by};
    use crate::config::{Host, HostOrigin, NetworkConfig};
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use crate::dirs::OperatingSystemFilesystem;
    use crate::event::Event;
    use crate::handle::AnnouncedHandle;
    use crate::inbox::socket::ProcessTable;
    use crate::keys::render_public_key;
    use crate::net::direct::DirectServer;
    use crate::net::direct::tests::{
        INTERVAL_MOMENT, INTRODUCE_KIND, LeapingClock, PAYLOAD_ACCEPTED, REQUEST_SENT,
        SPEC_STATE_SNAPSHOT_INTERVAL, START_MOMENT, STATE_COUNTERS, StandingClock,
        counters_snapshot_one_interval_on, deadline_of_the_gossip_join_timeout, field,
        recorded_until_introduce_sent_to, was_taken_at,
    };
    use crate::node::{Identity, Network, Node, NodeEffects};
    use crate::payload::ZERO_SIGNATURE;
    use crate::peers::{FailedContactKind, Status};
    use crate::stream_items::StreamExt;
    use crate::test_waits::{
        RecordedEvent, answered_within, found_within, gossip_events_recorded_until,
        next_recorded_meeting, recorded_events, recorded_until, secret_key, short_id_field,
        wall_clock_ms,
    };
    use crate::wire::{Addr, Nonce, Payload};

    const NETWORK_NAME: &str = "lab";
    const NETWORK_SECRET: [u8; KEY_LEN] = [33; KEY_LEN];
    const OWNER_SEED: u8 = 60;

    fn operating_system_filesystem() -> &'static OperatingSystemFilesystem {
        static FILESYSTEM: std::sync::OnceLock<OperatingSystemFilesystem> =
            std::sync::OnceLock::new();
        FILESYSTEM.get_or_init(|| OperatingSystemFilesystem::of_this_process().unwrap())
    }
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
    const ATTACH_WAIT: Duration = Duration::from_secs(2);
    const PAYLOAD_WAIT: Duration = Duration::from_secs(20);
    const SPEC_OBSERVER_WAIT: Duration = Duration::from_millis(3000);
    const SENT_INTRODUCE_WITHIN: Duration = Duration::from_secs(10);

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
        endpoint: BoundEndpoint,
    }

    struct MemberPlan {
        seed: u8,
        handle: &'static str,
        public: &'static [u8],
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
            let root = crate::this_process_directory::fresh_directory_of_this_process_in(
                &std::env::temp_dir(),
                &format!("u27-{tag}"),
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
                operating_system_filesystem(),
                CardsDirectoryOverride::Offered(None),
            )
            .unwrap();
            Self {
                root,
                directories: Arc::new(directories),
                _slot: slot,
            }
        }

        fn write_network_config(&self, host_seeds: &[u8]) {
            let path = self
                .directories
                .network_config_file(&NETWORK_NAME.parse().unwrap());
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
                            Arc::new(RuntimeClock),
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
            self.effects_recording_to(
                member_addresses,
                interrupted,
                crate::net::direct::tests::dropping_transport(),
            )
        }

        /// The effects of an observer run that hands every event it records, its transport's included, to the record.
        fn effects_recording_every_event_to(
            &self,
            member_addresses: Vec<EndpointAddr>,
            interrupted: Interrupted,
            record: RunRecord,
        ) -> (ObserverEffects, OperatorEffects) {
            let (effects, operator_effects) = self.effects_recording_to(
                member_addresses,
                interrupted,
                crate::net::endpoint::TransportRecord::new(
                    Arc::clone(&record),
                    crate::net::direct::tests::test_salt(),
                ),
            );
            (
                ObserverEffects {
                    report: record,
                    ..effects
                },
                operator_effects,
            )
        }

        fn effects_recording_to(
            &self,
            member_addresses: Vec<EndpointAddr>,
            interrupted: Interrupted,
            transport: crate::net::endpoint::TransportRecord,
        ) -> (ObserverEffects, OperatorEffects) {
            let directories = Arc::clone(&self.directories);
            let effects = ObserverEffects {
                clock: Arc::new(wall_clock_ms),
                monotonic: Arc::new(RuntimeClock),
                fill_seed: Arc::new(|seed| *seed = [OBSERVER_SEED; SEED_BYTES]),
                bind_endpoint: Box::new(move |key, gate_key, relays, monotonic| {
                    Box::pin(async move {
                        let endpoint = NodeEndpoint::bind(
                            key,
                            gate_key,
                            relays,
                            |nonce: &mut [u8; KEY_LEN]| *nonce = [OBSERVER_SEED; KEY_LEN],
                            |_ignored| {},
                            crate::net::direct::HeldConnections::new(monotonic, transport),
                        )
                        .await?;
                        for address in member_addresses {
                            endpoint.replace_address(address);
                        }
                        Ok(endpoint)
                    })
                }),
                report: Arc::new(|_event: Event| {}),
                interrupted,
            };
            let operator_effects = OperatorEffects {
                fill_probe_nonce: Arc::new(|nonce: &mut Nonce| *nonce = PROBE_NONCE),
                probe_relay: Arc::new(|_relay| Box::pin(async { true })),
                start_inbox: Box::new(move || {
                    InboxSocket::start(
                        &directories,
                        u32::from(OBSERVER_SEED),
                        &EveryProcessRuns,
                        Arc::new(RuntimeClock),
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
            crate::scratch_removal::remove_when_the_test_ends(&self.root);
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
                monotonic: Arc::new(RuntimeClock),
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
                debug: DebugArgs::default(),
            },
            &lab.directories,
            operating_system_filesystem(),
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

    /// Attaches a client to the member once the tail has joined its topic, so the member announces itself attached to a tail that hears it, waits until the tail prints the wanted line, then detaches.
    async fn attached_once_the_tail_joins_until_output_has(
        member: &Member,
        tail_recorded: &mut tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
        output: &SharedOutput,
        wanted: &str,
    ) {
        next_recorded_meeting(tail_recorded, "gossip.topic_joined", |_| true, PAYLOAD_WAIT).await;
        let client = attach_client(member).await;
        printed_within_payload_wait(output, &[wanted.to_owned()]).await;
        detach_client(member, client).await;
    }

    #[tokio::test]
    async fn spec_3_13_tail_prints_each_presence_payload_as_it_arrives_until_interrupted() {
        let lab = Lab::new("tail").await;
        let members = lab.start_members(two_members()).await;
        lab.write_network_config(&[FIRST_SEED, SECOND_SEED]);
        let output = SharedOutput::default();
        let mut tail_output = output.clone();
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let (record, mut recorded) = recorded_events();
        let first_presence_line_start = format!(
            "{}{}\ttrue\t",
            presence_line_start(FIRST_SEED, FIRST_HANDLE),
            String::from_utf8_lossy(FIRST_PUBLIC)
        );
        let tail = ObserverCommand::Tail {
            network: NETWORK_NAME.parse().unwrap(),
            debug: DebugArgs::default(),
        };

        let (tail_result, ()) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                operating_system_filesystem(),
                &NoPause,
                lab.effects_recording_every_event_to(
                    members
                        .iter()
                        .map(|member| member.address.clone())
                        .collect(),
                    Box::pin(async move { drop(interrupted.await) }),
                    record,
                ),
                &mut tail_output,
            ),
            async {
                attached_once_the_tail_joins_until_output_has(
                    &members[0],
                    &mut recorded,
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
            debug: DebugArgs::default(),
        };

        let (tail_result, _third) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                operating_system_filesystem(),
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
            operating_system_filesystem(),
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
        let sidecar = lab.directories.card_sidecar_file(
            &NETWORK_NAME.parse().unwrap(),
            &OPERATOR_CARD.parse().unwrap(),
        );
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
        let card_file = lab.directories.card_file(
            &NETWORK_NAME.parse().unwrap(),
            &OPERATOR_CARD.parse().unwrap(),
        );
        let sidecar_file = lab.directories.card_sidecar_file(
            &NETWORK_NAME.parse().unwrap(),
            &OPERATOR_CARD.parse().unwrap(),
        );
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
            operating_system_filesystem(),
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
        let card_file = lab.directories.card_file(
            &NETWORK_NAME.parse().unwrap(),
            &OPERATOR_CARD.parse().unwrap(),
        );
        let mut text = fs::read_to_string(&card_file).unwrap();
        text.push_str("network: acme\n");
        fs::write(&card_file, text).unwrap();
        let mut output = Vec::new();

        let refusal = run(
            &operator_test_send(),
            &lab.directories,
            operating_system_filesystem(),
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
        let card_file = lab.directories.card_file(
            &NETWORK_NAME.parse().unwrap(),
            &OPERATOR_CARD.parse().unwrap(),
        );
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
            operating_system_filesystem(),
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

        fn receive_message(&self, _sender: wire::PublicKey, _message: Message) -> bool {
            false
        }

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
        endpoint: BoundEndpoint,
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
            card: wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public()),
            introduces: Arc::clone(&introduces),
            announced: Arc::clone(&announced),
        };
        let bootstrap_ids = bootstrap.iter().map(|address| address.id).collect();
        for address in bootstrap {
            endpoint.replace_address(address);
        }
        let gossip = endpoint.endpoint().spawn_gossip();
        let router = endpoint
            .endpoint()
            .router_builder()
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
            Arc::new(RuntimeClock),
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
        let neighbor = wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public());
        found_within(
            within,
            "listing of the neighbor by the observer",
            async || {
                observer
                    .table()
                    .peers(wall_clock_ms())
                    .any(|peer| peer.card == neighbor)
                    .then_some(())
            },
        )
        .await
        .unwrap_or_else(|missed| {
            let held: Vec<wire::PublicKey> = observer
                .table()
                .peers(wall_clock_ms())
                .map(|peer| peer.card)
                .collect();
            panic!("{missed}; it holds {held:?}")
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
            debug: DebugArgs::default(),
        };

        run(
            &show_peers,
            &lab.directories,
            operating_system_filesystem(),
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
    async fn observability_decision_9_show_peers_records_each_introduce_it_sends_to_a_member_by_kind_and_peer()
     {
        let lab = Lab::new("introduce-recorded").await;
        let neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let (record, mut recorded) = recorded_events();
        let show_peers = ObserverCommand::ShowPeers {
            network: NETWORK_NAME.parse().unwrap(),
            debug: DebugArgs::default(),
        };

        run(
            &show_peers,
            &lab.directories,
            operating_system_filesystem(),
            &NoPause,
            lab.effects_recording_to(
                vec![neighbor.address.clone()],
                never_interrupted(),
                crate::net::endpoint::TransportRecord::new(
                    record,
                    crate::net::direct::tests::test_salt(),
                ),
            ),
            &mut Vec::new(),
        )
        .await
        .unwrap();

        recorded_until_introduce_sent_to(
            &mut recorded,
            &neighbor_short_id(),
            |fields| {
                field(fields, OUTCOME_FIELD) == Some(&serde_json::Value::from(ACKNOWLEDGED_OUTCOME))
            },
            SENT_INTRODUCE_WITHIN,
        )
        .await;
        drop(lab);
    }

    #[must_use]
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

    /// The observer's presence request taken from the topic, failing by name when none comes within the payload wait.
    async fn observers_presence_request(topic: &mut NetworkTopic) -> wire::PresenceRequest {
        next_presence_request(topic, PAYLOAD_WAIT)
            .await
            .unwrap_or_else(|| {
                panic!("the observer sent no presence request within {PAYLOAD_WAIT:?}")
            })
    }

    /// The `kind` and `peer` fields of an event's fields.
    fn kind_and_peer_of(
        fields: &crate::event::EventFields,
    ) -> (Option<serde_json::Value>, Option<serde_json::Value>) {
        (
            field(fields, KIND_FIELD).cloned(),
            field(fields, PEER_FIELD).cloned(),
        )
    }

    /// The `kind` and `peer` fields of each recorded event of the name, in the order recorded.
    fn kinds_and_peers_recorded_as(
        recorded: &[RecordedEvent],
        name: &str,
    ) -> Vec<(Option<serde_json::Value>, Option<serde_json::Value>)> {
        recorded
            .iter()
            .filter(|(recorded_name, _)| *recorded_name == name)
            .map(|(_, fields)| kind_and_peer_of(fields))
            .collect()
    }

    #[tokio::test]
    async fn spec_3_15_test_15_show_peers_sends_one_presence_request_after_joining_and_answers_none()
     {
        let lab = Lab::new("presence-request").await;
        let mut neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let show_peers = ObserverCommand::ShowPeers {
            network: NETWORK_NAME.parse().unwrap(),
            debug: DebugArgs::default(),
        };
        let (record, mut recorded) = recorded_events();
        let effects = lab.effects_recording_every_event_to(
            vec![neighbor.address.clone()],
            never_interrupted(),
            record,
        );
        let mut output = Vec::new();

        let (outcome, sent) = tokio::join!(
            run(
                &show_peers,
                &lab.directories,
                operating_system_filesystem(),
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
                            requester: wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public()),
                            seq: wall_clock_ms(),
                            mac: ZERO_MAC,
                            sig: ZERO_SIGNATURE,
                        }),
                    )
                    .await
                    .unwrap();
                sent
            }
        );

        outcome.unwrap();
        let recorded_through_the_end = crate::net::direct::tests::recorded_so_far(&mut recorded);
        let presence_request = Some(serde_json::Value::from(PRESENCE_REQUEST_EVENT_KIND));
        assert_eq!(
            sent.map(|request| request.requester),
            Some(wire::PublicKey::from(secret_key(OBSERVER_SEED).public()))
        );
        assert!(
            kinds_and_peers_recorded_as(&recorded_through_the_end, PAYLOAD_ACCEPTED)
                .contains(&(presence_request.clone(), Some(neighbor_short_id()))),
            "show-peers ended before it took the neighbor's presence request: {recorded_through_the_end:?}"
        );
        assert_eq!(
            kinds_and_peers_recorded_as(&recorded_through_the_end, "gossip.payload_broadcast"),
            vec![(presence_request, None)],
            "show-peers broadcast other than one presence request by the time it ended"
        );
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
    const REPORTED_CARD: wire::PublicKey = wire::PublicKey::from_bytes([8; 32]);

    /// The address the neighbor's presence announces: one relay and one direct address.
    fn neighbor_address() -> Addr {
        Addr::from_endpoint_report(
            [UNREACHABLE_RELAY.to_owned()],
            [NEIGHBOR_DIRECT_ADDRESS.parse().unwrap()],
        )
    }

    /// One payload of each kind the neighbor signs, every field set to a value no other field holds.
    fn neighbor_payloads() -> [Payload; 5] {
        let card = wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public());
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
        (HEXLOWER.encode(mac.as_bytes()), HEXLOWER.encode(&sig.0))
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
                HEXLOWER.encode(REPORTED_CARD.as_bytes()),
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
            debug: DebugArgs::default(),
        };

        let (tail_result, ()) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                operating_system_filesystem(),
                &NoPause,
                lab.effects(
                    vec![neighbor.address.clone()],
                    Box::pin(async move { drop(interrupted.await) }),
                ),
                &mut tail_output,
            ),
            async {
                observers_presence_request(&mut neighbor.topic).await;
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

    /// The wall clock of the observer in the tests that read its gossip events, in milliseconds since the Unix epoch.
    const OBSERVER_CLOCK_MS: u64 = 1_800_000_000_000;
    const SPEC_ANNOUNCEMENT_MAX_AGE_MS: u64 = 300_000;
    /// Bytes no payload decodes from.
    const NOT_A_PAYLOAD: &[u8] = b"not a payload";

    /// The five-byte id of the neighbor's card as the events file writes it.
    fn neighbor_short_id() -> serde_json::Value {
        short_id_field(&secret_key(NEIGHBOR_SEED).public())
    }

    /// The fields of a payload the neighbor signed, named by its handle, with its stamp.
    fn neighbor_payload_fields(
        kind: &str,
        stamp: (&'static str, u64),
    ) -> crate::event::EventFields {
        vec![
            ("kind", serde_json::Value::from(kind)),
            ("peer", neighbor_short_id()),
            ("handle", serde_json::Value::from(NEIGHBOR_HANDLE)),
            (stamp.0, serde_json::Value::from(stamp.1)),
        ]
    }

    /// The neighbor's payloads the observer judges by its clock: a fresh Presence, a Departure past the announcement max age, its host record and a presence request.
    fn payloads_judged_at_the_observer_clock() -> [Payload; 4] {
        let [_, _, _, record, request] = neighbor_payloads();
        let card = wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public());
        [
            Payload::Presence(wire::Presence {
                v: Version::CURRENT,
                card,
                handle: NEIGHBOR_HANDLE.to_owned(),
                public: NEIGHBOR_PUBLIC.to_vec(),
                attached: true,
                addr: neighbor_address(),
                seq: OBSERVER_CLOCK_MS - 1,
                mac: ZERO_MAC,
                sig: ZERO_SIGNATURE,
            }),
            Payload::Departure(wire::Departure {
                v: Version::CURRENT,
                card,
                seq: OBSERVER_CLOCK_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS - 1,
                mac: ZERO_MAC,
                sig: ZERO_SIGNATURE,
            }),
            record,
            request,
        ]
    }

    #[tokio::test]
    async fn observability_decision_9_tail_records_its_join_its_request_its_neighbor_and_each_payload_taken_or_dropped_by_rule()
     {
        let lab = Lab::new("tail-recorded").await;
        let mut neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let (record, mut recorded) = recorded_events();
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let (mut effects, operator_effects) = lab.effects(
            vec![neighbor.address.clone()],
            Box::pin(async move { drop(interrupted.await) }),
        );
        effects.clock = Arc::new(|| OBSERVER_CLOCK_MS);
        effects.report = record;
        let tail = ObserverCommand::Tail {
            network: NETWORK_NAME.parse().unwrap(),
            debug: DebugArgs::default(),
        };
        let expected = vec![
            ("gossip.topic_joined", Vec::new()),
            (
                "gossip.payload_broadcast",
                vec![
                    ("kind", serde_json::Value::from("presence_request")),
                    ("seq", serde_json::Value::from(OBSERVER_CLOCK_MS)),
                ],
            ),
            ("gossip.neighbor_up", vec![("peer", neighbor_short_id())]),
            (
                PAYLOAD_ACCEPTED,
                neighbor_payload_fields("presence", ("seq", OBSERVER_CLOCK_MS - 1)),
            ),
            ("gossip.payload_dropped", {
                let mut fields = neighbor_payload_fields(
                    "departure",
                    ("seq", OBSERVER_CLOCK_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS - 1),
                );
                fields.push(("rule", serde_json::Value::from("older_than_max_age")));
                fields
            }),
            (
                PAYLOAD_ACCEPTED,
                neighbor_payload_fields("infrastructure", ("ts", NEIGHBOR_RECORD_TS)),
            ),
            (
                PAYLOAD_ACCEPTED,
                neighbor_payload_fields("presence_request", ("seq", NEIGHBOR_REQUEST_SEQ)),
            ),
            (
                "gossip.payload_dropped",
                vec![("rule", serde_json::Value::from("malformed"))],
            ),
        ];
        let mut printed = Vec::new();

        let (tail_result, gossip) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                operating_system_filesystem(),
                &NoPause,
                (effects, operator_effects),
                &mut printed,
            ),
            async {
                observers_presence_request(&mut neighbor.topic).await;
                for payload in payloads_judged_at_the_observer_clock() {
                    neighbor
                        .topic
                        .broadcast(&secret_key(NEIGHBOR_SEED), payload)
                        .await
                        .unwrap();
                }
                neighbor
                    .topic
                    .broadcast_bytes(NOT_A_PAYLOAD.to_vec())
                    .await
                    .unwrap();
                let gossip = gossip_events_recorded_until(
                    &mut recorded,
                    |gathered| gathered.len() >= expected.len(),
                    PAYLOAD_WAIT,
                )
                .await;
                interrupt.send(()).unwrap_or_default();
                gossip
            }
        );

        tail_result.unwrap();
        assert_eq!(gossip, expected);
        drop(lab);
    }

    #[tokio::test(start_paused = true)]
    async fn observability_decisions_9_and_13_tail_records_its_socket_and_gossip_counters_at_its_start_then_every_60_s_while_it_runs()
     {
        let lab = Lab::new("tail-counters").await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let (record, mut recorded) = recorded_events();
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let (mut effects, operator_effects) =
            lab.effects(Vec::new(), Box::pin(async move { drop(interrupted.await) }));
        effects.report = record;
        let tail = ObserverCommand::Tail {
            network: NETWORK_NAME.parse().unwrap(),
            debug: DebugArgs::default(),
        };
        let mut printed = Vec::new();

        let (tail_result, (at_start, first, second)) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                operating_system_filesystem(),
                &NoPause,
                (effects, operator_effects),
                &mut printed,
            ),
            async {
                let at_start =
                    crate::net::direct::tests::next_counters_snapshot(&mut recorded).await;
                let first = crate::net::direct::tests::next_counters_snapshot(&mut recorded).await;
                let second = crate::net::direct::tests::next_counters_snapshot(&mut recorded).await;
                interrupt.send(()).unwrap_or_default();
                (at_start, first, second)
            }
        );

        tail_result.unwrap();
        assert_eq!(
            second.0.duration_since(first.0),
            SPEC_STATE_SNAPSHOT_INTERVAL
        );
        for ((_at, fields), moment) in [
            (&at_start, START_MOMENT),
            (&first, INTERVAL_MOMENT),
            (&second, INTERVAL_MOMENT),
        ] {
            assert!(was_taken_at(fields, moment), "{fields:?}");
            let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
            assert!(names.contains(&"socket.recv_datagrams"), "{names:?}");
            assert!(names.contains(&"gossip.neighbor_up"), "{names:?}");
        }
        drop(lab);
    }

    /// The longest an observer takes to act once its handed clock reads a deadline, which the runtime's timer would never reach on a clock an hour ahead.
    const ONCE_THE_CLOCK_READS_IT_WITHIN: Duration = Duration::from_secs(5);
    const SPEC_JOIN_TIMEOUT: Duration = Duration::from_millis(5000);

    #[tokio::test]
    async fn observability_decisions_9_and_13_tail_records_its_counters_once_its_handed_clock_reads_60_s_past_its_start_and_again_60_s_past_that_reading()
     {
        let lab = Lab::new("tail-counters-on-the-clock").await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let clock = StandingClock::standing_an_hour_ahead();
        let (record, mut recorded) = recorded_events();
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let (mut effects, operator_effects) =
            lab.effects(Vec::new(), Box::pin(async move { drop(interrupted.await) }));
        effects.report = record;
        effects.monotonic = clock.monotonic();
        let tail = ObserverCommand::Tail {
            network: NETWORK_NAME.parse().unwrap(),
            debug: DebugArgs::default(),
        };
        let mut printed = Vec::new();

        let (tail_result, (before_the_first, at_the_intervals)) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                operating_system_filesystem(),
                &NoPause,
                (effects, operator_effects),
                &mut printed,
            ),
            async {
                next_recorded_meeting(
                    &mut recorded,
                    STATE_COUNTERS,
                    |_fields| true,
                    ONCE_THE_CLOCK_READS_IT_WITHIN,
                )
                .await;
                let mut asked = deadline_of_the_gossip_join_timeout(&clock);
                let (before_the_first, at_the_first_interval) = counters_snapshot_one_interval_on(
                    &clock,
                    &mut recorded,
                    "tail's first interval counters",
                    &mut asked,
                    |_fields| true,
                )
                .await;
                let (_, at_the_second_interval) = counters_snapshot_one_interval_on(
                    &clock,
                    &mut recorded,
                    "tail's second interval counters",
                    &mut asked,
                    |_fields| true,
                )
                .await;
                interrupt.send(()).unwrap_or_default();
                (
                    before_the_first,
                    [at_the_first_interval, at_the_second_interval],
                )
            }
        );

        tail_result.unwrap();
        let counters_before_the_first: Vec<&RecordedEvent> = before_the_first
            .iter()
            .filter(|(name, _fields)| *name == STATE_COUNTERS)
            .collect();
        assert!(
            counters_before_the_first.is_empty(),
            "counters recorded before the clock read the first interval due: {counters_before_the_first:?}"
        );
        for at_the_interval in at_the_intervals {
            assert!(
                was_taken_at(&at_the_interval, INTERVAL_MOMENT),
                "{at_the_interval:?}"
            );
        }
        drop(lab);
    }

    #[tokio::test]
    async fn spec_3_8_show_peers_ends_once_its_handed_clock_reads_the_observer_wait_past_its_join_timeout()
     {
        let lab = Lab::new("show-peers-on-the-clock").await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let clock = StandingClock::standing_an_hour_ahead();
        let started = clock.now();
        let join_timed_out = later_by(started, SPEC_JOIN_TIMEOUT);
        let observer_wait_over = later_by(join_timed_out, SPEC_OBSERVER_WAIT);
        let (record, mut recorded) = recorded_events();
        let (mut effects, operator_effects) = lab.effects(Vec::new(), never_interrupted());
        effects.report = record;
        effects.monotonic = clock.monotonic();
        let show_peers = ObserverCommand::ShowPeers {
            network: NETWORK_NAME.parse().unwrap(),
            debug: DebugArgs::default(),
        };
        let mut printed = Vec::new();

        let (shown, ()) = tokio::join!(
            answered_within(
                ONCE_THE_CLOCK_READS_IT_WITHIN,
                "end of show-peers once the clock read the observer wait past its join timeout",
                run(
                    &show_peers,
                    &lab.directories,
                    operating_system_filesystem(),
                    &NoPause,
                    (effects, operator_effects),
                    &mut printed,
                ),
            ),
            async {
                next_recorded_meeting(
                    &mut recorded,
                    STATE_COUNTERS,
                    |_fields| true,
                    ONCE_THE_CLOCK_READS_IT_WITHIN,
                )
                .await;
                clock.move_forward(SPEC_JOIN_TIMEOUT);
                clock
                    .asked_exactly(
                        "show-peers' observer wait",
                        &BTreeSet::from([
                            join_timed_out,
                            observer_wait_over,
                            later_by(started, SPEC_STATE_SNAPSHOT_INTERVAL),
                        ]),
                    )
                    .await;
                clock.move_forward(SPEC_OBSERVER_WAIT);
            }
        );

        shown.unwrap();
        drop(lab);
    }

    const CLOCK_STEP_PAST_ANY_SILENCE: Duration = Duration::from_secs(3600);
    const SUPERSEDED_CLOSE_WAIT: Duration = Duration::from_secs(5);
    /// How far past every `IntroduceAck` the observer's clock is moved before the member's next Presence, the span within which a drop at an ack would show.
    const ACK_DROP_WOULD_SHOW_WITHIN: Duration = Duration::from_millis(500);
    const CONNECTION_OPENED_EVENT: &str = "transport.connection_opened";
    const CONNECTION_CLOSED_EVENT: &str = "transport.connection_closed";
    const CONNECTION_FIELD: &str = "connection";
    const PEER_FIELD: &str = "peer";
    const PROTOCOL_FIELD: &str = "protocol";
    const KIND_FIELD: &str = "kind";
    const SEQ_FIELD: &str = "seq";
    const CONNECTION_SOURCE_FIELD: &str = "connection_source";
    const DIRECT_PROTOCOL: &str = "direct";
    const HELD_CONNECTION_SOURCE: &str = "held";
    const PRESENCE_EVENT_KIND: &str = "presence";
    const PRESENCE_REQUEST_EVENT_KIND: &str = "presence_request";
    const OUTCOME_FIELD: &str = "outcome";
    const ACKNOWLEDGED_OUTCOME: &str = "acknowledged";
    /// The seq of the neighbor's presence request the observer takes once it has the answers of both its Introduces, before its clock moves.
    const REQUEST_SEQ_BEFORE_THE_MOVE: u64 = 1_000_006;
    /// The seq of the neighbor's presence request the observer takes once its clock has moved past every ack.
    const REQUEST_SEQ_AFTER_THE_MOVE: u64 = 1_000_007;
    const ONLINE_COLUMN: &str = "\tonline\t";
    const GONE_COLUMN: &str = "\tgone\t";

    const UNREACHABLE_COLUMN: &str = "\tunreachable\t";
    const SAME_ROWS_WAIT: Duration = Duration::from_secs(5);
    const SPEC_GONE_PROCESS_SILENCE: Duration = Duration::from_millis(1500);
    const HEARING_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

    /// A monotonic clock that moves an hour on at every reading, so every connection the observer holds reads as long silent whenever it is judged.
    fn clock_moving_past_any_silence_at_each_reading() -> MonotonicClock {
        LeapingClock::leaping_by(CLOCK_STEP_PAST_ANY_SILENCE).monotonic()
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
            debug: DebugArgs::default(),
        };
        let mut tail_output = Vec::new();

        let (tail_result, earlier_closed) = tokio::join!(
            run(
                &tail,
                &lab.directories,
                operating_system_filesystem(),
                &NoPause,
                (effects, operator_effects),
                &mut tail_output,
            ),
            async {
                observers_presence_request(&mut neighbor.topic).await;
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
            card: wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public()),
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
        let neighbor = wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public());
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
            card: wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public()),
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
        #[must_use]
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

    /// Waits until the observer has taken a Presence of the neighbor sent at or after `sent_ms`, failing by name when it has not within the payload wait.
    async fn wait_until_the_observer_takes_the_neighbors_presence_sent_at(
        observer: &ObserverHost,
        sent_ms: u64,
    ) {
        let neighbor = wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public());
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

    /// Has the neighbor broadcast a fresh Presence and waits until the observer has taken it.
    async fn neighbor_announces_itself_and_the_observer_takes_it(
        neighbor: &SilentNeighbor,
        observer: &ObserverHost,
    ) {
        let presence_sent_ms = wall_clock_ms();
        neighbor
            .topic
            .broadcast(
                &secret_key(NEIGHBOR_SEED),
                neighbor_presence_announcing(neighbor_address()),
            )
            .await
            .unwrap();
        wait_until_the_observer_takes_the_neighbors_presence_sent_at(observer, presence_sent_ms)
            .await;
    }

    #[tokio::test]
    async fn spec_3_8_an_observer_closes_the_connection_of_a_restarted_members_earlier_process_at_its_next_presence()
     {
        let lab = Lab::new("observer-restart").await;
        let mut neighbor = start_silent_neighbor().await;
        lab.write_network_config(&[NEIGHBOR_SEED]);
        let (interrupt, interrupted) = tokio::sync::oneshot::channel::<()>();
        let (mut effects, _operator_effects) = lab.effects(
            vec![neighbor.address.clone()],
            Box::pin(async move { drop(interrupted.await) }),
        );
        let observer_clock = StandingClock::standing_an_hour_ahead();
        effects.monotonic = observer_clock.monotonic();
        let setup = load_network_setup(&lab.directories, &NETWORK_NAME.parse().unwrap()).unwrap();
        let observation = start_observation(&setup, effects).await.unwrap();
        let observer_host = Arc::clone(&observation.host);
        let observer_address = observation.endpoint.endpoint().addr();
        let mut tail_output = Vec::new();

        let (tail_result, (earlier_closed, live_direct_closed, neighbor_events)) =
            tokio::join!(observation.tail(&mut tail_output), async {
                observers_presence_request(&mut neighbor.topic).await;
                let observer = secret_key(OBSERVER_SEED).public();
                let earlier = GoneProcess::introduced_to(observer_address).await;
                let live_direct = neighbor
                    .endpoint
                    .connect(observer, crate::net::endpoint::DIRECT_ALPN)
                    .await
                    .unwrap();
                answer_on(&live_direct, probe_from_neighbor()).await;
                observer_clock.move_forward(HEARING_SAMPLE_INTERVAL);
                observer_clock
                    .asked_to_wake(
                        "the hearing sample after the first",
                        observer_clock
                            .monotonic()
                            .now()
                            .checked_add(HEARING_SAMPLE_INTERVAL)
                            .unwrap(),
                    )
                    .await;
                answer_on(&live_direct, probe_from_neighbor()).await;
                neighbor_announces_itself_and_the_observer_takes_it(&neighbor, &observer_host)
                    .await;
                observer_clock.move_forward(
                    SPEC_GONE_PROCESS_SILENCE.saturating_add(HEARING_SAMPLE_INTERVAL),
                );
                neighbor_announces_itself_and_the_observer_takes_it(&neighbor, &observer_host)
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
        let (record, mut recorded) = recorded_events();
        let (mut effects, _operator_effects) = lab.effects_recording_every_event_to(
            vec![neighbor.address.clone()],
            Box::pin(async move { drop(interrupted.await) }),
            record,
        );
        let observer_clock = StandingClock::standing_an_hour_ahead();
        effects.monotonic = observer_clock.monotonic();
        let setup = load_network_setup(&lab.directories, &NETWORK_NAME.parse().unwrap()).unwrap();
        let observation = start_observation(&setup, effects).await.unwrap();
        let observer_host = Arc::clone(&observation.host);
        let mut tail_output = Vec::new();

        let (tail_result, (recorded_before_the_close, closed_at_the_presence)) = tokio::join!(
            observation.tail(&mut tail_output),
            async {
                let introduced_on = first_connection_accepted(&neighbor, PAYLOAD_WAIT).await;
                neighbor_listed_by(&observer_host, PAYLOAD_WAIT).await;
                let introduction = next_recorded_meeting(
                    &mut recorded,
                    CONNECTION_OPENED_EVENT,
                    |fields| {
                        field(fields, PROTOCOL_FIELD)
                            == Some(&serde_json::Value::from(DIRECT_PROTOCOL))
                            && field(fields, PEER_FIELD) == Some(&neighbor_short_id())
                    },
                    PAYLOAD_WAIT,
                )
                .await;
                let recorded_past_the_acks = recorded_once_the_clock_moves_past_every_ack(
                    &neighbor,
                    &mut recorded,
                    &observer_clock,
                )
                .await;
                assert!(
                    !recorded_past_the_acks.iter().any(|(name, fields)| {
                        *name == CONNECTION_CLOSED_EVENT
                            && field(fields, CONNECTION_FIELD)
                                == field(&introduction, CONNECTION_FIELD)
                    }),
                    "the observer closed the introduction's connection once its clock read {ACK_DROP_WOULD_SHOW_WITHIN:?} past every IntroduceAck, before any Presence: {recorded_past_the_acks:?}"
                );
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
                let (recorded_before_the_close, _closed) = recorded_until(
                    &mut recorded,
                    CONNECTION_CLOSED_EVENT,
                    |fields| {
                        field(fields, CONNECTION_FIELD) == field(&introduction, CONNECTION_FIELD)
                    },
                    SUPERSEDED_CLOSE_WAIT,
                )
                .await;
                interrupt.send(()).unwrap_or_default();
                (recorded_before_the_close, closed_at_the_presence)
            }
        );

        tail_result.unwrap();
        assert!(
            kinds_and_peers_recorded_as(&recorded_before_the_close, PAYLOAD_ACCEPTED).contains(&(
                Some(serde_json::Value::from(PRESENCE_EVENT_KIND)),
                Some(neighbor_short_id())
            )),
            "the observer closed the introduction's connection before it took the member's next Presence, sent once its clock read {ACK_DROP_WOULD_SHOW_WITHIN:?} past every IntroduceAck: {recorded_before_the_close:?}"
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

    /// Every event the observer records after the introduction's connection opened until it takes a presence request after its clock has moved past every ack, the clock moved only once it has taken a request sent after the answer to its second Introduce.
    async fn recorded_once_the_clock_moves_past_every_ack(
        neighbor: &SilentNeighbor,
        recorded: &mut tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
        observer_clock: &StandingClock,
    ) -> Vec<RecordedEvent> {
        let (mut recorded_past_the_acks, _second_ack) = recorded_until(
            recorded,
            REQUEST_SENT,
            |fields| {
                field(fields, KIND_FIELD) == Some(&serde_json::Value::from(INTRODUCE_KIND))
                    && field(fields, CONNECTION_SOURCE_FIELD)
                        == Some(&serde_json::Value::from(HELD_CONNECTION_SOURCE))
            },
            PAYLOAD_WAIT,
        )
        .await;
        recorded_past_the_acks.extend(
            recorded_until_the_observer_takes_a_request(
                neighbor,
                recorded,
                REQUEST_SEQ_BEFORE_THE_MOVE,
            )
            .await,
        );
        observer_clock.move_forward(ACK_DROP_WOULD_SHOW_WITHIN);
        recorded_past_the_acks.extend(
            recorded_until_the_observer_takes_a_request(
                neighbor,
                recorded,
                REQUEST_SEQ_AFTER_THE_MOVE,
            )
            .await,
        );
        recorded_past_the_acks
    }

    /// Every event the observer records before it takes a presence request the neighbor broadcasts with the seq, failing when it takes none within the payload wait.
    async fn recorded_until_the_observer_takes_a_request(
        neighbor: &SilentNeighbor,
        recorded: &mut tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
        seq: u64,
    ) -> Vec<RecordedEvent> {
        let requester = wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public());
        neighbor
            .topic
            .broadcast(&secret_key(NEIGHBOR_SEED), presence_request(requester, seq))
            .await
            .unwrap();
        let (before, _taken) = recorded_until(
            recorded,
            PAYLOAD_ACCEPTED,
            |fields| {
                field(fields, KIND_FIELD)
                    == Some(&serde_json::Value::from(PRESENCE_REQUEST_EVENT_KIND))
                    && field(fields, SEQ_FIELD) == Some(&serde_json::Value::from(seq))
            },
            PAYLOAD_WAIT,
        )
        .await;
        before
    }

    /// What a node holds for the neighbor: its entry as show-peers lists it, the address it carries, and the lookup entry the node dials it by.
    #[derive(Debug, PartialEq, Eq)]
    struct HeldNeighbor {
        line: Option<String>,
        addr: Option<Addr>,
        lookup: Option<EndpointAddr>,
    }

    /// What a node listing the peers and dialing through the endpoint holds for the neighbor now.
    async fn held_neighbor(peers: &[Peer], endpoint: &BoundEndpoint) -> HeldNeighbor {
        let neighbor = secret_key(NEIGHBOR_SEED).public();
        let entry = peers
            .iter()
            .find(|peer| peer.card == wire::PublicKey::from(neighbor));
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
        settled: impl Fn(&HeldNeighbor) -> bool + Send + Sync,
    ) -> (HeldNeighbor, HeldNeighbor) {
        let mut last_held = None;
        let awaited_holding = format!("member and observer both holding {awaited}");
        let settled_holdings = found_within(SAME_ROWS_WAIT, &awaited_holding, async || {
            let by_member = held_neighbor(&member.node.peers(), &member.endpoint).await;
            let observer_peers: Vec<Peer> =
                observer.table().peers(wall_clock_ms()).cloned().collect();
            let by_observer = held_neighbor(&observer_peers, &observer.endpoint).await;
            if settled(&by_member) && settled(&by_observer) {
                return Some((by_member, by_observer));
            }
            last_held = Some((by_member, by_observer));
            None
        })
        .await;
        settled_holdings
            .unwrap_or_else(|missed| panic!("{missed}: (member, observer) {last_held:?}"))
    }

    /// Waits until the member holds the neighbor and the observer holds both, so each introduction a neighbor report started is answered, failing with what each holds when the wait ends first.
    async fn introduced_to_member_and_joined_observer(member: &Member, observer: &ObserverHost) {
        const AWAITED_INTRODUCTION: &str =
            "neighbor introduced to the member and the joined observer";
        let neighbor = wire::PublicKey::from(secret_key(NEIGHBOR_SEED).public());
        let mut last_held = None;
        found_within(SAME_ROWS_WAIT, AWAITED_INTRODUCTION, async || {
            let member_holds_neighbor =
                member.node.peers().iter().any(|peer| peer.card == neighbor);
            let held_by_observer: Vec<wire::PublicKey> = observer
                .table()
                .peers(wall_clock_ms())
                .map(|peer| peer.card)
                .collect();
            let observer_holds_neighbor = held_by_observer.contains(&neighbor);
            let observer_holds_member =
                held_by_observer.contains(&wire::PublicKey::from(member.address.id));
            let held = (
                member_holds_neighbor,
                observer_holds_neighbor,
                observer_holds_member,
            );
            last_held = Some(held);
            (held == (true, true, true)).then_some(())
        })
        .await
        .unwrap_or_else(|missed| {
            panic!("{missed}: (member holds the neighbor, observer holds the neighbor, observer holds the member) {last_held:?}")
        });
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
                let announced_holdings = held_by_member_and_observer(
                    &members[0],
                    &observer_host,
                    "the announced address",
                    |held| held.addr.as_ref() == Some(&announced),
                )
                .await;
                assert!(
                    [&announced_holdings.0, &announced_holdings.1]
                        .iter()
                        .all(|holding| holding
                            .line
                            .as_deref()
                            .is_some_and(|line| line.contains(ONLINE_COLUMN))),
                    "the neighbor is not listed online after its Presence: {announced_holdings:?}"
                );
                held.push(announced_holdings);
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
            debug: DebugArgs::default(),
        };

        let refusal = run(
            &show_peers,
            &lab.directories,
            operating_system_filesystem(),
            &NoPause,
            lab.effects(Vec::new(), never_interrupted()),
            &mut output,
        )
        .await
        .unwrap_err();

        let configuration_file = lab
            .directories
            .network_config_file(&NETWORK_NAME.parse().unwrap());
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
        wire::PublicKey::from_bytes([seed; 32])
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
        drop(table.direct_contact_failed(card_of(2), LINE_CLOCK_MS, FailedContactKind::Send));
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
