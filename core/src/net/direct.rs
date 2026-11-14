use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use iroh::endpoint::{
    Closed, ClosedStream, Connection, ConnectionError, PathEvent, PathEventStream, ReadError,
    ReadToEndError, RecvStream, SendStream, VarInt, WeakConnectionHandle, WriteError,
};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::{Instant, sleep, timeout};

use super::endpoint::{
    Close, CloseReason, ConnectionId, DIRECT_ALPN, Direction, NodeEndpoint, PathChange, Protocol,
    QUIC_KEEP_ALIVE_INTERVAL, TransportRecord,
};
use crate::crypto::NetworkKeys;
use crate::errors::ErrorCode;
use crate::event::{DirectEvent, Event, ShortId, TransportEvent};
use crate::payload::{
    PayloadError, ZERO_MAC, open_request, open_response, seal_request, seal_response,
};
use crate::peers::{admissible_introduction_handle, announced_sender_handle};
use crate::stream_items::StreamExt;
use crate::wire::{
    self, IntroduceAck, MESSAGE_BODY_LIMIT_BYTES, Message, MessageAck, Nonce, Probe, ProbeAck,
    Request, Response, Version, WIRE_LIMIT_BYTES,
};

const SERVER_READ_TIMEOUT: Duration = Duration::from_millis(10_000);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(5000);
const ACK_TIMEOUT: Duration = Duration::from_millis(10_000);
const PROBE_TIMEOUT: Duration = Duration::from_millis(3000);
const RESET_CODE_OVER_WIRE_LIMIT: u32 = 1;
const RESET_CODE_READ_TIMEOUT: u32 = 2;
const RESET_CODE_UNANSWERED: u32 = 3;
pub(crate) const SUPERSEDED_CONNECTION_CLOSE_CODE: u32 = 410;
pub(crate) const SUPERSEDED_CONNECTION_CLOSE_REASON: &[u8] = b"superseded";
const HEARING_SAMPLES_PER_KEEP_ALIVE: u32 = 2;
const UNANSWERED_KEEP_ALIVES_OF_A_GONE_PROCESS: u32 = 3;
/// How often the register reads what each held connection has heard.
pub(crate) const HEARING_SAMPLE_INTERVAL: Duration =
    match QUIC_KEEP_ALIVE_INTERVAL.checked_div(HEARING_SAMPLES_PER_KEEP_ALIVE) {
        Some(interval) => interval,
        None => QUIC_KEEP_ALIVE_INTERVAL,
    };
/// How long a connection hears nothing before it is taken for one of a process that no longer runs.
pub(crate) const GONE_PROCESS_SILENCE: Duration =
    QUIC_KEEP_ALIVE_INTERVAL.saturating_mul(UNANSWERED_KEEP_ALIVES_OF_A_GONE_PROCESS);

/// What the direct server needs from the node: its identity and state, and the effects of a received Message and Introduce.
pub trait DirectHost: Send + Sync + 'static {
    /// Whether the process is Stopping, so that Messages are refused and Probes and Introduces go unanswered.
    fn is_stopping(&self) -> bool;

    /// The card public key of this process, or none for a node that holds no card and so refuses every Message.
    fn own_card(&self) -> Option<wire::PublicKey>;

    /// Whether an inbox client is attached to this process.
    fn is_attached(&self) -> bool;

    /// Applies the Message rows of spec 3.8 to the sender, enqueues the validated message in the inbox, and returns whether an inbox client was attached as it was enqueued.
    fn receive_message(&self, sender: wire::PublicKey, message: Message) -> bool;

    /// Applies the Introduce rows of spec 3.8 to the sender and answers with this node's own introduction.
    fn receive_introduce(
        &self,
        sender: wire::PublicKey,
        introduce: wire::Introduce,
    ) -> IntroduceAck;
}

/// A monotonic clock, read for the instant now.
pub type MonotonicClock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// Every connection the endpoint's gate let through and has not seen close, of any protocol, dialed or accepted, by the card at its other end, the cards whose spared connection still leaves iroh on a gone process's path, and the clock their silence is measured on.
#[derive(Clone)]
pub struct HeldConnections {
    held: Arc<Mutex<HashMap<wire::PublicKey, Vec<HeldConnection>>>>,
    left_on_a_gone_path: Arc<Mutex<HashSet<wire::PublicKey>>>,
    monotonic: MonotonicClock,
    transport: TransportRecord,
}

impl fmt::Debug for HeldConnections {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeldConnections")
            .finish_non_exhaustive()
    }
}

impl HeldConnections {
    /// An empty register that measures each connection's silence on the monotonic clock and records every connection it holds, each of their paths and each close to the transport record.
    #[must_use]
    pub fn new(monotonic: MonotonicClock, transport: TransportRecord) -> Self {
        Self {
            held: Arc::default(),
            left_on_a_gone_path: Arc::default(),
            monotonic,
            transport,
        }
    }

    /// Closes every connection to or from the card, of any protocol, whose process no longer runs, and every other while iroh stays on that process's path, so the endpoint forgets the path it held to that process (spec 3.8, address lookup).
    pub(crate) fn close_silent_of(&self, card: wire::PublicKey) {
        self.close_silent_of_sparing(card, None);
    }

    /// Closes every connection to or from the card whose process no longer runs, and every other while iroh stays on that process's path, but the one an Introduce arrived on (spec 3.8, address lookup).
    fn close_silent_others_of(&self, card: wire::PublicKey, arrived_on: &Connection) {
        self.close_silent_of_sparing(card, Some(ConnectionId::of(arrived_on)));
    }

    /// Closes every connection to or from the card that has heard nothing from its peer for as long as a gone process is silent, but the spared one, and then, when this pass or an earlier one found a gone process and none left reports a path iroh selected, every other but the spared one too.
    fn close_silent_of_sparing(&self, card: wire::PublicKey, spared: Option<ConnectionId>) {
        let now = (self.monotonic)();
        let mut held = self.held();
        let mut left_on_a_gone_path = self
            .left_on_a_gone_path
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let superseded = held
            .get_mut(&card)
            .map_or_else(Superseded::default, |of_card| {
                take_superseded(of_card, spared, now, left_on_a_gone_path.contains(&card))
            });
        if superseded.spared_leaves_a_gone_path {
            left_on_a_gone_path.insert(card);
        } else {
            left_on_a_gone_path.remove(&card);
        }
        drop(left_on_a_gone_path);
        drop(held);
        self.close_superseded(superseded.connections);
    }

    /// Closes each connection as superseded by a newer one of its card, recording each close it writes.
    fn close_superseded(&self, connections: Vec<Connection>) {
        for connection in connections {
            self.record(|_transport| TransportEvent::ConnectionClosing {
                connection: ConnectionId::of(&connection),
                peer: ShortId::from(connection.remote_id()),
                code: SUPERSEDED_CONNECTION_CLOSE_CODE,
                reason: CloseReason::Superseded,
            });
            connection.close(
                VarInt::from_u32(SUPERSEDED_CONNECTION_CLOSE_CODE),
                SUPERSEDED_CONNECTION_CLOSE_REASON,
            );
        }
    }

    /// Hands the event, built against the transport record, to that record.
    pub(crate) fn record<Recorded: Into<Event>>(
        &self,
        event: impl FnOnce(&TransportRecord) -> Recorded,
    ) {
        self.transport.record(event(&self.transport));
    }

    /// Holds a connection the gate just let through and follows what it hears until it closes, keeping it no more alive than its users do.
    pub(crate) fn hold(&self, connection: &Connection) {
        let card = *connection.remote_id().as_bytes();
        let weak = connection.weak_handle();
        let closed = weak.closed();
        let paths = connection.path_events();
        self.record(|_transport| TransportEvent::ConnectionOpened {
            connection: ConnectionId::of(connection),
            peer: ShortId::from(connection.remote_id()),
            direction: Direction::of(connection.side()),
            protocol: Protocol::of(connection.alpn()),
        });
        self.record_paths_open_at_hold(connection);
        self.add(
            card,
            HeldConnection::heard_at(connection, (self.monotonic)()),
        );
        tokio::spawn(self.clone().follow_hearing(
            card,
            ConnectionId::of(connection),
            weak,
            closed,
            paths,
        ));
    }

    /// Adds the connection under its card, forgetting every card left with no connection.
    fn add(&self, card: wire::PublicKey, held_connection: HeldConnection) {
        let mut held = self.held();
        held.retain(|_, of_card| !of_card.is_empty());
        held.entry(card).or_default().push(held_connection);
    }

    /// Reads what the connection to or from the card has heard every hearing sample interval and records each change of its paths, a change already reported before the close included, until it closes, then records the close and forgets it.
    async fn follow_hearing(
        self,
        card: wire::PublicKey,
        connection_id: ConnectionId,
        connection: WeakConnectionHandle,
        closed: impl Future<Output = Option<Closed>>,
        mut paths: PathEventStream,
    ) {
        tokio::pin!(closed);
        let ended = loop {
            tokio::select! {
                biased;
                Some(change) = paths.next() => self.record_path_change(connection_id, &connection, change),
                ended = &mut closed => break ended,
                () = sleep(HEARING_SAMPLE_INTERVAL) => self.note_hearing(card, connection_id, &connection),
            }
        };
        if let Some(ended) = ended {
            self.record(|_transport| TransportEvent::ConnectionClosed {
                connection: connection_id,
                close: Close::of(&ended.reason),
            });
        }
        self.forget(card, connection_id);
    }

    /// Records each path the connection opened before the register held it, and the one selected among them, since the path events it follows from then on omit them.
    fn record_paths_open_at_hold(&self, connection: &Connection) {
        for path in connection.paths().iter() {
            let changes: &[PathChange] = if path.is_selected() {
                &[PathChange::Opened, PathChange::Selected]
            } else {
                &[PathChange::Opened]
            };
            for change in changes {
                self.record(|transport| TransportEvent::PathChanged {
                    connection: ConnectionId::of(connection),
                    change: *change,
                    path: path.id(),
                    location: transport.locate(path.remote_addr()),
                    round_trip_ms: Some(milliseconds_of(path.rtt())),
                });
            }
        }
    }

    /// Records a change of one of the connection's paths with where the path leads and its round-trip time, or the count iroh lost, and nothing for a path event this build cannot read.
    fn record_path_change(
        &self,
        connection_id: ConnectionId,
        connection: &WeakConnectionHandle,
        change: PathEvent,
    ) {
        let live_round_trip = |path| {
            connection
                .upgrade()
                .and_then(|live| live.rtt(path))
                .map(milliseconds_of)
        };
        let (change, path, remote_addr, round_trip_ms) = match change {
            PathEvent::Opened {
                id, remote_addr, ..
            } => (PathChange::Opened, id, remote_addr, live_round_trip(id)),
            PathEvent::Selected {
                id, remote_addr, ..
            } => (PathChange::Selected, id, remote_addr, live_round_trip(id)),
            PathEvent::Closed {
                id,
                remote_addr,
                last_stats,
                ..
            } => (
                PathChange::Closed,
                id,
                remote_addr,
                Some(milliseconds_of(last_stats.rtt)),
            ),
            PathEvent::Lagged { missed, .. } => {
                return self.record(|_transport| path_events_missed(connection_id, missed));
            }
            _ => return,
        };
        self.record(|transport| TransportEvent::PathChanged {
            connection: connection_id,
            change,
            path,
            location: transport.locate(&remote_addr),
            round_trip_ms,
        });
    }

    /// Takes now as the last time the connection to or from the card heard from its peer, when it received a datagram since its last reading.
    fn note_hearing(
        &self,
        card: wire::PublicKey,
        connection_id: ConnectionId,
        connection: &WeakConnectionHandle,
    ) {
        let Some(datagrams) = connection
            .upgrade()
            .map(|live| live.stats().udp_rx.datagrams)
        else {
            return;
        };
        let now = (self.monotonic)();
        if let Some(held_connection) = self.held().get_mut(&card).and_then(|of_card| {
            of_card
                .iter_mut()
                .find(|held_connection| held_connection.id == connection_id)
        }) {
            held_connection.note_hearing(datagrams, now);
        }
    }

    /// Forgets a connection to or from the card that has closed.
    fn forget(&self, card: wire::PublicKey, connection_id: ConnectionId) {
        if let Some(of_card) = self.held().get_mut(&card) {
            of_card.retain(|held_connection| held_connection.id != connection_id);
        }
    }

    /// The register, locked for one change.
    fn held(&self) -> MutexGuard<'_, HashMap<wire::PublicKey, Vec<HeldConnection>>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A connection the gate let through, held weakly, with the count of datagrams it had received when that count last moved and when it was seen to move.
#[derive(Debug)]
struct HeldConnection {
    connection: WeakConnectionHandle,
    id: ConnectionId,
    datagrams_heard: u64,
    last_heard: Instant,
}

impl HeldConnection {
    /// The connection, taken as heard from its peer at `now`.
    fn heard_at(connection: &Connection, now: Instant) -> Self {
        Self {
            connection: connection.weak_handle(),
            id: ConnectionId::of(connection),
            datagrams_heard: connection.stats().udp_rx.datagrams,
            last_heard: now,
        }
    }

    /// Takes `now` as the last time the peer was heard, when the connection's count of received datagrams moved since it was last read.
    const fn note_hearing(&mut self, datagrams: u64, now: Instant) {
        if datagrams != self.datagrams_heard {
            self.datagrams_heard = datagrams;
            self.last_heard = now;
        }
    }

    /// Whether the peer has been silent on the connection for the gone-process silence.
    fn is_from_a_gone_process(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_heard) >= GONE_PROCESS_SILENCE
    }

    /// Whether the connection is open and one of its paths is the one iroh selected to reach the card's endpoint.
    fn reports_a_selected_path(&self) -> bool {
        self.connection
            .upgrade()
            .is_some_and(|live| live.paths().iter().any(|path| path.is_selected()))
    }
}

/// The connections one pass takes out of a card's held connections, and whether the connection it spared still leaves iroh on a gone process's path.
#[derive(Default)]
struct Superseded {
    connections: Vec<Connection>,
    spared_leaves_a_gone_path: bool,
}

/// Takes out the card's connections from a gone process but the spared one, and with them every other but the spared one when this pass found a gone process, or an earlier one left the card on its path, and none left reports a path iroh selected, since iroh keeps the gone process's path selected until the card's last connection closes.
fn take_superseded(
    of_card: &mut Vec<HeldConnection>,
    spared: Option<ConnectionId>,
    now: Instant,
    left_on_a_gone_path: bool,
) -> Superseded {
    let mut connections: Vec<Connection> = of_card
        .extract_if(.., |held_connection| {
            Some(held_connection.id) != spared && held_connection.is_from_a_gone_process(now)
        })
        .filter_map(|held_connection| held_connection.connection.upgrade())
        .collect();
    let on_a_gone_path = (!connections.is_empty() || left_on_a_gone_path)
        && !of_card.iter().any(HeldConnection::reports_a_selected_path);
    if on_a_gone_path {
        connections.extend(
            of_card
                .extract_if(.., |held_connection| Some(held_connection.id) != spared)
                .filter_map(|held_connection| held_connection.connection.upgrade()),
        );
    }
    Superseded {
        connections,
        spared_leaves_a_gone_path: on_a_gone_path && !of_card.is_empty(),
    }
}

/// The kind of a direct request and, for a Message, its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestSummary {
    Message(wire::MessageId),
    Probe,
    Introduce,
}

impl RequestSummary {
    /// The kind and message id of the request.
    #[must_use]
    pub const fn of(request: &Request) -> Self {
        match request {
            Request::Message(message) => Self::Message(message.id),
            Request::Probe(_) => Self::Probe,
            Request::Introduce(_) => Self::Introduce,
        }
    }
}

/// The text a message id takes in the inbox, in a send result and in the events file.
pub(crate) fn message_id_text(id: wire::MessageId) -> String {
    uuid::Uuid::from_bytes(id).to_string()
}

/// Whether a request rode the connection the client held or one it dialed for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionSource {
    Held,
    Dialed,
}

/// The error code a Message acknowledgement refuses with, as one of the codes estafette writes or another by none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckRefusal {
    TooLarge,
    Refused,
    Unlisted,
}

impl AckRefusal {
    /// The refusal named by the reason of a Message acknowledgement.
    #[must_use]
    pub fn of(reason: Option<&str>) -> Self {
        match reason {
            Some(code) if code == ErrorCode::TooLarge.code() => Self::TooLarge,
            Some(code) if code == ErrorCode::Refused.code() => Self::Refused,
            Some(_) | None => Self::Unlisted,
        }
    }
}

/// How a request this node sent ended: acknowledged, acknowledged as a refused Message, or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SentOutcome {
    Acknowledged,
    Refused(AckRefusal),
    Failed(DirectError),
}

impl SentOutcome {
    /// The outcome a Message acknowledgement reports.
    #[must_use]
    pub fn of_message_ack(ack: &MessageAck) -> Self {
        if ack.accepted {
            Self::Acknowledged
        } else {
            Self::Refused(AckRefusal::of(ack.reason.as_deref()))
        }
    }
}

/// How a request this node served ended: answered, answered as a refused Message, or left unanswered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedOutcome {
    Answered,
    Refused(AckRefusal),
    Unanswered(StreamEnd),
}

impl ServedOutcome {
    /// The outcome of answering with the response.
    #[must_use]
    pub fn of(response: &Response) -> Self {
        match response {
            Response::MessageAck(ack) if !ack.accepted => {
                Self::Refused(AckRefusal::of(ack.reason.as_deref()))
            }
            Response::MessageAck(_) | Response::ProbeAck(_) | Response::IntroduceAck(_) => {
                Self::Answered
            }
        }
    }
}

/// A request this node sent: which, to whom, over which connection, how it ended and how long it took on the register's monotonic clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentRequest {
    pub request: RequestSummary,
    pub peer: ShortId,
    pub connection: Option<(ConnectionId, ConnectionSource)>,
    pub outcome: SentOutcome,
    pub elapsed_ms: u64,
}

/// A request this node served: which once it opened, from whom, on which connection, how it ended and how long it took on the register's monotonic clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedRequest {
    pub request: Option<RequestSummary>,
    pub peer: ShortId,
    pub connection: ConnectionId,
    pub outcome: ServedOutcome,
    pub elapsed_ms: u64,
}

/// The record of the connection's path events lost before they were read, as many as iroh counted.
const fn path_events_missed(connection: ConnectionId, missed: u64) -> TransportEvent {
    TransportEvent::PathEventsMissed { connection, missed }
}

/// The whole milliseconds of a span, saturating past `u64::MAX`.
fn milliseconds_of(span: Duration) -> u64 {
    u64::try_from(span.as_millis()).unwrap_or(u64::MAX)
}

/// The timeouts of the direct client, in the units of spec 3.14.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientTimeouts {
    pub connect: Duration,
    pub ack: Duration,
    pub probe: Duration,
}

impl ClientTimeouts {
    pub const SPEC: Self = Self {
        connect: CONNECT_TIMEOUT,
        ack: ACK_TIMEOUT,
        probe: PROBE_TIMEOUT,
    };
}

/// When a request's acknowledgement is due, which also bounds how long getting a connection may take.
#[derive(Debug, Clone, Copy)]
enum AckDue {
    /// Within the timeout from the moment a connection is in hand, after getting it within the connect timeout (spec 3.10, steps 4 and 5).
    FromTheConnection(Duration),
    /// Within the timeout from the call, getting the connection included (spec 3.6: a probe fails when no valid `ProbeAck` arrives within the probe timeout, for any reason).
    FromTheCall(Duration),
}

impl AckDue {
    /// How long a request may take to hold a connection, never longer than the connect timeout allows.
    fn connection_timeout(self, connect_timeout: Duration) -> Duration {
        match self {
            Self::FromTheConnection(_) => connect_timeout,
            Self::FromTheCall(ack_timeout) => ack_timeout.min(connect_timeout),
        }
    }

    /// How long the acknowledgement of a request called `since_call` ago, now holding its connection, may still take.
    const fn ack_timeout_left(self, since_call: Duration) -> Duration {
        match self {
            Self::FromTheConnection(ack_timeout) => ack_timeout,
            Self::FromTheCall(ack_timeout) => ack_timeout.saturating_sub(since_call),
        }
    }
}

/// Why a direct request did not return its valid acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectError {
    Unreachable,
    Timeout,
    Refused,
    UnsealableRequest(PayloadError),
    InvalidAck(PayloadError),
    OversizedAck,
    UnexpectedAck,
    MessageAckIdMismatch,
    ProbeNonceMismatch,
}

impl fmt::Display for DirectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable => formatter.write_str("the peer could not be connected to"),
            Self::Timeout => formatter.write_str("the peer did not acknowledge in time"),
            Self::Refused => formatter.write_str("the peer reset the stream"),
            Self::UnsealableRequest(source) => {
                write!(formatter, "the request does not encode: {source}")
            }
            Self::InvalidAck(source) => {
                write!(formatter, "the acknowledgement is not valid: {source}")
            }
            Self::OversizedAck => {
                write!(
                    formatter,
                    "the acknowledgement is past the wire limit of {WIRE_LIMIT_BYTES} bytes"
                )
            }
            Self::UnexpectedAck => {
                formatter.write_str("the acknowledgement answers another kind of request")
            }
            Self::MessageAckIdMismatch => {
                formatter.write_str("the message acknowledgement carries another message id")
            }
            Self::ProbeNonceMismatch => {
                formatter.write_str("the probe acknowledgement carries another nonce")
            }
        }
    }
}

impl std::error::Error for DirectError {}

/// The server half of the direct protocol, served on the connections the router hands it.
pub struct DirectServer<H> {
    shared: Arc<ServerState<H>>,
}

struct ServerState<H> {
    keys: NetworkKeys,
    held: HeldConnections,
    host: H,
}

impl<H: DirectHost> DirectServer<H> {
    /// Builds the server over the host on the endpoint it serves, answering under the network keys, closing connections through the endpoint's own register, and waiting the server read timeout of spec 3.14 for each request.
    pub fn new(keys: NetworkKeys, host: H, endpoint: &NodeEndpoint) -> Self {
        Self {
            shared: Arc::new(ServerState {
                keys,
                held: endpoint.held_connections(),
                host,
            }),
        }
    }
}

impl<H> fmt::Debug for DirectServer<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectServer")
            .finish_non_exhaustive()
    }
}

impl<H: DirectHost> ProtocolHandler for DirectServer<H> {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let sender = *connection.remote_id().as_bytes();
        while let Ok((send, recv)) = connection.accept_bi().await {
            let shared = Arc::clone(&self.shared);
            let arrived_on = connection.clone();
            tokio::spawn(async move {
                shared.serve_stream(sender, &arrived_on, send, recv).await;
            });
        }
        Ok(())
    }
}

/// Why the server ended a stream without its response: a reset with its code, or a peer already gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEnd {
    OverWireLimit,
    ReadTimeout,
    Unanswered,
    PeerGone,
}

impl StreamEnd {
    /// The code the stream is reset with, or none when the peer is gone.
    const fn reset_code(self) -> Option<VarInt> {
        match self {
            Self::OverWireLimit => Some(VarInt::from_u32(RESET_CODE_OVER_WIRE_LIMIT)),
            Self::ReadTimeout => Some(VarInt::from_u32(RESET_CODE_READ_TIMEOUT)),
            Self::Unanswered => Some(VarInt::from_u32(RESET_CODE_UNANSWERED)),
            Self::PeerGone => None,
        }
    }
}

impl From<ReadError> for StreamEnd {
    fn from(_failure: ReadError) -> Self {
        Self::PeerGone
    }
}

impl From<WriteError> for StreamEnd {
    fn from(_failure: WriteError) -> Self {
        Self::PeerGone
    }
}

impl From<ClosedStream> for StreamEnd {
    fn from(_failure: ClosedStream) -> Self {
        Self::PeerGone
    }
}

impl<H: DirectHost> ServerState<H> {
    /// Answers the one request on the stream, or resets the stream when no answer is due, and records the request served with how it ended.
    async fn serve_stream(
        &self,
        sender: wire::PublicKey,
        arrived_on: &Connection,
        mut send: SendStream,
        recv: RecvStream,
    ) {
        let started = (self.held.monotonic)();
        let (request, outcome) = self
            .answer_stream(sender, arrived_on, &mut send, recv)
            .await;
        if let Err(end) = outcome
            && let Some(code) = end.reset_code()
        {
            match send.reset(code) {
                Ok(()) | Err(ClosedStream { .. }) => {}
            }
        }
        let elapsed = (self.held.monotonic)().saturating_duration_since(started);
        self.held.record(|_transport| {
            DirectEvent::RequestServed(ServedRequest {
                request,
                peer: ShortId::from(arrived_on.remote_id()),
                connection: ConnectionId::of(arrived_on),
                outcome: outcome.unwrap_or_else(ServedOutcome::Unanswered),
                elapsed_ms: milliseconds_of(elapsed),
            })
        });
    }

    /// Reads and opens the request, writes the response and finishes the send side, saying which request it was once it opened and how the stream ended.
    async fn answer_stream(
        &self,
        sender: wire::PublicKey,
        arrived_on: &Connection,
        send: &mut SendStream,
        mut recv: RecvStream,
    ) -> (Option<RequestSummary>, Result<ServedOutcome, StreamEnd>) {
        let request = match self
            .read_request(&mut recv)
            .await
            .and_then(|request_bytes| {
                open_request(&self.keys, &request_bytes).map_err(|_failure| StreamEnd::Unanswered)
            }) {
            Ok(request) => request,
            Err(end) => return (None, Err(end)),
        };
        let summary = RequestSummary::of(&request);
        let outcome = async {
            let response = self.answer(sender, arrived_on, request)?;
            let outcome = ServedOutcome::of(&response);
            let response_bytes =
                seal_response(&self.keys, response).map_err(|_failure| StreamEnd::Unanswered)?;
            send.write_all(&response_bytes).await?;
            send.finish()?;
            Ok::<_, StreamEnd>(outcome)
        }
        .await;
        (Some(summary), outcome)
    }

    /// Reads the whole request within the wire limit and the read timeout.
    async fn read_request(&self, recv: &mut RecvStream) -> Result<Vec<u8>, StreamEnd> {
        match timeout(SERVER_READ_TIMEOUT, recv.read_to_end(WIRE_LIMIT_BYTES)).await {
            Err(_elapsed) => Err(StreamEnd::ReadTimeout),
            Ok(Err(ReadToEndError::TooLong)) => Err(StreamEnd::OverWireLimit),
            Ok(Err(ReadToEndError::Read(failure))) => Err(failure.into()),
            Ok(Ok(bytes)) => Ok(bytes),
        }
    }

    /// The response to a validated request, or none while the process is stopping and the request is a Probe or an Introduce, or for an Introduce the node would drop, before any connection of its sender is closed.
    fn answer(
        &self,
        sender: wire::PublicKey,
        arrived_on: &Connection,
        request: Request,
    ) -> Result<Response, StreamEnd> {
        match request {
            Request::Message(message) => {
                Ok(Response::MessageAck(self.answer_message(sender, message)))
            }
            Request::Probe(_) | Request::Introduce(_) if self.host.is_stopping() => {
                Err(StreamEnd::Unanswered)
            }
            Request::Introduce(introduce)
                if admissible_introduction_handle(
                    introduce.handle.as_deref(),
                    &introduce.public,
                )
                .is_err() =>
            {
                Err(StreamEnd::Unanswered)
            }
            Request::Probe(probe) => Ok(Response::ProbeAck(self.answer_probe(&probe))),
            Request::Introduce(introduce) => {
                self.held.close_silent_others_of(sender, arrived_on);
                Ok(Response::IntroduceAck(
                    self.host.receive_introduce(sender, introduce),
                ))
            }
        }
    }

    /// Echoes the probe's nonce with this process's attached state.
    fn answer_probe(&self, probe: &Probe) -> ProbeAck {
        ProbeAck {
            v: Version::CURRENT,
            nonce: probe.nonce,
            attached: self.host.is_attached(),
            mac: ZERO_MAC,
        }
    }

    /// Queues the message when it is acceptable and acknowledges it, accepted or with the reason it was not, carrying the attached state the queueing read.
    fn answer_message(&self, sender: wire::PublicKey, message: Message) -> MessageAck {
        let id = message.id;
        let rejection = judge_message(self.host.own_card(), self.host.is_stopping(), &message);
        let attached = match rejection {
            None => self.host.receive_message(sender, message),
            Some(_) => self.host.is_attached(),
        };
        MessageAck {
            v: Version::CURRENT,
            id,
            accepted: rejection.is_none(),
            attached,
            reason: rejection.map(|code| code.code().to_owned()),
        }
    }
}

/// The error code a received Message is rejected with by the checks of spec 3.6 steps 3 and 4, or by the node's drop of a Message whose announced handle does not match (spec 3.1), or none when it is acceptable.
fn judge_message(
    own_card: Option<wire::PublicKey>,
    is_stopping: bool,
    message: &Message,
) -> Option<ErrorCode> {
    if own_card != Some(message.to) || is_stopping {
        Some(ErrorCode::Refused)
    } else if message.body.len() > MESSAGE_BODY_LIMIT_BYTES {
        Some(ErrorCode::TooLarge)
    } else if announced_sender_handle(message).is_none() {
        Some(ErrorCode::Refused)
    } else {
        None
    }
}

type PeerSlot = Arc<AsyncMutex<Option<Connection>>>;

/// The client half of the direct protocol, holding one connection per peer in that peer's slot, which a dial fills under the slot's lock, over the node's current endpoint.
pub struct DirectClient {
    endpoint: Mutex<Endpoint>,
    keys: NetworkKeys,
    timeouts: ClientTimeouts,
    slots: Mutex<HashMap<EndpointId, PeerSlot>>,
    register: HeldConnections,
}

impl DirectClient {
    /// Builds the client over the endpoint, macing requests and checking acknowledgements under the network keys, and recording each request it sends to the endpoint's register, on that register's monotonic clock.
    #[must_use]
    pub fn new(
        endpoint: Endpoint,
        keys: NetworkKeys,
        timeouts: ClientTimeouts,
        register: HeldConnections,
    ) -> Self {
        Self {
            endpoint: Mutex::new(endpoint),
            keys,
            timeouts,
            slots: Mutex::new(HashMap::new()),
            register,
        }
    }

    /// The endpoint the client dials from now.
    #[must_use]
    pub(crate) fn endpoint(&self) -> Endpoint {
        self.endpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Dials from the endpoint a rebind bound from here on, and drops every connection held on the one before it (spec 3.8, Rebind).
    pub(crate) async fn follow(&self, endpoint: Endpoint) {
        *self.endpoint.lock().unwrap_or_else(PoisonError::into_inner) = endpoint;
        self.drop_all_held_connections().await;
    }

    /// Sends the message to the peer and returns its acknowledgement of that message id, accepted or not.
    ///
    /// # Errors
    /// `DirectError` when no valid acknowledgement of the message id arrives within the connect and ack timeouts.
    pub async fn send_message(
        &self,
        to: EndpointId,
        message: Message,
    ) -> Result<MessageAck, DirectError> {
        let id = message.id;
        self.exchange(
            to,
            Request::Message(message),
            AckDue::FromTheConnection(self.timeouts.ack),
            |response| match response {
                Response::MessageAck(ack) if ack.id == id => {
                    Ok((SentOutcome::of_message_ack(&ack), ack))
                }
                Response::MessageAck(_) => Err(DirectError::MessageAckIdMismatch),
                Response::ProbeAck(_) | Response::IntroduceAck(_) => {
                    Err(DirectError::UnexpectedAck)
                }
            },
        )
        .await
    }

    /// Probes the peer and returns its acknowledgement when it is valid for the nonce.
    ///
    /// # Errors
    /// `DirectError` when no valid acknowledgement arrives within the probe timeout of the call, the dial included.
    pub async fn probe(&self, to: EndpointId, nonce: Nonce) -> Result<ProbeAck, DirectError> {
        let probe = Probe {
            v: Version::CURRENT,
            nonce,
            mac: ZERO_MAC,
        };
        self.exchange(
            to,
            Request::Probe(probe),
            AckDue::FromTheCall(self.timeouts.probe),
            |response| match response {
                Response::ProbeAck(ack) if ack.nonce == nonce => {
                    Ok((SentOutcome::Acknowledged, ack))
                }
                Response::ProbeAck(_) => Err(DirectError::ProbeNonceMismatch),
                Response::MessageAck(_) | Response::IntroduceAck(_) => {
                    Err(DirectError::UnexpectedAck)
                }
            },
        )
        .await
    }

    /// Introduces this node to the peer and returns the peer's introduction.
    ///
    /// # Errors
    /// `DirectError` when no valid acknowledgement arrives within the connect and ack timeouts.
    pub async fn introduce(
        &self,
        to: EndpointId,
        introduce: wire::Introduce,
    ) -> Result<IntroduceAck, DirectError> {
        self.exchange(
            to,
            Request::Introduce(introduce),
            AckDue::FromTheConnection(self.timeouts.ack),
            |response| match response {
                Response::IntroduceAck(ack) => Ok((SentOutcome::Acknowledged, ack)),
                Response::MessageAck(_) | Response::ProbeAck(_) => Err(DirectError::UnexpectedAck),
            },
        )
        .await
    }

    /// Sends the request, takes its acknowledgement from the response, and records the request sent with its outcome, its connection and how long it took.
    async fn exchange<Acknowledgement>(
        &self,
        to: EndpointId,
        request: Request,
        due: AckDue,
        acknowledgement_of: impl FnOnce(Response) -> Result<(SentOutcome, Acknowledgement), DirectError>,
    ) -> Result<Acknowledgement, DirectError> {
        let summary = RequestSummary::of(&request);
        let started = (self.register.monotonic)();
        let (connection, response) = self.exchange_over_a_connection(to, request, due).await;
        let acknowledged = response.and_then(acknowledgement_of);
        let elapsed = (self.register.monotonic)().saturating_duration_since(started);
        self.register.record(|_transport| {
            DirectEvent::RequestSent(SentRequest {
                request: summary,
                peer: ShortId::from(to),
                connection,
                outcome: acknowledged.as_ref().map_or_else(
                    |failure| SentOutcome::Failed(failure.clone()),
                    |(outcome, _)| outcome.clone(),
                ),
                elapsed_ms: milliseconds_of(elapsed),
            })
        });
        acknowledged.map(|(_outcome, acknowledgement)| acknowledgement)
    }

    /// Sends the request over the held or a freshly dialed connection and returns the valid response when it is due, dropping that connection from its slot on any failure, and says which connection carried it.
    async fn exchange_over_a_connection(
        &self,
        to: EndpointId,
        request: Request,
        due: AckDue,
    ) -> (
        Option<(ConnectionId, ConnectionSource)>,
        Result<Response, DirectError>,
    ) {
        let sealed = match seal_request(&self.keys, request) {
            Ok(sealed) => sealed,
            Err(failure) => return (None, Err(DirectError::UnsealableRequest(failure))),
        };
        let called = Instant::now();
        let (connection, source) = match self
            .connection_to(to, due.connection_timeout(self.timeouts.connect))
            .await
        {
            Ok(held_or_dialed) => held_or_dialed,
            Err(failure) => return (None, Err(failure)),
        };
        let response = self
            .round_trip(&connection, &sealed, due.ack_timeout_left(called.elapsed()))
            .await;
        if response.is_err() {
            self.forget_connection(to, &connection).await;
        }
        (Some((ConnectionId::of(&connection), source)), response)
    }

    /// Writes the sealed request on a new stream and reads the response, all within the ack timeout.
    async fn round_trip(
        &self,
        connection: &Connection,
        sealed_request: &[u8],
        ack_timeout: Duration,
    ) -> Result<Response, DirectError> {
        let response_bytes = timeout(ack_timeout, async {
            let (mut send, mut recv) = connection.open_bi().await?;
            send.write_all(sealed_request).await?;
            send.finish()?;
            Ok::<_, DirectError>(recv.read_to_end(WIRE_LIMIT_BYTES).await?)
        })
        .await
        .map_err(|_elapsed| DirectError::Timeout)??;
        open_response(&self.keys, &response_bytes).map_err(DirectError::InvalidAck)
    }

    /// The held connection to the peer, or a new one dialed by card id and held, saying which, unreachable when neither is in hand within the connection timeout.
    async fn connection_to(
        &self,
        to: EndpointId,
        connection_timeout: Duration,
    ) -> Result<(Connection, ConnectionSource), DirectError> {
        timeout(connection_timeout, self.held_or_dialed_connection(to))
            .await
            .map_err(|_elapsed| DirectError::Unreachable)?
    }

    /// The connection held for the peer, one the client knows closed counting as none and leaving the slot, or else a new one dialed by card id under the slot's lock and held, so concurrent callers share one dial.
    async fn held_or_dialed_connection(
        &self,
        to: EndpointId,
    ) -> Result<(Connection, ConnectionSource), DirectError> {
        let slot = self.slot_of(to);
        let mut held = slot.lock().await;
        held.take_if(|connection| connection.close_reason().is_some());
        if let Some(connection) = held.as_ref() {
            return Ok((connection.clone(), ConnectionSource::Held));
        }
        let dialed = self
            .endpoint()
            .connect(to, DIRECT_ALPN)
            .await
            .map_err(|_failure| DirectError::Unreachable)?;
        Ok((held.insert(dialed).clone(), ConnectionSource::Dialed))
    }

    /// Empties the peer's slot, so the next request to the peer dials afresh.
    pub async fn drop_held_connection(&self, to: EndpointId) {
        *self.slot_of(to).lock().await = None;
    }

    /// Empties every peer's slot, so the next request to any peer dials afresh.
    pub async fn drop_all_held_connections(&self) {
        let slots: Vec<PeerSlot> = self
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        for slot in slots {
            *slot.lock().await = None;
        }
    }

    /// Empties the peer's slot when it still holds the failed connection, leaving a newer connection in place.
    async fn forget_connection(&self, to: EndpointId, failed: &Connection) {
        let slot = self.slot_of(to);
        let mut held = slot.lock().await;
        if held
            .as_ref()
            .is_some_and(|connection| ConnectionId::of(connection) == ConnectionId::of(failed))
        {
            *held = None;
        }
    }

    fn slot_of(&self, to: EndpointId) -> PeerSlot {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(slots.entry(to).or_default())
    }
}

impl From<ConnectionError> for DirectError {
    fn from(_failure: ConnectionError) -> Self {
        Self::Unreachable
    }
}

impl From<WriteError> for DirectError {
    fn from(_failure: WriteError) -> Self {
        Self::Unreachable
    }
}

impl From<ClosedStream> for DirectError {
    fn from(_failure: ClosedStream) -> Self {
        Self::Unreachable
    }
}

impl From<ReadToEndError> for DirectError {
    fn from(failure: ReadToEndError) -> Self {
        match failure {
            ReadToEndError::TooLong => Self::OversizedAck,
            ReadToEndError::Read(ReadError::Reset(_code)) => Self::Refused,
            ReadToEndError::Read(
                ReadError::ConnectionLost(_) | ReadError::ClosedStream | ReadError::ZeroRttRejected,
            ) => Self::Unreachable,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

    use std::time::Instant;

    use iroh::SecretKey;
    use iroh::endpoint::ReadToEndError;
    use iroh::protocol::Router;
    use iroh_gossip::ALPN as GOSSIP_ALPN;

    use super::*;
    use crate::card::tests::{SPEC_PUBLIC_CAP_BYTES, public_mapping_of_bytes};
    use crate::crypto::{KEY_LEN, NetworkSecret};
    use crate::net::endpoint::NodeEndpoint;
    use crate::seeded_draws::Draws;

    const NETWORK_SECRET: [u8; KEY_LEN] = [7; KEY_LEN];
    const SERVER_SEED: u8 = 1;
    const CLIENT_SEED: u8 = 2;
    const MESSAGE_ID: [u8; 16] = [
        0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87, 0x98, 0xA9, 0xBA, 0xCB, 0xDC, 0xED, 0xFE,
        0x0F,
    ];
    const SENDER_HANDLE: &str = "alice";
    const SENT_AT: u64 = 1_700_000_000;
    const BODY: &str = "hello over the direct protocol";

    fn keys() -> NetworkKeys {
        NetworkKeys::derive(&NetworkSecret::from_bytes(NETWORK_SECRET))
    }

    fn card_of(seed: u8) -> wire::PublicKey {
        *SecretKey::from_bytes(&[seed; KEY_LEN]).public().as_bytes()
    }

    #[derive(Clone)]
    struct RecordingHost {
        own_card: Option<wire::PublicKey>,
        attached: Arc<AtomicBool>,
        detaches_on_delivery: bool,
        stopping: Arc<AtomicBool>,
        received: Arc<Mutex<Vec<(wire::PublicKey, Message)>>>,
        introduced: Arc<Mutex<Vec<(wire::PublicKey, wire::Introduce)>>>,
    }

    impl RecordingHost {
        fn with_card(own_card: wire::PublicKey) -> Self {
            Self::holding(Some(own_card))
        }

        fn without_card() -> Self {
            Self::holding(None)
        }

        fn holding(own_card: Option<wire::PublicKey>) -> Self {
            Self {
                own_card,
                attached: Arc::new(AtomicBool::new(false)),
                detaches_on_delivery: false,
                stopping: Arc::new(AtomicBool::new(false)),
                received: Arc::new(Mutex::new(Vec::new())),
                introduced: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn received(&self) -> Vec<(wire::PublicKey, Message)> {
            self.received.lock().unwrap().clone()
        }
    }

    impl DirectHost for RecordingHost {
        fn is_stopping(&self) -> bool {
            self.stopping.load(Ordering::SeqCst)
        }

        fn own_card(&self) -> Option<wire::PublicKey> {
            self.own_card
        }

        fn is_attached(&self) -> bool {
            self.attached.load(Ordering::SeqCst)
        }

        fn receive_message(&self, sender: wire::PublicKey, message: Message) -> bool {
            self.received.lock().unwrap().push((sender, message));
            let attached_at_delivery = self.is_attached();
            if self.detaches_on_delivery {
                self.attached.store(false, Ordering::SeqCst);
            }
            attached_at_delivery
        }

        fn receive_introduce(
            &self,
            sender: wire::PublicKey,
            introduce: wire::Introduce,
        ) -> IntroduceAck {
            self.introduced.lock().unwrap().push((sender, introduce));
            own_introduction(self.is_attached())
        }
    }

    const OWN_HANDLE: &str = "bob";
    const OWN_PUBLIC: &[u8] = br#"{"role":"server"}"#;
    const RELAY: &str = "https://relay.example/";

    fn addr_of_relay() -> wire::Addr {
        wire::Addr::from_endpoint_report([RELAY.to_owned()], std::iter::empty())
    }

    fn own_introduction(attached: bool) -> IntroduceAck {
        IntroduceAck {
            v: Version::CURRENT,
            handle: Some(OWN_HANDLE.to_owned()),
            public: OWN_PUBLIC.to_vec(),
            attached,
            addr: addr_of_relay(),
            mac: ZERO_MAC,
        }
    }

    async fn bind_node(seed: u8) -> NodeEndpoint {
        bind_node_holding(seed, unrecorded_register()).await
    }

    /// Binds the node of the seed, holding every connection its endpoint lets through in the register.
    async fn bind_node_holding(seed: u8, connections: HeldConnections) -> NodeEndpoint {
        NodeEndpoint::bind(
            SecretKey::from_bytes(&[seed; KEY_LEN]),
            *keys().gate_key(),
            Vec::new(),
            |nonce: &mut [u8; KEY_LEN]| *nonce = [0; KEY_LEN],
            |_reason| {},
            connections,
        )
        .await
        .unwrap()
    }

    struct Rig {
        host: RecordingHost,
        link: Link,
    }

    struct Link {
        client: DirectClient,
        client_node: NodeEndpoint,
        server_node: NodeEndpoint,
        _router: Router,
    }

    async fn link_to(handler: impl ProtocolHandler, timeouts: ClientTimeouts) -> Link {
        link_on(bind_node(SERVER_SEED).await, handler, timeouts).await
    }

    /// A client linked to the server node serving the handler.
    async fn link_on(
        server_node: NodeEndpoint,
        handler: impl ProtocolHandler,
        timeouts: ClientTimeouts,
    ) -> Link {
        let client_node = bind_node(CLIENT_SEED).await;
        let router = Router::builder(server_node.endpoint().clone())
            .accept(DIRECT_ALPN, handler)
            .spawn();
        client_node.replace_address(server_node.endpoint().addr());
        let client = DirectClient::new(
            client_node.endpoint().clone(),
            keys(),
            timeouts,
            unrecorded_register(),
        );
        Link {
            client,
            client_node,
            server_node,
            _router: router,
        }
    }

    async fn rig() -> Rig {
        rig_over(
            RecordingHost::with_card(card_of(SERVER_SEED)),
            Arc::new(tokio::time::Instant::now),
        )
        .await
    }

    /// A server over the host on an endpoint whose register measures silence on the clock, as the node's `serve` has it, and a client linked to it.
    async fn rig_over(host: RecordingHost, monotonic: MonotonicClock) -> Rig {
        let server_node = bind_node_holding(
            SERVER_SEED,
            HeldConnections::new(monotonic, dropping_transport()),
        )
        .await;
        let server = DirectServer::new(keys(), host.clone(), &server_node);
        let link = link_on(server_node, server, ClientTimeouts::SPEC).await;
        Rig { host, link }
    }

    fn message_to(card: wire::PublicKey, body: &str) -> Message {
        Message {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            to: card,
            handle: SENDER_HANDLE.to_owned(),
            attached: true,
            sent_at: SENT_AT,
            body: body.to_owned(),
            mac: ZERO_MAC,
        }
    }

    #[tokio::test]
    async fn spec_3_6_message_to_this_node_is_accepted_and_queued_from_the_remote_card() {
        let rig = rig().await;

        let ack = rig
            .link
            .client
            .send_message(
                rig.link.server_node.endpoint().id(),
                message_to(card_of(SERVER_SEED), BODY),
            )
            .await
            .unwrap();

        assert_eq!(
            ack,
            MessageAck {
                v: Version::CURRENT,
                id: MESSAGE_ID,
                accepted: true,
                attached: false,
                reason: None,
            }
        );
        let received = rig.host.received();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].0, card_of(CLIENT_SEED));
        assert_eq!(received[0].1.body, BODY);
        assert_eq!(received[0].1.id, MESSAGE_ID);
        assert_eq!(received[0].1.handle, SENDER_HANDLE);
        assert!(received[0].1.attached);
    }

    fn refusal_ack(reason: &ErrorCode) -> MessageAck {
        MessageAck {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            accepted: false,
            attached: false,
            reason: Some(reason.code().to_owned()),
        }
    }

    #[tokio::test]
    async fn spec_3_6_message_to_another_card_is_refused_and_not_queued() {
        let rig = rig().await;

        let ack = rig
            .link
            .client
            .send_message(
                rig.link.server_node.endpoint().id(),
                message_to(card_of(CLIENT_SEED), BODY),
            )
            .await
            .unwrap();

        assert_eq!(ack, refusal_ack(&ErrorCode::Refused));
        assert_eq!(rig.host.received().len(), 0);
    }

    #[tokio::test]
    async fn spec_3_9_message_is_refused_while_the_process_is_stopping() {
        let rig = rig().await;
        rig.host.stopping.store(true, Ordering::SeqCst);

        let ack = rig
            .link
            .client
            .send_message(
                rig.link.server_node.endpoint().id(),
                message_to(card_of(SERVER_SEED), BODY),
            )
            .await
            .unwrap();

        assert_eq!(ack, refusal_ack(&ErrorCode::Refused));
        assert_eq!(rig.host.received().len(), 0);
    }

    fn body_of_bytes(bytes: usize) -> String {
        "x".repeat(bytes)
    }

    #[tokio::test]
    async fn spec_3_6_body_one_byte_over_the_limit_is_too_large_and_the_limit_is_accepted() {
        let rig = rig().await;
        let server = rig.link.server_node.endpoint().id();

        let over = rig
            .link
            .client
            .send_message(
                server,
                message_to(
                    card_of(SERVER_SEED),
                    &body_of_bytes(MESSAGE_BODY_LIMIT_BYTES + 1),
                ),
            )
            .await
            .unwrap();
        let at_limit = rig
            .link
            .client
            .send_message(
                server,
                message_to(
                    card_of(SERVER_SEED),
                    &body_of_bytes(MESSAGE_BODY_LIMIT_BYTES),
                ),
            )
            .await
            .unwrap();

        assert_eq!(over, refusal_ack(&ErrorCode::TooLarge));
        assert!(at_limit.accepted);
        assert_eq!(rig.host.received().len(), 1);
    }

    #[tokio::test]
    async fn spec_3_6_wrong_recipient_is_refused_before_the_body_is_judged() {
        let rig = rig().await;

        let ack = rig
            .link
            .client
            .send_message(
                rig.link.server_node.endpoint().id(),
                message_to(
                    card_of(CLIENT_SEED),
                    &body_of_bytes(MESSAGE_BODY_LIMIT_BYTES + 1),
                ),
            )
            .await
            .unwrap();

        assert_eq!(ack, refusal_ack(&ErrorCode::Refused));
    }

    const OTHER_NETWORK_SECRET: [u8; KEY_LEN] = [8; KEY_LEN];
    const VERSION_BYTE_INDEX: usize = 1;
    const UNSUPPORTED_VERSION: u8 = 2;
    const SPEC_RESET_CODE_OVER_WIRE_LIMIT: u32 = 1;
    const READ_TIMEOUT_RESET_CODE_ON_THE_WIRE: u32 = 2;
    const UNANSWERED_RESET_CODE_ON_THE_WIRE: u32 = 3;
    const SPEC_SERVER_READ_TIMEOUT_EARLIEST: Duration = Duration::from_secs(10);
    const SPEC_SERVER_READ_TIMEOUT_LATEST: Duration = Duration::from_secs(11);

    async fn raw_connection(rig: &Rig) -> Connection {
        rig.link
            .client_node
            .endpoint()
            .connect(rig.link.server_node.endpoint().id(), DIRECT_ALPN)
            .await
            .unwrap()
    }

    async fn raw_exchange(
        connection: &Connection,
        request_bytes: &[u8],
    ) -> Result<Vec<u8>, ReadToEndError> {
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let (_write_outcome, response) = tokio::join!(
            async {
                let written = send.write_all(request_bytes).await;
                if written.is_ok() {
                    send.finish().unwrap();
                }
                written
            },
            recv.read_to_end(WIRE_LIMIT_BYTES)
        );
        response
    }

    fn reset_code_of(outcome: &Result<Vec<u8>, ReadToEndError>) -> Option<VarInt> {
        match outcome {
            Err(ReadToEndError::Read(ReadError::Reset(code))) => Some(*code),
            Err(ReadToEndError::Read(_) | ReadToEndError::TooLong) | Ok(_) => None,
        }
    }

    fn sealed_message_to_server(keys: &NetworkKeys) -> Vec<u8> {
        seal_request(
            keys,
            Request::Message(message_to(card_of(SERVER_SEED), BODY)),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn spec_3_15_test_9_message_with_a_wrong_mac_gets_no_response_and_is_not_queued() {
        let rig = rig().await;
        let connection = raw_connection(&rig).await;
        let other_keys = NetworkKeys::derive(&NetworkSecret::from_bytes(OTHER_NETWORK_SECRET));

        let outcome = raw_exchange(&connection, &sealed_message_to_server(&other_keys)).await;

        assert_eq!(
            reset_code_of(&outcome),
            Some(VarInt::from_u32(UNANSWERED_RESET_CODE_ON_THE_WIRE))
        );
        assert_eq!(rig.host.received().len(), 0);
    }

    #[tokio::test]
    async fn spec_3_6_message_of_an_unsupported_version_gets_no_response_and_is_not_queued() {
        let rig = rig().await;
        let connection = raw_connection(&rig).await;
        let mut request_bytes = sealed_message_to_server(&keys());
        request_bytes[VERSION_BYTE_INDEX] = UNSUPPORTED_VERSION;

        let outcome = raw_exchange(&connection, &request_bytes).await;

        assert_eq!(
            reset_code_of(&outcome),
            Some(VarInt::from_u32(UNANSWERED_RESET_CODE_ON_THE_WIRE))
        );
        assert_eq!(rig.host.received().len(), 0);
    }

    fn sealed_probe_and_introduce(keys: &NetworkKeys) -> [Vec<u8>; 2] {
        [
            Request::Probe(Probe {
                v: Version::CURRENT,
                nonce: PROBE_NONCE,
                mac: ZERO_MAC,
            }),
            Request::Introduce(introduction_from_client()),
        ]
        .map(|request| seal_request(keys, request).unwrap())
    }

    #[tokio::test]
    async fn spec_3_6_probe_or_introduce_with_a_wrong_mac_or_an_unsupported_version_gets_no_response()
     {
        let rig = rig().await;
        let connection = raw_connection(&rig).await;
        let other_keys = NetworkKeys::derive(&NetworkSecret::from_bytes(OTHER_NETWORK_SECRET));
        let under_another_secret = sealed_probe_and_introduce(&other_keys);
        let of_an_unsupported_version =
            sealed_probe_and_introduce(&keys()).map(|mut request_bytes| {
                request_bytes[VERSION_BYTE_INDEX] = UNSUPPORTED_VERSION;
                request_bytes
            });

        for request_bytes in under_another_secret
            .into_iter()
            .chain(of_an_unsupported_version)
        {
            let outcome = raw_exchange(&connection, &request_bytes).await;

            assert_eq!(
                reset_code_of(&outcome),
                Some(VarInt::from_u32(UNANSWERED_RESET_CODE_ON_THE_WIRE))
            );
        }
        assert_eq!(rig.host.introduced.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn spec_3_6_stream_past_the_wire_limit_is_reset_with_code_1() {
        let rig = rig().await;
        let connection = raw_connection(&rig).await;

        let outcome = raw_exchange(&connection, &vec![0; WIRE_LIMIT_BYTES + 1]).await;

        assert_eq!(
            reset_code_of(&outcome),
            Some(VarInt::from_u32(SPEC_RESET_CODE_OVER_WIRE_LIMIT))
        );
        assert_eq!(rig.host.received().len(), 0);
    }

    #[tokio::test]
    async fn spec_3_14_server_read_timeout_resets_that_stream_and_keeps_the_connection() {
        let rig = rig().await;
        let connection = raw_connection(&rig).await;
        let (mut silent_send, mut silent_recv) = connection.open_bi().await.unwrap();
        silent_send.write_all(&[0]).await.unwrap();

        let started = Instant::now();
        let outcome = silent_recv.read_to_end(WIRE_LIMIT_BYTES).await;
        let waited = started.elapsed();

        assert_eq!(
            reset_code_of(&Err(outcome.unwrap_err())),
            Some(VarInt::from_u32(READ_TIMEOUT_RESET_CODE_ON_THE_WIRE))
        );
        assert!(
            SPEC_SERVER_READ_TIMEOUT_EARLIEST <= waited && waited < SPEC_SERVER_READ_TIMEOUT_LATEST
        );
        assert_eq!(connection.close_reason(), None);
        let next = raw_exchange(&connection, &sealed_message_to_server(&keys()))
            .await
            .unwrap();
        assert!(matches!(
            open_response(&keys(), &next).unwrap(),
            Response::MessageAck(MessageAck { accepted: true, .. })
        ));
    }

    const PROBE_NONCE: Nonce = [
        0x51, 0x62, 0x73, 0x84, 0x95, 0xA6, 0xB7, 0xC8, 0xD9, 0xEA, 0xFB, 0x0C, 0x1D, 0x2E, 0x3F,
        0x40,
    ];
    const PROBE_NONCE_REVERSED: Nonce = [
        0x40, 0x3F, 0x2E, 0x1D, 0x0C, 0xFB, 0xEA, 0xD9, 0xC8, 0xB7, 0xA6, 0x95, 0x84, 0x73, 0x62,
        0x51,
    ];
    const SENDER_DIRECT_ADDRESS: &str = "127.0.0.1:4000";
    const LOOPBACK_ANY_PORT: &str = "127.0.0.1:0";
    const SPEC_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
    const SPEC_ACK_TIMEOUT: Duration = Duration::from_secs(10);
    const SPEC_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    const TIMEOUT_SLACK: Duration = Duration::from_secs(1);
    const SHORT_TIMEOUT: Duration = Duration::from_millis(300);

    #[tokio::test]
    async fn observability_decision_9_a_message_ack_carries_the_attached_state_its_delivery_read_though_the_client_left_after()
     {
        let rig = rig_over(
            RecordingHost {
                detaches_on_delivery: true,
                ..RecordingHost::with_card(card_of(SERVER_SEED))
            },
            Arc::new(tokio::time::Instant::now),
        )
        .await;
        rig.host.attached.store(true, Ordering::SeqCst);

        let ack = rig
            .link
            .client
            .send_message(
                rig.link.server_node.endpoint().id(),
                message_to(card_of(SERVER_SEED), BODY),
            )
            .await
            .unwrap();

        assert!(ack.accepted);
        assert!(ack.attached);
        assert!(!rig.host.is_attached());
    }

    #[tokio::test]
    async fn spec_3_6_probe_is_answered_with_its_nonce_and_the_attached_state() {
        let rig = rig().await;
        rig.host.attached.store(true, Ordering::SeqCst);

        let ack = rig
            .link
            .client
            .probe(rig.link.server_node.endpoint().id(), PROBE_NONCE)
            .await
            .unwrap();

        assert_eq!(ack.v, Version::CURRENT);
        assert_eq!(ack.nonce, PROBE_NONCE);
        assert!(ack.attached);
        assert_ne!(ack.mac, ZERO_MAC);
    }

    fn introduction_from_client() -> wire::Introduce {
        wire::Introduce {
            v: Version::CURRENT,
            handle: Some(SENDER_HANDLE.to_owned()),
            public: b"{}".to_vec(),
            attached: true,
            addr: wire::Addr::from_endpoint_report([], [SENDER_DIRECT_ADDRESS.parse().unwrap()]),
            mac: ZERO_MAC,
        }
    }

    const SILENCE_OF_A_GONE_PROCESS_JUDGED: Duration =
        GONE_PROCESS_SILENCE.saturating_add(HEARING_SAMPLE_INTERVAL);

    #[tokio::test]
    async fn spec_3_8_an_introduce_keeps_every_live_connection_accepted_from_its_sender_whatever_address_it_carries()
     {
        let rig = rig().await;
        let server = rig.link.server_node.endpoint().id();
        let earlier = raw_connection(&rig).await;
        raw_exchange(&earlier, &sealed_message_to_server(&keys()))
            .await
            .unwrap();
        tokio::time::sleep(SILENCE_OF_A_GONE_PROCESS_JUDGED).await;

        rig.link
            .client
            .introduce(server, introduction_from_client())
            .await
            .unwrap();
        let answered_on_the_earlier_connection =
            raw_exchange(&earlier, &sealed_message_to_server(&keys()))
                .await
                .map(|response| open_response(&keys(), &response).unwrap());

        assert!(
            matches!(
                answered_on_the_earlier_connection,
                Ok(Response::MessageAck(MessageAck { accepted: true, .. }))
            ),
            "{answered_on_the_earlier_connection:?}"
        );
    }

    /// Dials the server under the client's card from a process of its own, answers one Message there, and ends that process without closing its connection.
    async fn connection_from_a_process_that_vanished(server: iroh::EndpointAddr) {
        tokio::task::spawn_blocking(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let vanished = runtime.block_on(async {
                let node = bind_node(CLIENT_SEED).await;
                let server_id = server.id;
                node.replace_address(server);
                let connection = node
                    .endpoint()
                    .connect(server_id, DIRECT_ALPN)
                    .await
                    .unwrap();
                raw_exchange(&connection, &sealed_message_to_server(&keys()))
                    .await
                    .unwrap();
                (node, connection)
            });
            drop(runtime);
            drop(vanished);
        })
        .await
        .unwrap();
    }

    /// How many connections from the card the endpoint's register holds, whether or not any is still alive.
    fn connections_held_from(endpoint: &NodeEndpoint, card: wire::PublicKey) -> usize {
        endpoint
            .held_connections()
            .held()
            .get(&card)
            .map_or(0, Vec::len)
    }

    fn connections_held_open_from(
        endpoint: &NodeEndpoint,
        card: wire::PublicKey,
    ) -> Vec<Connection> {
        endpoint
            .held_connections()
            .held()
            .get(&card)
            .into_iter()
            .flatten()
            .filter_map(|accepted| accepted.connection.upgrade())
            .collect()
    }

    #[tokio::test]
    async fn spec_3_8_an_introduce_closes_the_connection_accepted_from_its_senders_process_that_is_gone()
     {
        let rig = rig().await;
        let server = rig.link.server_node.endpoint().id();
        connection_from_a_process_that_vanished(rig.link.server_node.endpoint().addr()).await;
        let vanished = connections_held_open_from(&rig.link.server_node, card_of(CLIENT_SEED));
        tokio::time::sleep(SILENCE_OF_A_GONE_PROCESS_JUDGED).await;

        rig.link
            .client
            .introduce(server, introduction_from_client())
            .await
            .unwrap();
        let probed_on_the_kept_connection = rig.link.client.probe(server, PROBE_NONCE).await;

        assert_eq!(vanished.len(), 1);
        assert!(
            vanished
                .iter()
                .all(|connection| connection.close_reason() == Some(ConnectionError::LocallyClosed)),
            "{:?}",
            vanished.iter().map(Connection::close_reason).collect::<Vec<_>>()
        );
        assert_eq!(probed_on_the_kept_connection.unwrap().nonce, PROBE_NONCE);
    }

    const SAMPLERS_DONE_WAIT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn spec_3_8_an_accept_a_router_shutdown_cuts_short_holds_its_connection_no_longer_than_the_connection_lives()
     {
        let host = RecordingHost::with_card(card_of(SERVER_SEED));
        let server_node = bind_node(SERVER_SEED).await;
        let client_node = bind_node(CLIENT_SEED).await;
        client_node.replace_address(server_node.endpoint().addr());
        let server = Arc::new(DirectServer::new(keys(), host, &server_node));
        let (dialed, accepted) = tokio::join!(
            client_node
                .endpoint()
                .connect(server_node.endpoint().id(), DIRECT_ALPN),
            async {
                let incoming = server_node.endpoint().accept().await.unwrap();
                incoming.accept().unwrap().await.unwrap()
            }
        );
        let accepted_closed = accepted.weak_handle().closed();
        let serving = tokio::spawn({
            let server = Arc::clone(&server);
            async move { server.accept(accepted).await }
        });
        let dialed = dialed.unwrap();
        raw_exchange(&dialed, &sealed_message_to_server(&keys()))
            .await
            .unwrap();
        let held_while_serving = connections_held_from(&server_node, card_of(CLIENT_SEED));

        serving.abort();
        drop(serving.await);
        dialed.close(VarInt::from_u32(0), b"");
        let all_forgotten = timeout(SAMPLERS_DONE_WAIT, async {
            accepted_closed.await;
            while connections_held_from(&server_node, card_of(CLIENT_SEED)) > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;

        assert_eq!(held_while_serving, 1);
        assert!(
            all_forgotten.is_ok(),
            "the register still holds {} connection(s) from the client within {SAMPLERS_DONE_WAIT:?} after the accept was cut short and the connection closed",
            connections_held_from(&server_node, card_of(CLIENT_SEED))
        );
    }

    /// A clock that moves a gone process's silence forward at every reading, so every held connection reads as gone when judged.
    fn clock_reading_every_connection_as_gone() -> MonotonicClock {
        let started = tokio::time::Instant::now();
        let readings = Arc::new(AtomicU32::new(0));
        Arc::new(move || {
            started
                .checked_add(
                    GONE_PROCESS_SILENCE.saturating_mul(readings.fetch_add(1, Ordering::SeqCst)),
                )
                .unwrap()
        })
    }

    #[tokio::test]
    async fn spec_3_8_closing_the_connections_of_a_gone_card_closes_the_gossip_connection_this_endpoint_dialed_to_it()
     {
        let register = HeldConnections::new(
            clock_reading_every_connection_as_gone(),
            dropping_transport(),
        );
        let this_node = bind_node_holding(SERVER_SEED, register.clone()).await;
        let gone_node = bind_node(CLIENT_SEED).await;
        this_node.replace_address(gone_node.endpoint().addr());
        let (dialed, accepted) = tokio::join!(
            this_node
                .endpoint()
                .connect(gone_node.endpoint().id(), GOSSIP_ALPN),
            async {
                let incoming = gone_node.endpoint().accept().await.unwrap();
                incoming.accept().unwrap().await
            }
        );
        let dialed = dialed.unwrap();
        let _accepted = accepted.unwrap();

        register.close_silent_of(card_of(CLIENT_SEED));

        assert_eq!(dialed.close_reason(), Some(ConnectionError::LocallyClosed));
    }

    const MISSED_COUNT_CASES: u64 = 500;
    const MISSED_COUNT_SPREAD: u64 = 0x9E37_79B9_7F4A_7C15;
    const RECORDED_EVENT_WITHIN: Duration = Duration::from_secs(10);
    const SALT_BYTE: u8 = 0x5a;
    const SPEC_SUPERSEDED_CLOSE_CODE: u64 = 410;

    /// The name and fields of one recorded event.
    pub type RecordedEvent = (&'static str, crate::event::EventFields);

    /// A run record that hands the name and fields of each event to the receiver.
    pub fn recorded_events() -> (
        crate::event::RunRecord,
        tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let record: crate::event::RunRecord = Arc::new(move |event: crate::event::Event| {
            drop(sender.send((event.name(), event.fields())));
        });
        (record, receiver)
    }

    /// Every event the run record has handed the receiver so far, in order.
    pub fn recorded_so_far(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
    ) -> Vec<RecordedEvent> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    /// The longest a test waits for a counters snapshot: two of the 60 s between snapshots of `spec/observability.md` decision 9, so a start that moved the paused clock still meets the first.
    const COUNTERS_SNAPSHOT_WITHIN: Duration = Duration::from_secs(120);

    /// The instant on the test's clock at which the next `state.counters` event reached the receiver, with its fields, failing by name when none comes within the wait.
    pub async fn next_counters_snapshot(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
    ) -> (tokio::time::Instant, crate::event::EventFields) {
        tokio::time::timeout(COUNTERS_SNAPSHOT_WITHIN, async {
            loop {
                let (name, fields) = receiver.recv().await.unwrap();
                if name == "state.counters" {
                    return (tokio::time::Instant::now(), fields);
                }
            }
        })
        .await
        .unwrap_or_else(|_elapsed| panic!("no state.counters within {COUNTERS_SNAPSHOT_WITHIN:?}"))
    }

    /// The salt every test register hashes locations under.
    pub fn test_salt() -> crate::trace::LocationSalt {
        crate::trace::LocationSalt::from_random_bytes(
            [SALT_BYTE; crate::trace::LOCATION_SALT_BYTES],
        )
    }

    /// A transport record whose sink drops every event, hashing under the test salt.
    pub fn dropping_transport() -> TransportRecord {
        TransportRecord::new(Arc::new(|_event: Event| {}), test_salt())
    }

    /// A register on the runtime's monotonic clock whose record drops every event.
    pub fn unrecorded_register() -> HeldConnections {
        HeldConnections::new(Arc::new(tokio::time::Instant::now), dropping_transport())
    }

    /// Waits for the next recorded event of the name, within the bound, failing with the name and the events seen.
    pub async fn next_recorded(
        receiver: &mut tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
        name: &str,
    ) -> crate::event::EventFields {
        let mut seen = Vec::new();
        let found = timeout(RECORDED_EVENT_WITHIN, async {
            loop {
                let (recorded_name, fields) = receiver.recv().await.unwrap();
                if recorded_name == name {
                    return fields;
                }
                seen.push(recorded_name);
            }
        })
        .await;
        found.unwrap_or_else(|_elapsed| panic!("no {name} within the bound; seen {seen:?}"))
    }

    /// Asserts that the map gives each case's input the case's output, and that its outputs hold as many distinct values as named.
    pub fn assert_each_arm<Input, Output: PartialEq + fmt::Debug>(
        cases: &[(Input, Output)],
        map: impl Fn(&Input) -> Output,
        distinct_outputs: usize,
    ) {
        let written: Vec<Output> = cases.iter().map(|(input, _)| map(input)).collect();
        let expected: Vec<&Output> = cases.iter().map(|(_, output)| output).collect();
        assert_eq!(written.iter().collect::<Vec<_>>(), expected);
        let distinct = written
            .iter()
            .enumerate()
            .filter(|(index, output)| !written[..*index].contains(output))
            .count();
        assert_eq!(
            distinct, distinct_outputs,
            "distinct outputs of {written:?}"
        );
    }

    /// The value of the field of the name among the fields.
    pub fn field<'a>(
        fields: &'a crate::event::EventFields,
        name: &str,
    ) -> Option<&'a serde_json::Value> {
        fields
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
    }

    const LONGEST_DRAWN_TEXT: u64 = 24;
    const SIDES_TO_PAD: u64 = 2;
    const BYTE_VALUES: u64 = 256;
    const ASCII_WHITESPACE: &[u8] = b" \t\n\x0c\r";
    const FIRST_SURROGATE: u32 = 0xD800;
    const SURROGATES: u32 = 0x800;
    const SCALAR_VALUES: u32 = 0x11_0000 - SURROGATES;
    pub const CATCH_ALL_CASES: u64 = 2000;

    /// An index below the length, drawn from the sequence.
    fn drawn_index(draws: &mut Draws, length: usize) -> usize {
        usize::try_from(draws.below(u64::try_from(length).unwrap())).unwrap()
    }

    /// One unit of an input a property test draws: a byte or a character.
    pub trait DrawnUnit: Copy {
        /// Any unit, drawn from the sequence.
        fn drawn(draws: &mut Draws) -> Self;
        /// An ASCII whitespace unit, drawn from the sequence.
        fn drawn_whitespace(draws: &mut Draws) -> Self;
        /// The unit with an ASCII letter's case swapped, any other unit unchanged.
        fn case_swapped(self) -> Self;
    }

    impl DrawnUnit for u8 {
        fn drawn(draws: &mut Draws) -> Self {
            Self::try_from(draws.below(BYTE_VALUES)).unwrap()
        }

        fn drawn_whitespace(draws: &mut Draws) -> Self {
            ASCII_WHITESPACE[drawn_index(draws, ASCII_WHITESPACE.len())]
        }

        fn case_swapped(self) -> Self {
            if self.is_ascii_lowercase() {
                self.to_ascii_uppercase()
            } else {
                self.to_ascii_lowercase()
            }
        }
    }

    impl DrawnUnit for char {
        fn drawn(draws: &mut Draws) -> Self {
            let below_the_gap = u32::try_from(draws.below(u64::from(SCALAR_VALUES))).unwrap();
            let scalar = if below_the_gap < FIRST_SURROGATE {
                below_the_gap
            } else {
                below_the_gap.saturating_add(SURROGATES)
            };
            Self::from_u32(scalar).unwrap()
        }

        fn drawn_whitespace(draws: &mut Draws) -> Self {
            Self::from(u8::drawn_whitespace(draws))
        }

        fn case_swapped(self) -> Self {
            if self.is_ascii_lowercase() {
                self.to_ascii_uppercase()
            } else {
                self.to_ascii_lowercase()
            }
        }
    }

    /// One way to draw units beside the named units.
    type WayBeside<Unit> = fn(Vec<Unit>, &mut Draws) -> Vec<Unit>;

    /// Every way to draw beside the named units, so the count of ways is the count of arms.
    fn ways_beside<Unit: DrawnUnit>() -> [WayBeside<Unit>; 7] {
        [
            lengthened_by_a_unit,
            shortened_by_a_unit,
            with_one_unit_changed,
            with_case_swapped,
            padded_with_whitespace,
            with_a_unit_before,
            any_units,
        ]
    }

    /// The named units with a drawn unit after them.
    fn lengthened_by_a_unit<Unit: DrawnUnit>(mut named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
        named.push(Unit::drawn(draws));
        named
    }

    /// The named units without their last.
    fn shortened_by_a_unit<Unit: DrawnUnit>(mut named: Vec<Unit>, _draws: &mut Draws) -> Vec<Unit> {
        named.truncate(named.len().saturating_sub(1));
        named
    }

    /// The named units with one of them replaced by a drawn unit.
    fn with_one_unit_changed<Unit: DrawnUnit>(
        mut named: Vec<Unit>,
        draws: &mut Draws,
    ) -> Vec<Unit> {
        let position = drawn_index(draws, named.len());
        named[position] = Unit::drawn(draws);
        named
    }

    /// The named units with every ASCII letter's case swapped.
    fn with_case_swapped<Unit: DrawnUnit>(named: Vec<Unit>, _draws: &mut Draws) -> Vec<Unit> {
        named.into_iter().map(Unit::case_swapped).collect()
    }

    /// The named units with a drawn whitespace unit before or after them.
    fn padded_with_whitespace<Unit: DrawnUnit>(
        mut named: Vec<Unit>,
        draws: &mut Draws,
    ) -> Vec<Unit> {
        let whitespace = Unit::drawn_whitespace(draws);
        if draws.below(SIDES_TO_PAD) == 0 {
            named.insert(0, whitespace);
        } else {
            named.push(whitespace);
        }
        named
    }

    /// The named units with a drawn unit before them.
    fn with_a_unit_before<Unit: DrawnUnit>(mut named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
        named.insert(0, Unit::drawn(draws));
        named
    }

    /// Any drawn units, the named ones aside.
    fn any_units<Unit: DrawnUnit>(_named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
        (0..draws.below(LONGEST_DRAWN_TEXT))
            .map(|_| Unit::drawn(draws))
            .collect()
    }

    /// The units drawn beside the named units by one of the ways beside, drawn alike often.
    fn drawn_beside<Unit: DrawnUnit>(named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
        let ways = ways_beside::<Unit>();
        ways[drawn_index(draws, ways.len())](named, draws)
    }

    /// Bytes drawn beside one of the named byte strings, over all 256 byte values.
    pub fn bytes_beside(named: &[&[u8]], draws: &mut Draws) -> Vec<u8> {
        let picked = named[drawn_index(draws, named.len())].to_vec();
        drawn_beside(picked, draws)
    }

    /// Text drawn beside one of the named strings, over every character.
    fn text_beside(named: &[&str], draws: &mut Draws) -> String {
        let picked = named[drawn_index(draws, named.len())].chars().collect();
        drawn_beside(picked, draws).into_iter().collect()
    }

    #[tokio::test]
    async fn observability_decision_9_a_connection_is_recorded_opened_by_direction_and_protocol_closing_with_estafettes_code_and_reason_and_closed_on_both_sides()
     {
        let (this_record, mut this_side) = recorded_events();
        let (peer_record, mut peer_side) = recorded_events();
        let register = HeldConnections::new(
            clock_reading_every_connection_as_gone(),
            super::super::endpoint::TransportRecord::new(this_record, test_salt()),
        );
        let this_node = bind_node_holding(SERVER_SEED, register.clone()).await;
        let peer_node = bind_node_holding(
            CLIENT_SEED,
            HeldConnections::new(
                Arc::new(tokio::time::Instant::now),
                super::super::endpoint::TransportRecord::new(peer_record, test_salt()),
            ),
        )
        .await;
        this_node.replace_address(peer_node.endpoint().addr());
        let router = Router::builder(peer_node.endpoint().clone())
            .accept(
                DIRECT_ALPN,
                DirectServer::new(
                    keys(),
                    RecordingHost::with_card(card_of(CLIENT_SEED)),
                    &peer_node,
                ),
            )
            .spawn();
        let dialed = this_node
            .endpoint()
            .connect(peer_node.endpoint().id(), DIRECT_ALPN)
            .await
            .unwrap();
        let peer_short = data_encoding::HEXLOWER.encode(&card_of(CLIENT_SEED)[..5]);
        let this_short = data_encoding::HEXLOWER.encode(&card_of(SERVER_SEED)[..5]);

        let opened = next_recorded(&mut this_side, "transport.connection_opened").await;
        let accepted = next_recorded(&mut peer_side, "transport.connection_opened").await;
        register.close_silent_of(card_of(CLIENT_SEED));
        let closing = next_recorded(&mut this_side, "transport.connection_closing").await;
        let closed_on_this_side =
            next_recorded(&mut this_side, "transport.connection_closed").await;
        let closed_on_the_peer = next_recorded(&mut peer_side, "transport.connection_closed").await;

        let connection = serde_json::Value::from(dialed.stable_id());
        assert_eq!(
            opened,
            vec![
                ("connection", connection.clone()),
                ("peer", serde_json::Value::from(peer_short.clone())),
                ("direction", serde_json::Value::from("dialed")),
                ("protocol", serde_json::Value::from("direct")),
            ]
        );
        assert_eq!(
            field(&accepted, "peer"),
            Some(&serde_json::Value::from(this_short))
        );
        assert_eq!(
            field(&accepted, "direction"),
            Some(&serde_json::Value::from("accepted"))
        );
        assert_eq!(
            closing,
            vec![
                ("connection", connection.clone()),
                ("peer", serde_json::Value::from(peer_short)),
                ("code", serde_json::Value::from(SPEC_SUPERSEDED_CLOSE_CODE)),
                ("reason", serde_json::Value::from("superseded")),
            ]
        );
        assert_eq!(
            closed_on_this_side,
            vec![
                ("connection", connection),
                ("side", serde_json::Value::from("this_side"))
            ]
        );
        assert_eq!(
            field(&closed_on_the_peer, "side"),
            Some(&serde_json::Value::from("peer"))
        );
        assert_eq!(
            field(&closed_on_the_peer, "code"),
            Some(&serde_json::Value::from(SPEC_SUPERSEDED_CLOSE_CODE))
        );
        assert_eq!(
            field(&closed_on_the_peer, "reason"),
            Some(&serde_json::Value::from("superseded"))
        );
        drop(router);
    }

    #[tokio::test]
    async fn observability_decision_9_and_6_a_paths_opening_and_selection_carry_its_kind_port_salted_hash_and_round_trip_never_its_address()
     {
        let (this_record, mut this_side) = recorded_events();
        let this_node = bind_node_holding(
            SERVER_SEED,
            HeldConnections::new(
                Arc::new(tokio::time::Instant::now),
                super::super::endpoint::TransportRecord::new(this_record, test_salt()),
            ),
        )
        .await;
        let peer_node = bind_node(CLIENT_SEED).await;
        this_node.replace_address(peer_node.endpoint().addr());
        let router = Router::builder(peer_node.endpoint().clone())
            .accept(
                DIRECT_ALPN,
                DirectServer::new(
                    keys(),
                    RecordingHost::with_card(card_of(CLIENT_SEED)),
                    &peer_node,
                ),
            )
            .spawn();
        let dialed = this_node
            .endpoint()
            .connect(peer_node.endpoint().id(), DIRECT_ALPN)
            .await
            .unwrap();

        let opened = next_recorded(&mut this_side, "transport.path_opened").await;
        let selected = next_recorded(&mut this_side, "transport.path_selected").await;

        let selected_path = dialed
            .paths()
            .iter()
            .find(iroh::endpoint::Path::is_selected)
            .map(|path| path.remote_addr().clone())
            .unwrap();
        let iroh::TransportAddr::Ip(address) = selected_path else {
            panic!("a path between two local endpoints is direct: {selected_path:?}");
        };
        let expected_hash =
            super::super::endpoint::LocationHash::of(&test_salt(), &address.to_string()).hex();
        for fields in [&opened, &selected] {
            assert_eq!(
                field(fields, "connection"),
                Some(&serde_json::Value::from(dialed.stable_id()))
            );
            assert_eq!(
                field(fields, "kind"),
                Some(&serde_json::Value::from("direct"))
            );
            assert_eq!(
                field(fields, "port"),
                Some(&serde_json::Value::from(address.port()))
            );
            assert!(
                field(fields, "rtt_ms").is_some_and(serde_json::Value::is_u64),
                "{fields:?}"
            );
            let rendered = serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), value.clone()))
                    .collect(),
            )
            .to_string();
            assert!(!rendered.contains(&address.ip().to_string()), "{rendered}");
        }
        assert_eq!(
            field(&selected, "location"),
            Some(&serde_json::Value::from(expected_hash))
        );
        drop(router);
    }

    const SERVER_HANDLING_MS: u64 = 1234;
    const MESSAGE_ID_TEXT: &str = "10213243-5465-7687-98a9-bacbdcedfe0f";

    /// A host that holds its card and takes `SERVER_HANDLING_MS` on the shared clock to receive each message.
    #[derive(Clone)]
    struct SlowHost {
        card: wire::PublicKey,
        clock: Arc<Mutex<tokio::time::Instant>>,
    }

    impl DirectHost for SlowHost {
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
            let mut now = self.clock.lock().unwrap();
            *now = now
                .checked_add(Duration::from_millis(SERVER_HANDLING_MS))
                .unwrap();
            false
        }

        fn receive_introduce(
            &self,
            _sender: wire::PublicKey,
            _introduce: wire::Introduce,
        ) -> IntroduceAck {
            own_introduction(false)
        }
    }

    /// A recording client linked to a recording server over a slow host, both registers on one clock the host moves.
    struct RecordingLink {
        client: DirectClient,
        server_id: EndpointId,
        sent: tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
        served: tokio::sync::mpsc::UnboundedReceiver<RecordedEvent>,
        _router: Router,
    }

    async fn recording_link_over_a_slow_host() -> RecordingLink {
        let shared_clock = Arc::new(Mutex::new(tokio::time::Instant::now()));
        let reading = Arc::clone(&shared_clock);
        let clock: MonotonicClock = Arc::new(move || *reading.lock().unwrap());
        let (client_record, sent) = recorded_events();
        let (server_record, served) = recorded_events();
        let server_node = bind_node_holding(
            SERVER_SEED,
            HeldConnections::new(
                Arc::clone(&clock),
                super::super::endpoint::TransportRecord::new(server_record, test_salt()),
            ),
        )
        .await;
        let client_register = HeldConnections::new(
            clock,
            super::super::endpoint::TransportRecord::new(client_record, test_salt()),
        );
        let client_node = bind_node_holding(CLIENT_SEED, client_register.clone()).await;
        let router = Router::builder(server_node.endpoint().clone())
            .accept(
                DIRECT_ALPN,
                DirectServer::new(
                    keys(),
                    SlowHost {
                        card: card_of(SERVER_SEED),
                        clock: shared_clock,
                    },
                    &server_node,
                ),
            )
            .spawn();
        client_node.replace_address(server_node.endpoint().addr());
        let client = DirectClient::new(
            client_node.endpoint().clone(),
            keys(),
            ClientTimeouts::SPEC,
            client_register,
        );
        RecordingLink {
            client,
            server_id: server_node.endpoint().id(),
            sent,
            served,
            _router: router,
        }
    }

    #[cfg(feature = "relay")]
    #[tokio::test]
    async fn observability_decision_9_and_6_a_relay_path_that_iroh_closes_is_recorded_closed_with_its_kind_port_and_salted_hash()
     {
        use iroh_relay::server::{RelayConfig, Server, ServerConfig};

        let mut relay_config = ServerConfig::default();
        relay_config.relay = Some(RelayConfig::new(std::net::SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            0,
        ))));
        let relay = Server::spawn(relay_config).await.unwrap();
        let relay_address: iroh::RelayUrl = format!("http://{}/", relay.http_addr().unwrap())
            .parse()
            .unwrap();
        let (this_record, mut this_side) = recorded_events();
        let this_node = NodeEndpoint::bind(
            SecretKey::from_bytes(&[SERVER_SEED; KEY_LEN]),
            *keys().gate_key(),
            vec![relay_address.clone()],
            |nonce: &mut [u8; KEY_LEN]| *nonce = [0; KEY_LEN],
            |_reason| {},
            HeldConnections::new(
                Arc::new(tokio::time::Instant::now),
                super::super::endpoint::TransportRecord::new(this_record, test_salt()),
            ),
        )
        .await
        .unwrap();
        let peer_node = NodeEndpoint::bind(
            SecretKey::from_bytes(&[CLIENT_SEED; KEY_LEN]),
            *keys().gate_key(),
            vec![relay_address.clone()],
            |nonce: &mut [u8; KEY_LEN]| *nonce = [0; KEY_LEN],
            |_reason| {},
            unrecorded_register(),
        )
        .await
        .unwrap();
        this_node.replace_address(iroh::EndpointAddr::from_parts(
            peer_node.endpoint().id(),
            [iroh::TransportAddr::Relay(relay_address.clone())],
        ));
        let router = Router::builder(peer_node.endpoint().clone())
            .accept(
                DIRECT_ALPN,
                DirectServer::new(
                    keys(),
                    RecordingHost::with_card(card_of(CLIENT_SEED)),
                    &peer_node,
                ),
            )
            .spawn();
        let dialed = this_node
            .endpoint()
            .connect(peer_node.endpoint().id(), DIRECT_ALPN)
            .await
            .unwrap();
        let relay_location = vec![
            ("kind", serde_json::Value::from("relay")),
            (
                "port",
                serde_json::Value::from(relay_address.port().unwrap()),
            ),
            (
                "location",
                serde_json::Value::from(
                    super::super::endpoint::LocationHash::of(&test_salt(), relay_address.as_str())
                        .hex(),
                ),
            ),
        ];

        relay.shutdown().await.unwrap();
        let closed = next_recorded(&mut this_side, "transport.path_closed").await;

        assert_eq!(
            field(&closed, "connection"),
            Some(&serde_json::Value::from(dialed.stable_id()))
        );
        for (name, value) in &relay_location {
            assert_eq!(field(&closed, name), Some(value), "{closed:?}");
        }
        drop(router);
    }

    #[tokio::test]
    async fn observability_decision_9_a_connections_lost_path_events_are_recorded_with_the_count_iroh_reports()
     {
        let rig = rig().await;
        let dialed = rig
            .link
            .client_node
            .endpoint()
            .connect(rig.link.server_node.endpoint().id(), DIRECT_ALPN)
            .await
            .unwrap();
        let connection = ConnectionId::of(&dialed);
        for case in 0..MISSED_COUNT_CASES {
            let missed = match case {
                0 => 0,
                1 => 1,
                2 => u64::MAX,
                _ => case.wrapping_mul(MISSED_COUNT_SPREAD),
            };
            let event = crate::event::Event::from(path_events_missed(connection, missed));

            assert_eq!(event.name(), "transport.path_events_missed", "case {case}");
            assert_eq!(
                event.fields(),
                vec![
                    ("connection", serde_json::Value::from(dialed.stable_id())),
                    ("missed", serde_json::Value::from(missed)),
                ],
                "case {case}"
            );
        }
    }

    #[tokio::test]
    async fn observability_decision_9_a_message_is_recorded_sent_and_served_with_its_id_connection_outcome_and_elapsed_time_on_both_ends()
     {
        let RecordingLink {
            client,
            server_id,
            mut sent,
            mut served,
            _router: router,
        } = recording_link_over_a_slow_host().await;

        client
            .send_message(server_id, message_to(card_of(SERVER_SEED), BODY))
            .await
            .unwrap();
        let first_sent = next_recorded(&mut sent, "direct.request_sent").await;
        let first_served = next_recorded(&mut served, "direct.request_served").await;
        client
            .send_message(server_id, message_to(card_of(CLIENT_SEED), BODY))
            .await
            .unwrap();
        let refused_sent = next_recorded(&mut sent, "direct.request_sent").await;
        let refused_served = next_recorded(&mut served, "direct.request_served").await;

        let connection = field(&first_sent, "connection").cloned().unwrap();
        let server_short = data_encoding::HEXLOWER.encode(&card_of(SERVER_SEED)[..5]);
        let client_short = data_encoding::HEXLOWER.encode(&card_of(CLIENT_SEED)[..5]);
        assert_eq!(
            first_sent,
            vec![
                ("kind", serde_json::Value::from("message")),
                ("message", serde_json::Value::from(MESSAGE_ID_TEXT)),
                ("peer", serde_json::Value::from(server_short.clone())),
                ("connection", connection.clone()),
                ("connection_source", serde_json::Value::from("dialed")),
                ("outcome", serde_json::Value::from("acknowledged")),
                ("elapsed_ms", serde_json::Value::from(SERVER_HANDLING_MS)),
            ]
        );
        assert_eq!(
            field(&first_served, "kind"),
            Some(&serde_json::Value::from("message"))
        );
        assert_eq!(
            field(&first_served, "message"),
            Some(&serde_json::Value::from(MESSAGE_ID_TEXT))
        );
        assert_eq!(
            field(&first_served, "peer"),
            Some(&serde_json::Value::from(client_short))
        );
        assert_eq!(
            field(&first_served, "outcome"),
            Some(&serde_json::Value::from("answered"))
        );
        assert_eq!(
            field(&first_served, "elapsed_ms"),
            Some(&serde_json::Value::from(SERVER_HANDLING_MS))
        );
        assert_eq!(
            refused_sent,
            vec![
                ("kind", serde_json::Value::from("message")),
                ("message", serde_json::Value::from(MESSAGE_ID_TEXT)),
                ("peer", serde_json::Value::from(server_short)),
                ("connection", connection),
                ("connection_source", serde_json::Value::from("held")),
                ("outcome", serde_json::Value::from("refused")),
                ("code", serde_json::Value::from("refused")),
                ("elapsed_ms", serde_json::Value::from(0)),
            ]
        );
        assert_eq!(
            field(&refused_served, "outcome"),
            Some(&serde_json::Value::from("refused"))
        );
        assert_eq!(
            field(&refused_served, "code"),
            Some(&serde_json::Value::from("refused"))
        );
        assert_eq!(
            field(&refused_served, "connection"),
            field(&first_served, "connection")
        );
        drop(router);
    }

    #[tokio::test]
    async fn observability_decision_9_a_probe_to_an_unreachable_peer_is_recorded_failed_with_no_connection()
     {
        let (client_record, mut sent) = recorded_events();
        let client_register = HeldConnections::new(
            Arc::new(tokio::time::Instant::now),
            super::super::endpoint::TransportRecord::new(client_record, test_salt()),
        );
        let client_node = bind_node_holding(CLIENT_SEED, client_register.clone()).await;
        let client = DirectClient::new(
            client_node.endpoint().clone(),
            keys(),
            ClientTimeouts::SPEC,
            client_register,
        );
        let unknown = SecretKey::from_bytes(&[SERVER_SEED; KEY_LEN]).public();

        let probed = client.probe(unknown, Nonce::default()).await;
        let recorded = next_recorded(&mut sent, "direct.request_sent").await;

        assert_eq!(probed.unwrap_err(), DirectError::Unreachable);
        assert_eq!(
            recorded.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            ["kind", "peer", "outcome", "error", "elapsed_ms"]
        );
        assert_eq!(
            field(&recorded, "kind"),
            Some(&serde_json::Value::from("probe"))
        );
        assert_eq!(
            field(&recorded, "outcome"),
            Some(&serde_json::Value::from("failed"))
        );
        assert_eq!(
            field(&recorded, "error"),
            Some(&serde_json::Value::from("unreachable"))
        );
    }

    const SILENCE_OF_A_GONE_PROCESS: Duration = Duration::from_millis(1500);
    const JUST_BEFORE: Duration = Duration::from_millis(1);
    const JUST_UNDER_THE_SILENCE_OF_A_GONE_PROCESS: Duration =
        SILENCE_OF_A_GONE_PROCESS.saturating_sub(JUST_BEFORE);

    #[tokio::test]
    async fn spec_3_8_a_connection_silent_just_under_three_keep_alive_intervals_is_kept_and_one_silent_for_them_is_closed()
     {
        let held_at = tokio::time::Instant::now();
        let reading = Arc::new(Mutex::new(held_at));
        let register = HeldConnections::new(
            {
                let reading = Arc::clone(&reading);
                Arc::new(move || *reading.lock().unwrap())
            },
            dropping_transport(),
        );
        let this_node = bind_node_holding(SERVER_SEED, register.clone()).await;
        let peer_node = bind_node(CLIENT_SEED).await;
        this_node.replace_address(peer_node.endpoint().addr());
        let (dialed, accepted) = tokio::join!(
            this_node
                .endpoint()
                .connect(peer_node.endpoint().id(), GOSSIP_ALPN),
            async {
                let incoming = peer_node.endpoint().accept().await.unwrap();
                incoming.accept().unwrap().await
            }
        );
        let dialed = dialed.unwrap();
        let _accepted = accepted.unwrap();

        *reading.lock().unwrap() = held_at
            .checked_add(JUST_UNDER_THE_SILENCE_OF_A_GONE_PROCESS)
            .unwrap();
        register.close_silent_of(card_of(CLIENT_SEED));
        let closed_just_before_the_bound = dialed.close_reason();
        *reading.lock().unwrap() = held_at.checked_add(SILENCE_OF_A_GONE_PROCESS).unwrap();
        register.close_silent_of(card_of(CLIENT_SEED));

        assert_eq!(closed_just_before_the_bound, None);
        assert_eq!(dialed.close_reason(), Some(ConnectionError::LocallyClosed));
    }

    const SETTLED_WAIT: Duration = Duration::from_secs(5);

    /// Whether each connection to or from the card the register holds reports a path iroh selected, in the order held.
    fn selected_path_reported_by_each_held_of(
        register: &HeldConnections,
        card: wire::PublicKey,
    ) -> Vec<bool> {
        register
            .held()
            .get(&card)
            .into_iter()
            .flatten()
            .map(HeldConnection::reports_a_selected_path)
            .collect()
    }

    /// Whether the register holds connections to or from the card and has heard from the peer on every one of them at or after the instant.
    fn every_held_of_heard_since(
        register: &HeldConnections,
        card: wire::PublicKey,
        since: tokio::time::Instant,
    ) -> bool {
        register.held().get(&card).is_some_and(|of_card| {
            !of_card.is_empty()
                && of_card
                    .iter()
                    .all(|held_connection| held_connection.last_heard >= since)
        })
    }

    /// Dials the peer node on the protocol from this node and returns the dialed connection with the one the peer accepted.
    async fn dialed_and_accepted(
        this_node: &NodeEndpoint,
        peer_node: &NodeEndpoint,
        alpn: &[u8],
    ) -> (Connection, Connection) {
        let (dialed, accepted) = tokio::join!(
            this_node
                .endpoint()
                .connect(peer_node.endpoint().id(), alpn),
            async {
                let incoming = peer_node.endpoint().accept().await.unwrap();
                incoming.accept().unwrap().await
            }
        );
        (dialed.unwrap(), accepted.unwrap())
    }

    #[tokio::test]
    async fn spec_3_8_a_healthy_connection_of_each_protocol_reports_a_selected_path_at_both_ends_when_it_opens_and_three_keep_alive_intervals_later()
     {
        let this_register = unrecorded_register();
        let peer_register = unrecorded_register();
        let this_node = bind_node_holding(SERVER_SEED, this_register.clone()).await;
        let peer_node = bind_node_holding(CLIENT_SEED, peer_register.clone()).await;
        this_node.replace_address(peer_node.endpoint().addr());
        let opened_at = tokio::time::Instant::now();

        let mut reported_when_opened = Vec::new();
        let mut open = Vec::new();
        for alpn in [DIRECT_ALPN, GOSSIP_ALPN] {
            open.push(dialed_and_accepted(&this_node, &peer_node, alpn).await);
            reported_when_opened.push((
                selected_path_reported_by_each_held_of(&this_register, card_of(CLIENT_SEED)),
                selected_path_reported_by_each_held_of(&peer_register, card_of(SERVER_SEED)),
            ));
        }
        let heard_since = opened_at.checked_add(SILENCE_OF_A_GONE_PROCESS).unwrap();
        let heard_through_three_keep_alive_intervals = timeout(SETTLED_WAIT, async {
            while !(every_held_of_heard_since(&this_register, card_of(CLIENT_SEED), heard_since)
                && every_held_of_heard_since(&peer_register, card_of(SERVER_SEED), heard_since))
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let reported_three_keep_alive_intervals_later = (
            selected_path_reported_by_each_held_of(&this_register, card_of(CLIENT_SEED)),
            selected_path_reported_by_each_held_of(&peer_register, card_of(SERVER_SEED)),
        );

        assert!(
            heard_through_three_keep_alive_intervals.is_ok(),
            "the peers were not heard on every connection {SILENCE_OF_A_GONE_PROCESS:?} after it opened, within {SETTLED_WAIT:?}"
        );
        assert_eq!(
            reported_when_opened,
            vec![
                (vec![true], vec![true]),
                (vec![true, true], vec![true, true])
            ]
        );
        assert_eq!(
            reported_three_keep_alive_intervals_later,
            (vec![true, true], vec![true, true])
        );
        drop(open);
    }

    /// A monotonic clock the test sets, read at the instant it was last set to, and the setting it reads from.
    fn clock_the_test_sets() -> (MonotonicClock, Arc<Mutex<tokio::time::Instant>>) {
        let reading = Arc::new(Mutex::new(tokio::time::Instant::now()));
        let clock: MonotonicClock = {
            let reading = Arc::clone(&reading);
            Arc::new(move || *reading.lock().unwrap())
        };
        (clock, reading)
    }

    const HEARING_NOTED_WAIT: Duration = Duration::from_secs(5);

    /// Waits until the register has noted every datagram each connection to or from the card has received, so a silence read later starts at the last of them, failing by name when it has not within the wait.
    async fn wait_until_every_datagram_of_is_noted(
        register: &HeldConnections,
        card: wire::PublicKey,
    ) {
        let noted = timeout(HEARING_NOTED_WAIT, async {
            while !register.held().get(&card).is_some_and(|of_card| {
                of_card.iter().all(|held_connection| {
                    held_connection.connection.upgrade().is_some_and(|live| {
                        live.stats().udp_rx.datagrams == held_connection.datagrams_heard
                    })
                })
            }) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            noted.is_ok(),
            "the register did not note every datagram of the card's connections within {HEARING_NOTED_WAIT:?}"
        );
    }

    /// Each path of each live connection to or from the card the register holds, with whether iroh selected it, in the order held.
    fn paths_of_each_held_of(
        register: &HeldConnections,
        card: wire::PublicKey,
    ) -> Vec<Vec<String>> {
        register
            .held()
            .get(&card)
            .into_iter()
            .flatten()
            .filter_map(|held_connection| held_connection.connection.upgrade())
            .map(|live| {
                live.paths()
                    .iter()
                    .map(|path| format!("{:?} selected {}", path.remote_addr(), path.is_selected()))
                    .collect()
            })
            .collect()
    }

    /// Moves the clock the test sets on by as long as a gone process is silent.
    fn move_past_the_silence_of_a_gone_process(reading: &Mutex<tokio::time::Instant>) {
        let mut reading = reading.lock().unwrap();
        *reading = reading.checked_add(SILENCE_OF_A_GONE_PROCESS).unwrap();
    }

    #[tokio::test]
    async fn spec_3_8_closing_a_restarted_cards_gone_connections_also_closes_its_new_one_that_reports_no_selected_path_so_the_next_dial_reaches_the_new_process()
     {
        let (clock, reading) = clock_the_test_sets();
        let Rig { link, .. } =
            rig_over(RecordingHost::with_card(card_of(SERVER_SEED)), clock).await;
        connection_from_a_process_that_vanished(link.server_node.endpoint().addr()).await;
        wait_until_every_datagram_of_is_noted(
            &link.server_node.held_connections(),
            card_of(CLIENT_SEED),
        )
        .await;
        move_past_the_silence_of_a_gone_process(&reading);
        let restarted = link
            .client_node
            .endpoint()
            .connect(link.server_node.endpoint().id(), DIRECT_ALPN)
            .await
            .unwrap();
        raw_exchange(&restarted, &sealed_message_to_server(&keys()))
            .await
            .unwrap();
        let register = link.server_node.held_connections();
        let reported_before =
            selected_path_reported_by_each_held_of(&register, card_of(CLIENT_SEED));
        let paths_before = paths_of_each_held_of(&register, card_of(CLIENT_SEED));

        register.close_silent_of(card_of(CLIENT_SEED));
        let restarted_closed = timeout(DIRECT_ANSWER_WAIT, restarted.closed()).await;
        let dialed_back = timeout(DIRECT_ANSWER_WAIT, async {
            tokio::join!(
                link.server_node
                    .endpoint()
                    .connect(link.client_node.endpoint().id(), GOSSIP_ALPN),
                async {
                    let incoming = link.client_node.endpoint().accept().await?;
                    incoming.accept().ok()?.await.ok()
                }
            )
        })
        .await;

        let restarted_reported_a_selected_path = reported_before.get(1) == Some(&true);
        let restarted_closed_as_superseded = matches!(
            &restarted_closed,
            Ok(ConnectionError::ApplicationClosed(close))
                if close.error_code == VarInt::from_u32(SUPERSEDED_CONNECTION_CLOSE_CODE)
        );

        assert_eq!(reported_before.len(), 2, "{paths_before:?}");
        assert_eq!(
            restarted_closed_as_superseded, !restarted_reported_a_selected_path,
            "the restarted connection, among {paths_before:?}, ended {restarted_closed:?}"
        );
        assert!(
            matches!(&dialed_back, Ok((Ok(_), Some(_)))),
            "the server's dial back to the restarted process did not connect within {DIRECT_ANSWER_WAIT:?}: {dialed_back:?}"
        );
    }

    /// Takes the card's first held connection as last heard from as long ago as a gone process is silent, as a connection of a live process that stalled reads.
    fn stall_the_first_held_of(register: &HeldConnections, card: wire::PublicKey) {
        let stalled_since = tokio::time::Instant::now()
            .checked_sub(SILENCE_OF_A_GONE_PROCESS)
            .unwrap();
        register
            .held()
            .get_mut(&card)
            .unwrap()
            .first_mut()
            .unwrap()
            .last_heard = stalled_since;
    }

    #[tokio::test]
    async fn spec_3_8_a_live_card_whose_other_connection_reports_a_selected_path_keeps_it_when_its_stalled_connection_closes()
     {
        let rig = rig().await;
        let stalled = raw_connection(&rig).await;
        raw_exchange(&stalled, &sealed_message_to_server(&keys()))
            .await
            .unwrap();
        let healthy = raw_connection(&rig).await;
        raw_exchange(&healthy, &sealed_message_to_server(&keys()))
            .await
            .unwrap();
        let register = rig.link.server_node.held_connections();
        let reported_before =
            selected_path_reported_by_each_held_of(&register, card_of(CLIENT_SEED));

        stall_the_first_held_of(&register, card_of(CLIENT_SEED));
        register.close_silent_of(card_of(CLIENT_SEED));
        let stalled_closed = timeout(DIRECT_ANSWER_WAIT, stalled.closed()).await;
        let healthy_still_answers = still_answers(&healthy).await;

        assert_eq!(reported_before, vec![true, true]);
        assert!(
            matches!(
                &stalled_closed,
                Ok(ConnectionError::ApplicationClosed(close))
                    if close.error_code == VarInt::from_u32(SUPERSEDED_CONNECTION_CLOSE_CODE)
            ),
            "{stalled_closed:?}"
        );
        assert!(
            healthy_still_answers,
            "the healthy connection did not answer after its card's stalled one closed"
        );
    }

    const DIVERGENT_SELECTION_ATTEMPTS: usize = 10;
    const LIVE_CARD_HEARD_WAIT: Duration = Duration::from_secs(10);

    /// The node's address on the loopback interface, at the port its endpoint bound.
    fn address_on_loopback(node: &NodeEndpoint) -> iroh::EndpointAddr {
        let address = node.endpoint().addr();
        iroh::EndpointAddr::from_parts(
            address.id,
            address.ip_addrs().map(|socket| {
                iroh::TransportAddr::Ip(std::net::SocketAddr::new(
                    std::net::Ipv4Addr::LOCALHOST.into(),
                    socket.port(),
                ))
            }),
        )
    }

    /// Two live nodes, the peer's card held in this node's register, with the connection this node dialed and the one it accepted from the peer, each with its other end.
    struct LiveCard {
        register: HeldConnections,
        this_node: NodeEndpoint,
        peer_node: NodeEndpoint,
        dialed: (Connection, Connection),
        accepted: (Connection, Connection),
    }

    /// A live card whose connections this node dialed on loopback and accepted from the network at once, each heard for as long as a gone process is silent after they opened.
    async fn live_card_dialed_on_loopback_and_accepted_from_the_network() -> LiveCard {
        let register = unrecorded_register();
        let this_node = bind_node_holding(SERVER_SEED, register.clone()).await;
        let peer_node = bind_node(CLIENT_SEED).await;
        this_node.replace_address(address_on_loopback(&peer_node));
        peer_node.replace_address(this_node.endpoint().addr());
        let opened_at = tokio::time::Instant::now();
        let (dialed, (accepted_at_peer, accepted)) = tokio::join!(
            dialed_and_accepted(&this_node, &peer_node, DIRECT_ALPN),
            dialed_and_accepted(&peer_node, &this_node, GOSSIP_ALPN)
        );
        let heard_since = opened_at.checked_add(SILENCE_OF_A_GONE_PROCESS).unwrap();
        let heard = timeout(LIVE_CARD_HEARD_WAIT, async {
            while !every_held_of_heard_since(&register, card_of(CLIENT_SEED), heard_since) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            heard.is_ok(),
            "the live card was not heard on every connection {SILENCE_OF_A_GONE_PROCESS:?} after they opened, within {LIVE_CARD_HEARD_WAIT:?}"
        );
        LiveCard {
            register,
            this_node,
            peer_node,
            dialed,
            accepted: (accepted, accepted_at_peer),
        }
    }

    /// The live card once the connection this node dialed is on the path iroh selected and the one it accepted is not, retried while iroh selects one path at both ends.
    async fn live_card_whose_accepted_connection_is_off_the_selected_path() -> LiveCard {
        for _ in 0..DIVERGENT_SELECTION_ATTEMPTS {
            let live_card = live_card_dialed_on_loopback_and_accepted_from_the_network().await;
            if held_on_a_selected_path(
                &live_card.register,
                card_of(CLIENT_SEED),
                &live_card.dialed.0,
            ) && !held_on_a_selected_path(
                &live_card.register,
                card_of(CLIENT_SEED),
                &live_card.accepted.0,
            ) {
                return live_card;
            }
        }
        panic!(
            "iroh selected one path at both ends in each of {DIVERGENT_SELECTION_ATTEMPTS} attempts"
        )
    }

    /// A router serving the direct protocol on the node for a host holding the card of the seed.
    fn serving_the_direct_protocol(node: &NodeEndpoint, seed: u8) -> Router {
        Router::builder(node.endpoint().clone())
            .accept(
                DIRECT_ALPN,
                DirectServer::new(keys(), RecordingHost::with_card(card_of(seed)), node),
            )
            .spawn()
    }

    /// Whether the register holds the connection to or from the card on a path iroh selected.
    fn held_on_a_selected_path(
        register: &HeldConnections,
        card: wire::PublicKey,
        connection: &Connection,
    ) -> bool {
        register.held().get(&card).is_some_and(|of_card| {
            of_card.iter().any(|held_connection| {
                held_connection.id == ConnectionId::of(connection)
                    && held_connection.reports_a_selected_path()
            })
        })
    }

    /// Takes the held connection as last heard from as long ago as a gone process is silent, as a connection of a live process that stalled reads.
    fn stall_the_held(register: &HeldConnections, card: wire::PublicKey, connection: &Connection) {
        let stalled_since = tokio::time::Instant::now()
            .checked_sub(SILENCE_OF_A_GONE_PROCESS)
            .unwrap();
        for held_connection in register.held().get_mut(&card).unwrap() {
            if held_connection.id == ConnectionId::of(connection) {
                held_connection.last_heard = stalled_since;
            }
        }
    }

    #[tokio::test]
    async fn spec_3_8_a_live_card_whose_other_connection_reports_no_selected_path_loses_it_with_its_stalled_one_and_each_side_dials_afresh_and_is_answered()
     {
        let live_card = live_card_whose_accepted_connection_is_off_the_selected_path().await;
        let this_node = live_card.this_node.endpoint().id();
        let peer = live_card.peer_node.endpoint().id();

        stall_the_held(
            &live_card.register,
            card_of(CLIENT_SEED),
            &live_card.dialed.0,
        );
        live_card.register.close_silent_of(card_of(CLIENT_SEED));
        let held_after_the_pass = connections_held_from(&live_card.this_node, card_of(CLIENT_SEED));
        let closed_at_the_card = timeout(DIRECT_ANSWER_WAIT, live_card.accepted.1.closed()).await;
        let _routers = (
            serving_the_direct_protocol(&live_card.this_node, SERVER_SEED),
            serving_the_direct_protocol(&live_card.peer_node, CLIENT_SEED),
        );
        let answered_by_the_peer = DirectClient::new(
            live_card.this_node.endpoint().clone(),
            keys(),
            ClientTimeouts::SPEC,
            unrecorded_register(),
        )
        .probe(peer, PROBE_NONCE)
        .await;
        let answered_by_this_node = DirectClient::new(
            live_card.peer_node.endpoint().clone(),
            keys(),
            ClientTimeouts::SPEC,
            unrecorded_register(),
        )
        .probe(this_node, PROBE_NONCE)
        .await;

        assert_eq!(
            held_after_the_pass, 0,
            "the live card's connection off the selected path stayed open after its stalled one closed"
        );
        assert!(
            matches!(
                &closed_at_the_card,
                Ok(ConnectionError::ApplicationClosed(close))
                    if close.error_code == VarInt::from_u32(SUPERSEDED_CONNECTION_CLOSE_CODE)
            ),
            "{closed_at_the_card:?}"
        );
        assert_eq!(answered_by_the_peer.unwrap().nonce, PROBE_NONCE);
        assert_eq!(answered_by_this_node.unwrap().nonce, PROBE_NONCE);
    }

    #[tokio::test]
    async fn spec_3_8_an_introduce_from_a_restarted_process_closes_its_cards_gone_connection_and_keeps_the_one_it_arrived_on_whatever_path_iroh_selected()
     {
        let (clock, reading) = clock_the_test_sets();
        let Rig { link, .. } =
            rig_over(RecordingHost::with_card(card_of(SERVER_SEED)), clock).await;
        let server = link.server_node.endpoint().id();
        connection_from_a_process_that_vanished(link.server_node.endpoint().addr()).await;
        let gone = connections_held_open_from(&link.server_node, card_of(CLIENT_SEED));
        wait_until_every_datagram_of_is_noted(
            &link.server_node.held_connections(),
            card_of(CLIENT_SEED),
        )
        .await;
        move_past_the_silence_of_a_gone_process(&reading);

        let introduced = link
            .client
            .introduce(server, introduction_from_client())
            .await;
        let probed_on_the_same_connection = link.client.probe(server, PROBE_NONCE).await;

        assert!(
            matches!(introduced, Ok(IntroduceAck { .. })),
            "{introduced:?}"
        );
        assert_eq!(
            gone.iter()
                .map(Connection::close_reason)
                .collect::<Vec<_>>(),
            vec![Some(ConnectionError::LocallyClosed)]
        );
        assert_eq!(probed_on_the_same_connection.unwrap().nonce, PROBE_NONCE);
    }

    #[tokio::test]
    async fn spec_3_8_a_presence_after_an_introduce_that_left_its_card_on_a_gone_processs_path_closes_the_introduces_connection_so_the_next_dial_reaches_the_new_process()
     {
        let (clock, reading) = clock_the_test_sets();
        let Rig { link, .. } =
            rig_over(RecordingHost::with_card(card_of(SERVER_SEED)), clock).await;
        let server = link.server_node.endpoint().id();
        let register = link.server_node.held_connections();
        connection_from_a_process_that_vanished(link.server_node.endpoint().addr()).await;
        wait_until_every_datagram_of_is_noted(&register, card_of(CLIENT_SEED)).await;
        move_past_the_silence_of_a_gone_process(&reading);
        link.client
            .introduce(server, introduction_from_client())
            .await
            .unwrap();
        let reported_after_the_introduce =
            selected_path_reported_by_each_held_of(&register, card_of(CLIENT_SEED));
        let paths_after_the_introduce = paths_of_each_held_of(&register, card_of(CLIENT_SEED));

        register.close_silent_of(card_of(CLIENT_SEED));
        let held_after_the_presence =
            connections_held_from(&link.server_node, card_of(CLIENT_SEED));
        let dialed_back = timeout(DIRECT_ANSWER_WAIT, async {
            tokio::join!(
                link.server_node
                    .endpoint()
                    .connect(link.client_node.endpoint().id(), GOSSIP_ALPN),
                async {
                    let incoming = link.client_node.endpoint().accept().await?;
                    incoming.accept().ok()?.await.ok()
                }
            )
        })
        .await;

        assert_eq!(
            reported_after_the_introduce.len(),
            1,
            "{paths_after_the_introduce:?}"
        );
        assert_eq!(
            held_after_the_presence,
            usize::from(reported_after_the_introduce[0]),
            "the introduce's connection, on {paths_after_the_introduce:?}, was held {held_after_the_presence} time(s) after the presence"
        );
        assert!(
            matches!(&dialed_back, Ok((Ok(_), Some(_)))),
            "the server's dial back to the restarted process did not connect within {DIRECT_ANSWER_WAIT:?}: {dialed_back:?}"
        );
    }

    #[tokio::test]
    async fn spec_3_8_an_introduce_keeps_the_connection_it_arrived_on_even_one_that_reads_as_gone()
    {
        let Rig { link, .. } = rig_over(
            RecordingHost::with_card(card_of(SERVER_SEED)),
            clock_reading_every_connection_as_gone(),
        )
        .await;
        let server = link.server_node.endpoint().id();

        let introduced = link
            .client
            .introduce(server, introduction_from_client())
            .await;
        let probed_on_the_same_connection = link.client.probe(server, PROBE_NONCE).await;

        assert!(
            matches!(introduced, Ok(IntroduceAck { .. })),
            "{introduced:?}"
        );
        assert_eq!(probed_on_the_same_connection.unwrap().nonce, PROBE_NONCE);
    }

    const NOT_AN_ANNOUNCED_HANDLE: &str = "Alice";
    const DIRECT_ANSWER_WAIT: Duration = Duration::from_secs(5);

    async fn introduce_beside_an_earlier_connection(
        introduction: wire::Introduce,
    ) -> (Rig, Result<IntroduceAck, DirectError>, Connection, bool) {
        let rig = rig_over(
            RecordingHost::with_card(card_of(SERVER_SEED)),
            clock_reading_every_connection_as_gone(),
        )
        .await;
        let earlier = raw_connection(&rig).await;
        raw_exchange(&earlier, &sealed_message_to_server(&keys()))
            .await
            .unwrap();

        let introduced = rig
            .link
            .client
            .introduce(rig.link.server_node.endpoint().id(), introduction)
            .await;
        let answered_on_the_earlier_connection = still_answers(&earlier).await;
        (rig, introduced, earlier, answered_on_the_earlier_connection)
    }

    /// Whether a Message sent on the connection after the server's last answer is acknowledged, within the wait for a direct answer: any close the server made before that answer reaches the connection first.
    async fn still_answers(connection: &Connection) -> bool {
        timeout(DIRECT_ANSWER_WAIT, async {
            let (mut send, mut recv) = connection.open_bi().await.ok()?;
            send.write_all(&sealed_message_to_server(&keys()))
                .await
                .ok()?;
            send.finish().ok()?;
            let answer = recv.read_to_end(WIRE_LIMIT_BYTES).await.ok()?;
            open_response(&keys(), &answer).ok()
        })
        .await
        .ok()
        .flatten()
        .is_some_and(|response| {
            matches!(
                response,
                Response::MessageAck(MessageAck { accepted: true, .. })
            )
        })
    }

    #[tokio::test]
    async fn spec_3_8_an_introduce_whose_announced_handle_does_not_match_is_dropped_before_any_connection_closes()
     {
        let (rig, introduced, earlier, answered_on_the_earlier_connection) =
            introduce_beside_an_earlier_connection(wire::Introduce {
                handle: Some(NOT_AN_ANNOUNCED_HANDLE.to_owned()),
                ..introduction_from_client()
            })
            .await;

        assert_eq!(introduced, Err(DirectError::Refused));
        assert!(
            answered_on_the_earlier_connection,
            "the earlier connection did not answer after the dropped introduce: {:?}",
            earlier.close_reason()
        );
        assert_eq!(earlier.close_reason(), None);
        assert_eq!(rig.host.introduced.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn spec_3_5_an_introduce_whose_public_is_over_the_cap_is_dropped_before_any_connection_closes()
     {
        let (rig, introduced, earlier, answered_on_the_earlier_connection) =
            introduce_beside_an_earlier_connection(wire::Introduce {
                public: public_mapping_of_bytes(SPEC_PUBLIC_CAP_BYTES + 1),
                ..introduction_from_client()
            })
            .await;

        assert_eq!(introduced, Err(DirectError::Refused));
        assert!(
            answered_on_the_earlier_connection,
            "the earlier connection did not answer after the dropped introduce: {:?}",
            earlier.close_reason()
        );
        assert_eq!(earlier.close_reason(), None);
        assert_eq!(rig.host.introduced.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn spec_3_6_introduce_is_answered_with_the_hosts_introduction_and_applied_for_the_sender()
    {
        let rig = rig().await;
        rig.host.attached.store(true, Ordering::SeqCst);

        let ack = rig
            .link
            .client
            .introduce(
                rig.link.server_node.endpoint().id(),
                introduction_from_client(),
            )
            .await
            .unwrap();

        assert_eq!(
            ack,
            IntroduceAck {
                mac: ack.mac,
                ..own_introduction(true)
            }
        );
        assert_ne!(ack.mac, ZERO_MAC);
        let introduced = rig.host.introduced.lock().unwrap().clone();
        assert_eq!(introduced.len(), 1);
        assert_eq!(introduced[0].0, card_of(CLIENT_SEED));
        assert_eq!(introduced[0].1.handle.as_deref(), Some(SENDER_HANDLE));
    }

    #[tokio::test]
    async fn spec_3_14_a_node_without_a_card_refuses_every_message_and_answers_probe_and_introduce()
    {
        let Rig { host, link } = rig_over(
            RecordingHost::without_card(),
            Arc::new(tokio::time::Instant::now),
        )
        .await;
        let server = link.server_node.endpoint().id();

        let to_server_id = link
            .client
            .send_message(server, message_to(card_of(SERVER_SEED), BODY))
            .await
            .unwrap();
        let to_client_card = link
            .client
            .send_message(server, message_to(card_of(CLIENT_SEED), BODY))
            .await
            .unwrap();
        let probed = link.client.probe(server, PROBE_NONCE).await.unwrap();
        let introduced = link
            .client
            .introduce(server, introduction_from_client())
            .await;

        assert_eq!(to_server_id, refusal_ack(&ErrorCode::Refused));
        assert_eq!(to_client_card, refusal_ack(&ErrorCode::Refused));
        assert_eq!(host.received().len(), 0);
        assert_eq!(probed.nonce, PROBE_NONCE);
        assert_eq!(introduced.unwrap().handle.as_deref(), Some(OWN_HANDLE));
        assert_eq!(host.introduced.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn spec_3_9_probe_and_introduce_go_unanswered_while_the_process_is_stopping() {
        let rig = rig().await;
        rig.host.stopping.store(true, Ordering::SeqCst);
        let server = rig.link.server_node.endpoint().id();

        let probed = rig.link.client.probe(server, PROBE_NONCE).await;
        let introduced = rig
            .link
            .client
            .introduce(server, introduction_from_client())
            .await;

        assert_eq!(probed, Err(DirectError::Refused));
        assert_eq!(introduced, Err(DirectError::Refused));
        assert_eq!(rig.host.introduced.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn spec_3_6_client_reports_a_reset_stream_as_refused() {
        let rig = rig().await;
        let other_keys = NetworkKeys::derive(&NetworkSecret::from_bytes(OTHER_NETWORK_SECRET));
        let client_under_another_secret = DirectClient::new(
            rig.link.client_node.endpoint().clone(),
            other_keys,
            ClientTimeouts::SPEC,
            unrecorded_register(),
        );

        let outcome = client_under_another_secret
            .send_message(
                rig.link.server_node.endpoint().id(),
                message_to(card_of(SERVER_SEED), BODY),
            )
            .await;

        assert_eq!(outcome, Err(DirectError::Refused));
    }

    #[derive(Debug, Clone)]
    enum StreamScript {
        Silent,
        Reply(Vec<u8>),
        Reset,
    }

    #[derive(Debug, Clone)]
    struct ScriptedServer {
        script: Vec<StreamScript>,
        connections: Arc<AtomicUsize>,
        streams: Arc<AtomicUsize>,
    }

    impl ScriptedServer {
        fn replying(reply: Option<Vec<u8>>) -> Self {
            Self::following(vec![
                reply.map_or(StreamScript::Silent, StreamScript::Reply),
            ])
        }

        fn following(script: Vec<StreamScript>) -> Self {
            Self {
                script,
                connections: Arc::new(AtomicUsize::new(0)),
                streams: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn next_script(&self) -> StreamScript {
            let index = self.streams.fetch_add(1, Ordering::SeqCst);
            self.script
                .get(index)
                .or_else(|| self.script.last())
                .unwrap()
                .clone()
        }

        fn connections(&self) -> usize {
            self.connections.load(Ordering::SeqCst)
        }
    }

    impl ProtocolHandler for ScriptedServer {
        async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
            self.connections.fetch_add(1, Ordering::SeqCst);
            while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                let script = self.next_script();
                tokio::spawn(async move {
                    recv.read_to_end(WIRE_LIMIT_BYTES).await.unwrap();
                    match script {
                        StreamScript::Reply(bytes) => {
                            send.write_all(&bytes).await.unwrap();
                            send.finish().unwrap();
                        }
                        StreamScript::Reset => {
                            send.reset(VarInt::from_u32(UNANSWERED_RESET_CODE_ON_THE_WIRE))
                                .unwrap();
                        }
                        StreamScript::Silent => std::future::pending().await,
                    }
                });
            }
            Ok(())
        }
    }

    fn sealed_probe_ack(nonce: Nonce) -> Vec<u8> {
        seal_response(
            &keys(),
            Response::ProbeAck(ProbeAck {
                v: Version::CURRENT,
                nonce,
                attached: false,
                mac: ZERO_MAC,
            }),
        )
        .unwrap()
    }

    const MESSAGE_ID_REVERSED: [u8; 16] = [
        0x0F, 0xFE, 0xED, 0xDC, 0xCB, 0xBA, 0xA9, 0x98, 0x87, 0x76, 0x65, 0x54, 0x43, 0x32, 0x21,
        0x10,
    ];

    fn sealed_accepting_message_ack(id: wire::MessageId) -> Vec<u8> {
        seal_response(
            &keys(),
            Response::MessageAck(MessageAck {
                v: Version::CURRENT,
                id,
                accepted: true,
                attached: true,
                reason: None,
            }),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn spec_3_6_a_message_ack_for_another_id_is_a_failed_send() {
        let server =
            ScriptedServer::replying(Some(sealed_accepting_message_ack(MESSAGE_ID_REVERSED)));
        let link = link_to(server, ClientTimeouts::SPEC).await;

        let outcome = link
            .client
            .send_message(
                link.server_node.endpoint().id(),
                message_to(card_of(SERVER_SEED), BODY),
            )
            .await;

        assert_eq!(outcome, Err(DirectError::MessageAckIdMismatch));
    }

    #[tokio::test]
    async fn spec_3_6_probe_ack_with_another_nonce_is_a_failed_probe() {
        let server = ScriptedServer::replying(Some(sealed_probe_ack(PROBE_NONCE_REVERSED)));
        let link = link_to(server, ClientTimeouts::SPEC).await;

        let outcome = link
            .client
            .probe(link.server_node.endpoint().id(), PROBE_NONCE)
            .await;

        assert_eq!(outcome, Err(DirectError::ProbeNonceMismatch));
    }

    #[tokio::test]
    async fn spec_3_6_probe_ack_with_another_networks_mac_is_a_failed_probe() {
        let other_keys = NetworkKeys::derive(&NetworkSecret::from_bytes(OTHER_NETWORK_SECRET));
        let forged = seal_response(
            &other_keys,
            Response::ProbeAck(ProbeAck {
                v: Version::CURRENT,
                nonce: PROBE_NONCE,
                attached: false,
                mac: ZERO_MAC,
            }),
        )
        .unwrap();
        let link = link_to(ScriptedServer::replying(Some(forged)), ClientTimeouts::SPEC).await;

        let outcome = link
            .client
            .probe(link.server_node.endpoint().id(), PROBE_NONCE)
            .await;

        assert!(matches!(outcome, Err(DirectError::InvalidAck(_))));
    }

    #[tokio::test]
    async fn spec_3_14_probe_without_an_ack_fails_after_the_probe_timeout() {
        let link = link_to(ScriptedServer::replying(None), ClientTimeouts::SPEC).await;

        let started = Instant::now();
        let outcome = link
            .client
            .probe(link.server_node.endpoint().id(), PROBE_NONCE)
            .await;
        let waited = started.elapsed();

        assert_eq!(outcome, Err(DirectError::Timeout));
        assert!(SPEC_PROBE_TIMEOUT <= waited && waited < SPEC_PROBE_TIMEOUT + TIMEOUT_SLACK);
    }

    #[tokio::test]
    async fn spec_3_14_message_without_an_ack_times_out_after_the_ack_timeout() {
        let link = link_to(ScriptedServer::replying(None), ClientTimeouts::SPEC).await;

        let started = Instant::now();
        let outcome = link
            .client
            .send_message(
                link.server_node.endpoint().id(),
                message_to(card_of(SERVER_SEED), BODY),
            )
            .await;
        let waited = started.elapsed();

        assert_eq!(outcome, Err(DirectError::Timeout));
        assert!(SPEC_ACK_TIMEOUT <= waited && waited < SPEC_ACK_TIMEOUT + TIMEOUT_SLACK);
    }

    const TIMEOUTS_PAST_THE_CLOCK_S_REACH: ClientTimeouts = ClientTimeouts {
        connect: Duration::MAX,
        ack: Duration::MAX,
        probe: Duration::MAX,
    };

    #[tokio::test]
    async fn spec_3_14_timeouts_past_the_clock_s_reach_wait_for_the_ack_instead_of_failing_at_once()
    {
        let server = ScriptedServer::following(vec![
            StreamScript::Reply(sealed_probe_ack(PROBE_NONCE)),
            StreamScript::Reply(sealed_accepting_message_ack(MESSAGE_ID)),
        ]);
        let link = link_to(server, TIMEOUTS_PAST_THE_CLOCK_S_REACH).await;
        let peer = link.server_node.endpoint().id();

        let probed = link.client.probe(peer, PROBE_NONCE).await;
        let sent = link
            .client
            .send_message(peer, message_to(card_of(SERVER_SEED), BODY))
            .await;

        assert_eq!(probed.map(|ack| ack.nonce), Ok(PROBE_NONCE));
        assert_eq!(sent.map(|ack| ack.id), Ok(MESSAGE_ID));
    }

    fn point_the_server_at_a_silent_socket(link: &Link) -> std::net::UdpSocket {
        let black_hole = std::net::UdpSocket::bind(LOOPBACK_ANY_PORT).unwrap();
        link.client_node
            .replace_address(iroh::EndpointAddr::from_parts(
                link.server_node.endpoint().id(),
                [iroh::TransportAddr::Ip(black_hole.local_addr().unwrap())],
            ));
        black_hole
    }

    #[tokio::test]
    async fn spec_3_14_dial_to_an_unanswering_address_is_unreachable_after_the_connect_timeout() {
        let link = link_to(ScriptedServer::replying(None), ClientTimeouts::SPEC).await;
        let _black_hole = point_the_server_at_a_silent_socket(&link);

        let started = Instant::now();
        let outcome = link
            .client
            .send_message(
                link.server_node.endpoint().id(),
                message_to(card_of(SERVER_SEED), BODY),
            )
            .await;
        let waited = started.elapsed();

        assert_eq!(outcome, Err(DirectError::Unreachable));
        assert!(SPEC_CONNECT_TIMEOUT <= waited && waited < SPEC_CONNECT_TIMEOUT + TIMEOUT_SLACK);
    }

    const PROBE_FAILURE_SLACK: Duration = Duration::from_millis(500);

    #[tokio::test]
    async fn spec_3_6_probe_of_an_unanswering_address_with_no_held_connection_fails_within_the_probe_timeout()
     {
        let link = link_to(ScriptedServer::replying(None), ClientTimeouts::SPEC).await;
        let _black_hole = point_the_server_at_a_silent_socket(&link);

        let started = Instant::now();
        let outcome = link
            .client
            .probe(link.server_node.endpoint().id(), PROBE_NONCE)
            .await;
        let waited = started.elapsed();

        assert_eq!(outcome, Err(DirectError::Unreachable));
        assert!(
            SPEC_PROBE_TIMEOUT <= waited && waited < SPEC_PROBE_TIMEOUT + PROBE_FAILURE_SLACK,
            "the probe failed after {waited:?}"
        );
    }

    const HANDSHAKE_HELD_FOR: Duration = Duration::from_secs(2);
    const PROBE_ACK_HELD_FOR: Duration = Duration::from_secs(2);

    async fn answer_one_probe_late(server: Endpoint) {
        let incoming = server.accept().await.unwrap();
        tokio::time::sleep(HANDSHAKE_HELD_FOR).await;
        let connection = incoming.accept().unwrap().await.unwrap();
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        recv.read_to_end(WIRE_LIMIT_BYTES).await.unwrap();
        tokio::time::sleep(PROBE_ACK_HELD_FOR).await;
        if send.write_all(&sealed_probe_ack(PROBE_NONCE)).await.is_ok() {
            let _finished_after_the_probe_timeout = send.finish();
        }
        connection.closed().await;
    }

    #[tokio::test]
    async fn spec_3_6_a_probe_ack_arriving_after_the_probe_timeout_of_the_call_is_a_failed_probe() {
        let server_node = bind_node(SERVER_SEED).await;
        let client_node = bind_node(CLIENT_SEED).await;
        client_node.replace_address(server_node.endpoint().addr());
        let client = DirectClient::new(
            client_node.endpoint().clone(),
            keys(),
            ClientTimeouts::SPEC,
            unrecorded_register(),
        );
        let _answering = tokio::spawn(answer_one_probe_late(server_node.endpoint().clone()));

        let started = Instant::now();
        let outcome = client.probe(server_node.endpoint().id(), PROBE_NONCE).await;
        let waited = started.elapsed();

        assert_eq!(outcome, Err(DirectError::Timeout));
        assert!(
            SPEC_PROBE_TIMEOUT <= waited && waited < SPEC_PROBE_TIMEOUT + PROBE_FAILURE_SLACK,
            "the probe failed after {waited:?}"
        );
    }

    #[tokio::test]
    async fn spec_3_6_requests_to_one_peer_share_one_connection() {
        let server = ScriptedServer::replying(Some(sealed_probe_ack(PROBE_NONCE)));
        let link = link_to(server.clone(), ClientTimeouts::SPEC).await;
        let peer = link.server_node.endpoint().id();

        link.client.probe(peer, PROBE_NONCE).await.unwrap();
        link.client.probe(peer, PROBE_NONCE).await.unwrap();

        assert_eq!(server.connections(), 1);
    }

    #[tokio::test]
    async fn spec_3_6_a_failed_request_drops_the_held_connection_so_the_next_dials_afresh() {
        let server = ScriptedServer::replying(None);
        let short_acks = ClientTimeouts {
            ack: SHORT_TIMEOUT,
            ..ClientTimeouts::SPEC
        };
        let link = link_to(server.clone(), short_acks).await;
        let peer = link.server_node.endpoint().id();

        let first = link
            .client
            .send_message(peer, message_to(card_of(SERVER_SEED), BODY))
            .await;
        let second = link
            .client
            .send_message(peer, message_to(card_of(SERVER_SEED), BODY))
            .await;

        assert_eq!(first, Err(DirectError::Timeout));
        assert_eq!(second, Err(DirectError::Timeout));
        assert_eq!(server.connections(), 2);
    }

    #[tokio::test]
    async fn spec_3_8_dropping_the_held_connection_makes_the_next_request_dial_afresh() {
        let server = ScriptedServer::replying(Some(sealed_probe_ack(PROBE_NONCE)));
        let link = link_to(server.clone(), ClientTimeouts::SPEC).await;
        let peer = link.server_node.endpoint().id();

        link.client.probe(peer, PROBE_NONCE).await.unwrap();
        link.client.drop_held_connection(peer).await;
        link.client.probe(peer, PROBE_NONCE).await.unwrap();

        assert_eq!(server.connections(), 2);
    }

    const CLOSE_KNOWN_WAIT: Duration = Duration::from_secs(5);
    const CONNECT_TIMEOUT_SLACK: Duration = Duration::from_millis(500);

    /// Whether the client's held connection to the peer reads closed within the wait, waited for without a sleep.
    async fn held_connection_known_closed_within(
        client: &DirectClient,
        peer: EndpointId,
        within: Duration,
    ) -> bool {
        timeout(within, async {
            while client
                .slot_of(peer)
                .lock()
                .await
                .as_ref()
                .is_none_or(|connection| connection.close_reason().is_none())
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn spec_3_10_a_request_after_a_live_peer_closed_the_held_connection_dials_afresh_and_is_answered()
     {
        let server = ScriptedServer::replying(Some(sealed_probe_ack(PROBE_NONCE)));
        let server_node = bind_node_holding(
            SERVER_SEED,
            HeldConnections::new(
                clock_reading_every_connection_as_gone(),
                dropping_transport(),
            ),
        )
        .await;
        let link = link_on(server_node, server.clone(), ClientTimeouts::SPEC).await;
        let peer = link.server_node.endpoint().id();
        link.client.probe(peer, PROBE_NONCE).await.unwrap();

        link.server_node
            .held_connections()
            .close_silent_of(card_of(CLIENT_SEED));
        let known_closed =
            held_connection_known_closed_within(&link.client, peer, CLOSE_KNOWN_WAIT).await;
        let probed_after_the_close = link.client.probe(peer, PROBE_NONCE).await;

        assert!(
            known_closed,
            "the client did not learn of the peer's close within {CLOSE_KNOWN_WAIT:?}"
        );
        assert_eq!(probed_after_the_close.unwrap().nonce, PROBE_NONCE);
        assert_eq!(server.connections(), 2);
    }

    #[tokio::test]
    async fn spec_3_10_a_request_to_a_gone_peer_whose_held_connection_reads_closed_is_unreachable_within_the_connect_timeout_and_empties_the_slot()
     {
        let server = ScriptedServer::replying(Some(sealed_probe_ack(PROBE_NONCE)));
        let link = link_to(server, ClientTimeouts::SPEC).await;
        let peer = link.server_node.endpoint().id();
        link.client.probe(peer, PROBE_NONCE).await.unwrap();

        link.server_node.endpoint().close().await;
        let known_closed =
            held_connection_known_closed_within(&link.client, peer, CLOSE_KNOWN_WAIT).await;
        let started = tokio::time::Instant::now();
        let sent = link
            .client
            .send_message(peer, message_to(card_of(SERVER_SEED), BODY))
            .await;
        let answered_after = started.elapsed();
        let slot_left_empty = link.client.slot_of(peer).lock().await.is_none();

        assert!(
            known_closed,
            "the client did not learn of the peer's close within {CLOSE_KNOWN_WAIT:?}"
        );
        assert_eq!(
            sent,
            Err(DirectError::Unreachable),
            "after {answered_after:?}"
        );
        assert!(
            answered_after <= SPEC_CONNECT_TIMEOUT.saturating_add(CONNECT_TIMEOUT_SLACK),
            "answered after {answered_after:?}"
        );
        assert!(slot_left_empty);
    }

    #[tokio::test]
    async fn spec_3_8_after_a_rebind_the_client_dials_afresh_from_the_new_endpoint() {
        let server = ScriptedServer::replying(Some(sealed_probe_ack(PROBE_NONCE)));
        let mut link = link_to(server.clone(), ClientTimeouts::SPEC).await;
        let peer = link.server_node.endpoint().id();
        link.client.probe(peer, PROBE_NONCE).await.unwrap();

        link.client_node.rebind().await.unwrap();
        link.client
            .follow(link.client_node.endpoint().clone())
            .await;

        link.client.probe(peer, PROBE_NONCE).await.unwrap();
        assert_eq!(server.connections(), 2);
    }

    #[tokio::test]
    async fn spec_3_6_concurrent_first_requests_to_one_peer_dial_once() {
        let server = ScriptedServer::replying(Some(sealed_probe_ack(PROBE_NONCE)));
        let link = link_to(server.clone(), ClientTimeouts::SPEC).await;
        let peer = link.server_node.endpoint().id();

        let (first, second) = tokio::join!(
            link.client.probe(peer, PROBE_NONCE),
            link.client.probe(peer, PROBE_NONCE)
        );

        assert!(first.is_ok() && second.is_ok());
        assert_eq!(server.connections(), 1);
    }

    const FIRST_REQUEST_PROBE_TIMEOUT: Duration = Duration::from_millis(600);
    const BEFORE_FIRST_REQUEST_TIMES_OUT: Duration = Duration::from_millis(100);
    const AFTER_FIRST_REQUEST_TIMED_OUT: Duration = Duration::from_millis(900);

    #[tokio::test]
    async fn spec_3_6_a_failure_on_an_old_connection_leaves_the_newer_connection_held() {
        let server = ScriptedServer::following(vec![
            StreamScript::Silent,
            StreamScript::Reset,
            StreamScript::Reply(sealed_probe_ack(PROBE_NONCE)),
        ]);
        let timeouts = ClientTimeouts {
            probe: FIRST_REQUEST_PROBE_TIMEOUT,
            ..ClientTimeouts::SPEC
        };
        let link = link_to(server.clone(), timeouts).await;
        let peer = link.server_node.endpoint().id();

        let (slow_first, later) = tokio::join!(link.client.probe(peer, PROBE_NONCE), async {
            tokio::time::sleep(BEFORE_FIRST_REQUEST_TIMES_OUT).await;
            let reset = link
                .client
                .send_message(peer, message_to(card_of(SERVER_SEED), BODY))
                .await;
            let redialed = link.client.probe(peer, PROBE_NONCE).await;
            tokio::time::sleep(AFTER_FIRST_REQUEST_TIMED_OUT).await;
            let on_newer = link.client.probe(peer, PROBE_NONCE).await;
            (reset, redialed, on_newer)
        });

        assert_eq!(slow_first, Err(DirectError::Timeout));
        assert_eq!(later.0, Err(DirectError::Refused));
        assert!(later.1.is_ok() && later.2.is_ok());
        assert_eq!(server.connections(), 2);
    }

    const CONCURRENT_STREAM_LIMIT: usize = 100;
    const STALLED_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
    const STALLED_PROBE_SLACK: Duration = Duration::from_millis(500);
    const AFTER_THE_STREAM_LIMIT_IS_REACHED: Duration = Duration::from_millis(200);

    #[tokio::test]
    async fn spec_3_14_a_request_stalled_before_its_stream_opens_times_out_within_the_ack_timeout()
    {
        let server = ScriptedServer::replying(None);
        let timeouts = ClientTimeouts {
            probe: STALLED_PROBE_TIMEOUT,
            ..ClientTimeouts::SPEC
        };
        let link = link_to(server, timeouts).await;
        let peer = link.server_node.endpoint().id();
        let client = Arc::new(link.client);
        let mut holding_every_stream = tokio::task::JoinSet::new();
        for _ in 0..CONCURRENT_STREAM_LIMIT {
            let client = Arc::clone(&client);
            holding_every_stream.spawn(async move { client.probe(peer, PROBE_NONCE).await });
        }
        tokio::time::sleep(AFTER_THE_STREAM_LIMIT_IS_REACHED).await;

        let stalled = tokio::time::timeout(
            STALLED_PROBE_TIMEOUT + STALLED_PROBE_SLACK,
            client.probe(peer, PROBE_NONCE),
        )
        .await;

        assert_eq!(stalled, Ok(Err(DirectError::Timeout)));
    }

    const INVALID_UTF8: [u8; 2] = [0xff, 0xfe];
    const BODY_BEFORE_CORRUPTION: &str = "ab";

    #[tokio::test]
    async fn spec_3_6_message_whose_body_is_not_utf8_fails_the_decode_and_resets_the_stream() {
        let rig = rig().await;
        let connection = raw_connection(&rig).await;
        let mut request_bytes = seal_request(
            &keys(),
            Request::Message(message_to(card_of(SERVER_SEED), BODY_BEFORE_CORRUPTION)),
        )
        .unwrap();
        let body_start = request_bytes
            .windows(BODY_BEFORE_CORRUPTION.len())
            .position(|window| window == BODY_BEFORE_CORRUPTION.as_bytes())
            .unwrap();
        request_bytes[body_start..body_start + INVALID_UTF8.len()].copy_from_slice(&INVALID_UTF8);

        let outcome = raw_exchange(&connection, &request_bytes).await;

        assert_eq!(
            reset_code_of(&outcome),
            Some(VarInt::from_u32(UNANSWERED_RESET_CODE_ON_THE_WIRE))
        );
        assert_eq!(rig.host.received().len(), 0);
    }

    const SPEC_TOO_LARGE_CODE: &str = "too_large";
    const SPEC_REFUSED_CODE: &str = "refused";
    const UNLISTED_REFUSAL_REASON: &str = "a reason estafette never writes";
    const ACK_REFUSALS_APART: usize = 3;
    const SERVED_OUTCOMES_APART: usize = 2;
    const ACK_READ_FAILURES_APART: usize = 3;
    const UNANSWERED_RESET_CODE: u32 = 3;

    fn message_ack(accepted: bool, reason: Option<&str>) -> MessageAck {
        MessageAck {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            accepted,
            attached: false,
            reason: reason.map(str::to_owned),
        }
    }

    #[test]
    fn observability_decision_9_an_acknowledgements_reason_is_read_as_a_code_estafette_writes_or_as_unlisted()
     {
        let cases = [
            (Some(SPEC_TOO_LARGE_CODE), AckRefusal::TooLarge),
            (Some(SPEC_REFUSED_CODE), AckRefusal::Refused),
            (Some(UNLISTED_REFUSAL_REASON), AckRefusal::Unlisted),
            (None, AckRefusal::Unlisted),
        ];

        assert_each_arm(&cases, |reason| AckRefusal::of(*reason), ACK_REFUSALS_APART);
    }

    #[test]
    fn observability_decision_9_every_reason_but_the_two_codes_estafette_writes_reads_as_unlisted()
    {
        let named = [SPEC_TOO_LARGE_CODE, SPEC_REFUSED_CODE];
        for case in 0..CATCH_ALL_CASES {
            let reason = text_beside(&named, &mut Draws::seeded(case));
            if named.contains(&reason.as_str()) {
                continue;
            }
            assert_eq!(
                AckRefusal::of(Some(&reason)),
                AckRefusal::Unlisted,
                "reason {reason:?}"
            );
        }
    }

    #[test]
    fn observability_decision_9_a_message_acknowledgement_is_read_as_acknowledged_or_refused_with_its_code()
     {
        let cases = [
            (message_ack(true, None), SentOutcome::Acknowledged),
            (
                message_ack(false, Some(SPEC_TOO_LARGE_CODE)),
                SentOutcome::Refused(AckRefusal::TooLarge),
            ),
            (
                message_ack(false, None),
                SentOutcome::Refused(AckRefusal::Unlisted),
            ),
        ];

        assert_each_arm(&cases, SentOutcome::of_message_ack, cases.len());
    }

    #[test]
    fn observability_decision_9_each_response_served_is_read_as_answered_or_as_a_refused_message() {
        let cases = [
            (
                Response::MessageAck(message_ack(true, None)),
                ServedOutcome::Answered,
            ),
            (
                Response::MessageAck(message_ack(false, Some(SPEC_REFUSED_CODE))),
                ServedOutcome::Refused(AckRefusal::Refused),
            ),
            (
                Response::ProbeAck(ProbeAck {
                    v: Version::CURRENT,
                    nonce: PROBE_NONCE,
                    attached: false,
                    mac: ZERO_MAC,
                }),
                ServedOutcome::Answered,
            ),
            (
                Response::IntroduceAck(own_introduction(false)),
                ServedOutcome::Answered,
            ),
        ];

        assert_each_arm(&cases, ServedOutcome::of, SERVED_OUTCOMES_APART);
    }

    #[test]
    fn observability_decision_9_each_request_is_read_as_its_kind_and_a_message_with_its_id() {
        let cases = [
            (
                Request::Message(message_to(card_of(SERVER_SEED), BODY)),
                RequestSummary::Message(MESSAGE_ID),
            ),
            (
                Request::Probe(Probe {
                    v: Version::CURRENT,
                    nonce: PROBE_NONCE,
                    mac: ZERO_MAC,
                }),
                RequestSummary::Probe,
            ),
            (
                Request::Introduce(introduction_from_client()),
                RequestSummary::Introduce,
            ),
        ];

        assert_each_arm(&cases, RequestSummary::of, cases.len());
    }

    #[test]
    fn observability_decision_9_each_way_reading_an_acknowledgement_fails_is_read_as_the_failure_it_is()
     {
        let cases = [
            (ReadToEndError::TooLong, DirectError::OversizedAck),
            (
                ReadToEndError::Read(ReadError::Reset(VarInt::from_u32(UNANSWERED_RESET_CODE))),
                DirectError::Refused,
            ),
            (
                ReadToEndError::Read(ReadError::ConnectionLost(ConnectionError::Reset)),
                DirectError::Unreachable,
            ),
            (
                ReadToEndError::Read(ReadError::ClosedStream),
                DirectError::Unreachable,
            ),
            (
                ReadToEndError::Read(ReadError::ZeroRttRejected),
                DirectError::Unreachable,
            ),
        ];

        assert_each_arm(
            &cases,
            |failure| DirectError::from(failure.clone()),
            ACK_READ_FAILURES_APART,
        );
    }
}
