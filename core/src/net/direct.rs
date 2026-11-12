use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use iroh::endpoint::{
    Closed, ClosedStream, Connection, ConnectionError, ReadError, ReadToEndError, RecvStream,
    SendStream, VarInt, WeakConnectionHandle, WriteError,
};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointId};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::{Instant, sleep, timeout};

use super::endpoint::{DIRECT_ALPN, NodeEndpoint, QUIC_KEEP_ALIVE_INTERVAL};
use crate::crypto::NetworkKeys;
use crate::errors::ErrorCode;
use crate::payload::{
    PayloadError, ZERO_MAC, open_request, open_response, seal_request, seal_response,
};
use crate::peers::{admissible_introduction_handle, announced_sender_handle};
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
const SUPERSEDED_CONNECTION_CLOSE_REASON: &[u8] = b"superseded";
const HEARING_SAMPLES_PER_KEEP_ALIVE: u32 = 2;
const UNANSWERED_KEEP_ALIVES_OF_A_GONE_PROCESS: u32 = 3;
/// How often the register reads what each held connection has heard.
const HEARING_SAMPLE_INTERVAL: Duration =
    match QUIC_KEEP_ALIVE_INTERVAL.checked_div(HEARING_SAMPLES_PER_KEEP_ALIVE) {
        Some(interval) => interval,
        None => QUIC_KEEP_ALIVE_INTERVAL,
    };
/// How long a connection hears nothing before it is taken for one of a process that no longer runs.
const GONE_PROCESS_SILENCE: Duration =
    QUIC_KEEP_ALIVE_INTERVAL.saturating_mul(UNANSWERED_KEEP_ALIVES_OF_A_GONE_PROCESS);

/// What the direct server needs from the node: its identity and state, and the effects of a received Message and Introduce.
pub trait DirectHost: Send + Sync + 'static {
    /// Whether the process is Stopping, so that Messages are refused and Probes and Introduces go unanswered.
    fn is_stopping(&self) -> bool;

    /// The card public key of this process, or none for a node that holds no card and so refuses every Message.
    fn own_card(&self) -> Option<wire::PublicKey>;

    /// Whether an inbox client is attached to this process.
    fn is_attached(&self) -> bool;

    /// Applies the Message rows of spec 3.8 to the sender and enqueues the validated message in the inbox.
    fn receive_message(&self, sender: wire::PublicKey, message: Message);

    /// Applies the Introduce rows of spec 3.8 to the sender and answers with this node's own introduction.
    fn receive_introduce(
        &self,
        sender: wire::PublicKey,
        introduce: wire::Introduce,
    ) -> IntroduceAck;
}

/// A monotonic clock, read for the instant now.
pub type MonotonicClock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// Every connection the endpoint's gate let through and has not seen close, of any protocol, dialed or accepted, by the card at its other end, and the clock their silence is measured on.
#[derive(Clone)]
pub struct HeldConnections {
    held: Arc<Mutex<HashMap<wire::PublicKey, Vec<HeldConnection>>>>,
    monotonic: MonotonicClock,
}

impl fmt::Debug for HeldConnections {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HeldConnections")
            .finish_non_exhaustive()
    }
}

impl HeldConnections {
    /// An empty register that measures each connection's silence on the monotonic clock.
    #[must_use]
    pub fn new(monotonic: MonotonicClock) -> Self {
        Self {
            held: Arc::default(),
            monotonic,
        }
    }

    /// Closes every connection to or from the card, of any protocol, whose process no longer runs, so the endpoint forgets the path it held to that process (spec 3.8, address lookup).
    pub(crate) fn close_silent_of(&self, card: wire::PublicKey) {
        self.close_silent_of_sparing(card, None);
    }

    /// Closes every connection to or from the card whose process no longer runs, but the one an Introduce arrived on (spec 3.8, address lookup).
    fn close_silent_others_of(&self, card: wire::PublicKey, arrived_on: &Connection) {
        self.close_silent_of_sparing(card, Some(arrived_on.stable_id()));
    }

    /// Closes every connection to or from the card that has heard nothing from its peer for as long as a gone process is silent, but the spared one.
    fn close_silent_of_sparing(&self, card: wire::PublicKey, spared: Option<usize>) {
        let now = (self.monotonic)();
        let superseded = self
            .held()
            .get_mut(&card)
            .map(|of_card| {
                of_card
                    .extract_if(.., |held_connection| {
                        Some(held_connection.stable_id) != spared
                            && held_connection.is_from_a_gone_process(now)
                    })
                    .filter_map(|held_connection| held_connection.connection.upgrade())
                    .collect()
            })
            .unwrap_or_default();
        close_superseded(superseded);
    }

    /// Holds a connection the gate just let through and follows what it hears until it closes, keeping it no more alive than its users do.
    pub(crate) fn hold(&self, connection: &Connection) {
        let card = *connection.remote_id().as_bytes();
        let weak = connection.weak_handle();
        let closed = weak.closed();
        self.add(
            card,
            HeldConnection::heard_at(connection, (self.monotonic)()),
        );
        tokio::spawn(
            self.clone()
                .follow_hearing(card, connection.stable_id(), weak, closed),
        );
    }

    /// Adds the connection under its card, forgetting every card left with no connection.
    fn add(&self, card: wire::PublicKey, held_connection: HeldConnection) {
        let mut held = self.held();
        held.retain(|_, of_card| !of_card.is_empty());
        held.entry(card).or_default().push(held_connection);
    }

    /// Reads what the connection to or from the card has heard every hearing sample interval until it closes, then forgets it.
    async fn follow_hearing(
        self,
        card: wire::PublicKey,
        stable_id: usize,
        connection: WeakConnectionHandle,
        closed: impl Future<Output = Option<Closed>>,
    ) {
        tokio::pin!(closed);
        loop {
            tokio::select! {
                _closed = &mut closed => break,
                () = sleep(HEARING_SAMPLE_INTERVAL) => self.note_hearing(card, stable_id, &connection),
            }
        }
        self.forget(card, stable_id);
    }

    /// Takes now as the last time the connection to or from the card heard from its peer, when it received a datagram since its last reading.
    fn note_hearing(
        &self,
        card: wire::PublicKey,
        stable_id: usize,
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
                .find(|held_connection| held_connection.stable_id == stable_id)
        }) {
            held_connection.note_hearing(datagrams, now);
        }
    }

    /// Forgets a connection to or from the card that has closed.
    fn forget(&self, card: wire::PublicKey, stable_id: usize) {
        if let Some(of_card) = self.held().get_mut(&card) {
            of_card.retain(|held_connection| held_connection.stable_id != stable_id);
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
    stable_id: usize,
    datagrams_heard: u64,
    last_heard: Instant,
}

impl HeldConnection {
    /// The connection, taken as heard from its peer at `now`.
    fn heard_at(connection: &Connection, now: Instant) -> Self {
        Self {
            connection: connection.weak_handle(),
            stable_id: connection.stable_id(),
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

    /// Whether the peer has been silent on the connection for as long as a process that no longer runs is, which a live peer answering keep-alives never is.
    fn is_from_a_gone_process(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_heard) >= GONE_PROCESS_SILENCE
    }
}

/// Closes each connection as superseded by a newer one of its card.
fn close_superseded(connections: Vec<Connection>) {
    for connection in connections {
        connection.close(
            VarInt::from_u32(SUPERSEDED_CONNECTION_CLOSE_CODE),
            SUPERSEDED_CONNECTION_CLOSE_REASON,
        );
    }
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

#[derive(Debug, Clone, Copy)]
enum StreamReset {
    OverWireLimit,
    ReadTimeout,
    Unanswered,
}

impl StreamReset {
    const fn code(self) -> VarInt {
        VarInt::from_u32(match self {
            Self::OverWireLimit => RESET_CODE_OVER_WIRE_LIMIT,
            Self::ReadTimeout => RESET_CODE_READ_TIMEOUT,
            Self::Unanswered => RESET_CODE_UNANSWERED,
        })
    }
}

#[derive(Debug)]
enum StreamEnd {
    Reset(StreamReset),
    PeerGone,
}

const UNANSWERED: StreamEnd = StreamEnd::Reset(StreamReset::Unanswered);

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
    /// Answers the one request on the stream, or resets the stream when no answer is due.
    async fn serve_stream(
        &self,
        sender: wire::PublicKey,
        arrived_on: &Connection,
        mut send: SendStream,
        recv: RecvStream,
    ) {
        match self
            .answer_stream(sender, arrived_on, &mut send, recv)
            .await
        {
            Ok(()) | Err(StreamEnd::PeerGone) => {}
            Err(StreamEnd::Reset(reset)) => match send.reset(reset.code()) {
                Ok(()) | Err(ClosedStream { .. }) => {}
            },
        }
    }

    /// Reads the request, writes the response and finishes the send side.
    async fn answer_stream(
        &self,
        sender: wire::PublicKey,
        arrived_on: &Connection,
        send: &mut SendStream,
        mut recv: RecvStream,
    ) -> Result<(), StreamEnd> {
        let request_bytes = self.read_request(&mut recv).await?;
        let response_bytes = self.respond(sender, arrived_on, &request_bytes)?;
        send.write_all(&response_bytes).await?;
        Ok(send.finish()?)
    }

    /// Reads the whole request within the wire limit and the read timeout.
    async fn read_request(&self, recv: &mut RecvStream) -> Result<Vec<u8>, StreamEnd> {
        match timeout(SERVER_READ_TIMEOUT, recv.read_to_end(WIRE_LIMIT_BYTES)).await {
            Err(_elapsed) => Err(StreamEnd::Reset(StreamReset::ReadTimeout)),
            Ok(Err(ReadToEndError::TooLong)) => Err(StreamEnd::Reset(StreamReset::OverWireLimit)),
            Ok(Err(ReadToEndError::Read(failure))) => Err(failure.into()),
            Ok(Ok(bytes)) => Ok(bytes),
        }
    }

    /// Opens the request, answers it, and seals the answer, or reports that none is due.
    fn respond(
        &self,
        sender: wire::PublicKey,
        arrived_on: &Connection,
        request_bytes: &[u8],
    ) -> Result<Vec<u8>, StreamEnd> {
        let request = open_request(&self.keys, request_bytes).map_err(|_failure| UNANSWERED)?;
        let response = self.answer(sender, arrived_on, request)?;
        seal_response(&self.keys, response).map_err(|_failure| UNANSWERED)
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
            Request::Probe(_) | Request::Introduce(_) if self.host.is_stopping() => Err(UNANSWERED),
            Request::Introduce(introduce)
                if admissible_introduction_handle(
                    introduce.handle.as_deref(),
                    &introduce.public,
                )
                .is_err() =>
            {
                Err(UNANSWERED)
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

    /// Queues the message when it is acceptable and acknowledges it, accepted or with the reason it was not.
    fn answer_message(&self, sender: wire::PublicKey, message: Message) -> MessageAck {
        let id = message.id;
        let rejection = judge_message(self.host.own_card(), self.host.is_stopping(), &message);
        if rejection.is_none() {
            self.host.receive_message(sender, message);
        }
        MessageAck {
            v: Version::CURRENT,
            id,
            accepted: rejection.is_none(),
            attached: self.host.is_attached(),
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
}

impl DirectClient {
    /// Builds the client over the endpoint, macing requests and checking acknowledgements under the network keys.
    #[must_use]
    pub fn new(endpoint: Endpoint, keys: NetworkKeys, timeouts: ClientTimeouts) -> Self {
        Self {
            endpoint: Mutex::new(endpoint),
            keys,
            timeouts,
            slots: Mutex::new(HashMap::new()),
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
        match self
            .exchange(
                to,
                Request::Message(message),
                AckDue::FromTheConnection(self.timeouts.ack),
            )
            .await?
        {
            Response::MessageAck(ack) if ack.id == id => Ok(ack),
            Response::MessageAck(_) => Err(DirectError::MessageAckIdMismatch),
            Response::ProbeAck(_) | Response::IntroduceAck(_) => Err(DirectError::UnexpectedAck),
        }
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
        match self
            .exchange(
                to,
                Request::Probe(probe),
                AckDue::FromTheCall(self.timeouts.probe),
            )
            .await?
        {
            Response::ProbeAck(ack) if ack.nonce == nonce => Ok(ack),
            Response::ProbeAck(_) => Err(DirectError::ProbeNonceMismatch),
            Response::MessageAck(_) | Response::IntroduceAck(_) => Err(DirectError::UnexpectedAck),
        }
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
        match self
            .exchange(
                to,
                Request::Introduce(introduce),
                AckDue::FromTheConnection(self.timeouts.ack),
            )
            .await?
        {
            Response::IntroduceAck(ack) => Ok(ack),
            Response::MessageAck(_) | Response::ProbeAck(_) => Err(DirectError::UnexpectedAck),
        }
    }

    /// Sends the request over the held or a freshly dialed connection and returns the valid response when it is due, dropping that connection from its slot on any failure.
    async fn exchange(
        &self,
        to: EndpointId,
        request: Request,
        due: AckDue,
    ) -> Result<Response, DirectError> {
        let sealed = seal_request(&self.keys, request).map_err(DirectError::UnsealableRequest)?;
        let called = Instant::now();
        let connection = self
            .connection_to(to, due.connection_timeout(self.timeouts.connect))
            .await?;
        let response = self
            .round_trip(&connection, &sealed, due.ack_timeout_left(called.elapsed()))
            .await;
        if response.is_err() {
            self.forget_connection(to, &connection).await;
        }
        response
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

    /// The held connection to the peer, or a new one dialed by card id and held, unreachable when neither is in hand within the connection timeout.
    async fn connection_to(
        &self,
        to: EndpointId,
        connection_timeout: Duration,
    ) -> Result<Connection, DirectError> {
        timeout(connection_timeout, self.held_or_dialed_connection(to))
            .await
            .map_err(|_elapsed| DirectError::Unreachable)?
    }

    /// The held connection to the peer, or a new one dialed by card id under the slot's lock and held, so concurrent callers share one dial.
    async fn held_or_dialed_connection(&self, to: EndpointId) -> Result<Connection, DirectError> {
        let slot = self.slot_of(to);
        let mut held = slot.lock().await;
        if let Some(connection) = held.as_ref() {
            return Ok(connection.clone());
        }
        let dialed = self
            .endpoint()
            .connect(to, DIRECT_ALPN)
            .await
            .map_err(|_failure| DirectError::Unreachable)?;
        Ok(held.insert(dialed).clone())
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
            .is_some_and(|connection| connection.stable_id() == failed.stable_id())
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
mod tests {
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

    const NETWORK_SECRET: [u8; KEY_LEN] = [7; KEY_LEN];
    const SERVER_SEED: u8 = 1;
    const CLIENT_SEED: u8 = 2;
    const MESSAGE_ID: [u8; 16] = [9; 16];
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

        fn receive_message(&self, sender: wire::PublicKey, message: Message) {
            self.received.lock().unwrap().push((sender, message));
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
        bind_node_holding(
            seed,
            HeldConnections::new(Arc::new(tokio::time::Instant::now)),
        )
        .await
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
        let client = DirectClient::new(client_node.endpoint().clone(), keys(), timeouts);
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
        let server_node = bind_node_holding(SERVER_SEED, HeldConnections::new(monotonic)).await;
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
            Some(StreamReset::Unanswered.code())
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
            Some(StreamReset::Unanswered.code())
        );
        assert_eq!(rig.host.received().len(), 0);
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
            Some(StreamReset::ReadTimeout.code())
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

    const PROBE_NONCE: Nonce = [5; 16];
    const SENDER_DIRECT_ADDRESS: &str = "127.0.0.1:4000";
    const LOOPBACK_ANY_PORT: &str = "127.0.0.1:0";
    const SPEC_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
    const SPEC_ACK_TIMEOUT: Duration = Duration::from_secs(10);
    const SPEC_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
    const TIMEOUT_SLACK: Duration = Duration::from_secs(1);
    const SHORT_TIMEOUT: Duration = Duration::from_millis(300);

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
        let register = HeldConnections::new(clock_reading_every_connection_as_gone());
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

    const SILENCE_OF_A_GONE_PROCESS: Duration = Duration::from_millis(1500);
    const JUST_BEFORE: Duration = Duration::from_millis(1);
    const JUST_UNDER_THE_SILENCE_OF_A_GONE_PROCESS: Duration =
        SILENCE_OF_A_GONE_PROCESS.saturating_sub(JUST_BEFORE);

    #[tokio::test]
    async fn spec_3_8_a_connection_silent_just_under_three_keep_alive_intervals_is_kept_and_one_silent_for_them_is_closed()
     {
        let held_at = tokio::time::Instant::now();
        let reading = Arc::new(Mutex::new(held_at));
        let register = HeldConnections::new({
            let reading = Arc::clone(&reading);
            Arc::new(move || *reading.lock().unwrap())
        });
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
            .map(|held_connection| {
                held_connection
                    .connection
                    .upgrade()
                    .is_some_and(|live| live.paths().iter().any(|path| path.is_selected()))
            })
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
        let this_register = HeldConnections::new(Arc::new(tokio::time::Instant::now));
        let peer_register = HeldConnections::new(Arc::new(tokio::time::Instant::now));
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
                            send.reset(StreamReset::Unanswered.code()).unwrap();
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

    const ANOTHER_MESSAGE_ID: [u8; 16] = [10; 16];

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
            ScriptedServer::replying(Some(sealed_accepting_message_ack(ANOTHER_MESSAGE_ID)));
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
        let answer_with_other_nonce = [6; 16];
        let server = ScriptedServer::replying(Some(sealed_probe_ack(answer_with_other_nonce)));
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
        let client =
            DirectClient::new(client_node.endpoint().clone(), keys(), ClientTimeouts::SPEC);
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
            Some(StreamReset::Unanswered.code())
        );
        assert_eq!(rig.host.received().len(), 0);
    }
}
