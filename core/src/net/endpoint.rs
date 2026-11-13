use std::sync::Arc;
use std::time::Duration;

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{
    BindError, ConnectionError, QuicTransportConfig, RelayStatus, Side, VarInt, presets,
};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey,
    TransportAddr,
};
use iroh_gossip::ALPN as GOSSIP_ALPN;

use super::direct::{HeldConnections, SUPERSEDED_CONNECTION_CLOSE_REASON};
use super::gate::{FillGateNonce, GateNonce, NetworkSecretGate, ReportGateClosing};
use crate::crypto::GateKey;
use crate::event::{RunRecord, TransportEvent};
use crate::trace::LocationSalt;

pub use super::gate::GateFailure;

pub const DIRECT_ALPN: &[u8] = b"estafette/direct/1";
const SPEC_3_14_QUIC_IDLE_TIMEOUT_MILLIS: VarInt = VarInt::from_u32(10_000);
const QUIC_KEEP_ALIVES_PER_IDLE_TIMEOUT: u64 = 20;
pub const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(
    SPEC_3_14_QUIC_IDLE_TIMEOUT_MILLIS.into_inner() / QUIC_KEEP_ALIVES_PER_IDLE_TIMEOUT,
);
const LOCATION_HASH_BYTES: usize = 8;
const PEER_ID_BYTES: usize = 5;
const GATE_CLOSE_REASON: &[u8] = b"gate";

/// The node's iroh endpoint, gated on the network secret for every gossip connection, with the address lookup the node feeds and the relay map it edits, both kept across rebinds.
pub struct NodeEndpoint {
    endpoint: Endpoint,
    address_lookup: MemoryLookup,
    relay_map: RelayMap,
    binding: Binding,
}

/// What every endpoint of the node is bound with: the card key and the gate's key, nonce source, closing sink and connection register, the source and the sink shared with each gate a rebind builds.
struct Binding {
    secret_key: SecretKey,
    gate_key: GateKey,
    fill_nonce: FillGateNonce,
    report_gate_closing: ReportGateClosing,
    connections: HeldConnections,
}

impl NodeEndpoint {
    /// Binds an endpoint under the key with the relays, the fed lookup as its only address source, and the network secret gate on every gossip connection, which hands each closing reason to the sink and holds every connection it lets through in the register.
    ///
    /// # Errors
    /// The iroh bind error when no socket can be bound.
    pub async fn bind(
        secret_key: SecretKey,
        gate_key: GateKey,
        relays: impl IntoIterator<Item = RelayUrl>,
        fill_nonce: impl Fn(&mut GateNonce) + Send + Sync + 'static,
        report_gate_closing: impl Fn(GateFailure) + Send + Sync + 'static,
        connections: HeldConnections,
    ) -> Result<Self, BindError> {
        let relay_map: RelayMap = relays.into_iter().collect();
        let address_lookup = MemoryLookup::new();
        let binding = Binding {
            secret_key,
            gate_key,
            fill_nonce: Arc::new(fill_nonce),
            report_gate_closing: Arc::new(report_gate_closing),
            connections,
        };
        let endpoint = bind_endpoint(&binding, &relay_map, &address_lookup).await?;
        Ok(Self {
            endpoint,
            address_lookup,
            relay_map,
            binding,
        })
    }

    /// Closes the endpoint, a no-op when a router's shutdown has closed it already, and binds a fresh one in its place under the same key, relay map, lookup and gate (spec 3.8, Rebind).
    ///
    /// # Errors
    /// The iroh bind error when no socket can be bound; the closed endpoint then stays in place.
    pub(crate) async fn rebind(&mut self) -> Result<(), BindError> {
        self.endpoint.close().await;
        self.endpoint = bind_endpoint(&self.binding, &self.relay_map, &self.address_lookup).await?;
        Ok(())
    }

    /// The register holding every connection this endpoint and each endpoint it rebinds to lets through, of any protocol, dialed or accepted.
    #[must_use]
    pub(crate) fn held_connections(&self) -> HeldConnections {
        self.binding.connections.clone()
    }

    /// Records each home relay's connection status, as the node reads it.
    pub(crate) fn record_home_relays(&self, statuses: &[RelayStatus]) {
        for status in statuses {
            self.record(|transport| TransportEvent::HomeRelay {
                location: transport.locate(&TransportAddr::Relay(status.url().clone())),
                connected: status.is_connected(),
            });
        }
    }

    /// Hands the transport event, built against the transport record, to the record the endpoint's register holds, when it records.
    pub(crate) fn record(&self, event: impl FnOnce(&TransportRecord) -> TransportEvent) {
        self.binding.connections.record(event);
    }

    /// The iroh endpoint the gossip layer and the direct protocol run on.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Replaces, never merges, the lookup entry of the address's card, so a dial by card id goes there alone.
    pub fn replace_address(&self, address: EndpointAddr) {
        self.address_lookup.set_endpoint_info(address);
    }

    /// The address the lookup holds for the node, if any.
    #[must_use]
    pub(crate) fn address_of(&self, id: EndpointId) -> Option<EndpointAddr> {
        self.address_lookup
            .get_endpoint_info(id)
            .map(EndpointAddr::from)
    }

    /// Adds the relay to the relay map unless the map already holds it, so a held relay keeps its entry.
    pub async fn add_relay(&self, relay: RelayUrl) {
        if !self.relay_map.contains(&relay) {
            let entry = Arc::new(RelayConfig::from(relay.clone()));
            self.endpoint.insert_relay(relay, entry).await;
        }
    }

    /// Removes the relay from the relay map.
    pub async fn remove_relay(&self, relay: &RelayUrl) {
        self.endpoint.remove_relay(relay).await;
    }
}

/// Binds an endpoint with the binding's key and gate, the relay map, and the lookup as its only address source.
async fn bind_endpoint(
    binding: &Binding,
    relay_map: &RelayMap,
    address_lookup: &MemoryLookup,
) -> Result<Endpoint, BindError> {
    let gate = NetworkSecretGate::new(
        binding.gate_key,
        binding.secret_key.public(),
        Arc::clone(&binding.fill_nonce),
        Arc::clone(&binding.report_gate_closing),
        binding.connections.clone(),
    );
    Endpoint::builder(presets::Minimal)
        .secret_key(binding.secret_key.clone())
        .alpns(vec![DIRECT_ALPN.to_vec(), GOSSIP_ALPN.to_vec()])
        .relay_mode(RelayMode::Custom(relay_map.clone()))
        .transport_config(quic_transport_config())
        .address_lookup(address_lookup.clone())
        .hooks(gate)
        .bind()
        .await
}

/// The QUIC transport config with the idle timeout of spec 3.14 and the keep-alive interval.
fn quic_transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .keep_alive_interval(QUIC_KEEP_ALIVE_INTERVAL)
        .max_idle_timeout(Some(SPEC_3_14_QUIC_IDLE_TIMEOUT_MILLIS.into()))
        .build()
}

/// The run record a process hands its transport, with the salt that hides every network location it records.
#[derive(Clone)]
pub struct TransportRecord {
    record: RunRecord,
    salt: LocationSalt,
}

impl TransportRecord {
    /// The record that writes to the run record and hashes each location under the salt.
    #[must_use]
    pub fn new(record: RunRecord, salt: LocationSalt) -> Self {
        Self { record, salt }
    }

    /// Hands the event to the run record.
    pub fn record(&self, event: TransportEvent) {
        (self.record)(event.into());
    }

    /// The location of the address as this process's events file carries it.
    #[must_use]
    pub fn locate(&self, address: &TransportAddr) -> Location {
        Location::of(address, &self.salt)
    }
}

/// The salted hash of a network location: alike for one location under one salt, and never the location itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocationHash([u8; LOCATION_HASH_BYTES]);

impl LocationHash {
    /// The first bytes of the keyed hash of the location's text under the salt.
    #[must_use]
    pub fn of(salt: &LocationSalt, location_text: &str) -> Self {
        let mut hash = [0; LOCATION_HASH_BYTES];
        for (byte, hashed) in hash
            .iter_mut()
            .zip(salt.keyed_hash(location_text.as_bytes()).as_bytes())
        {
            *byte = *hashed;
        }
        Self(hash)
    }

    /// The hash as lowercase hex.
    #[must_use]
    pub fn hex(self) -> String {
        data_encoding::HEXLOWER.encode(&self.0)
    }
}

/// How a path reaches its peer: straight over IP, through a relay, or over any other transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationKind {
    Direct,
    Relay,
    Other,
}

/// A network location as an events file may carry it: its kind, its port, and its salted hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub kind: LocationKind,
    pub port: Option<u16>,
    pub hash: LocationHash,
}

impl Location {
    /// The location of a transport address, hashed under the salt.
    #[must_use]
    pub fn of(address: &TransportAddr, salt: &LocationSalt) -> Self {
        match address {
            TransportAddr::Ip(socket) => Self {
                kind: LocationKind::Direct,
                port: Some(socket.port()),
                hash: LocationHash::of(salt, &socket.to_string()),
            },
            TransportAddr::Relay(relay) => Self {
                kind: LocationKind::Relay,
                port: relay.port_or_known_default(),
                hash: LocationHash::of(salt, relay.as_str()),
            },
            other => Self {
                kind: LocationKind::Other,
                port: None,
                hash: LocationHash::of(salt, &format!("{other:?}")),
            },
        }
    }
}

/// The identifier iroh gives a connection for its whole life, the `conn_id` of iroh's own trace lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionId(pub usize);

/// A node named by the first five bytes of its endpoint id, the form iroh's own trace lines use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerId([u8; PEER_ID_BYTES]);

impl PeerId {
    /// The first five bytes of the node's endpoint id.
    #[must_use]
    pub fn of(node: EndpointId) -> Self {
        let mut short = [0; PEER_ID_BYTES];
        for (byte, id_byte) in short.iter_mut().zip(node.as_bytes()) {
            *byte = *id_byte;
        }
        Self(short)
    }

    /// The five bytes as lowercase hex.
    #[must_use]
    pub fn hex(self) -> String {
        data_encoding::HEXLOWER.encode(&self.0)
    }
}

/// Whether this side dialed a connection or accepted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Dialed,
    Accepted,
}

impl Direction {
    /// The direction of a connection this process is the given side of.
    #[must_use]
    pub const fn of(side: Side) -> Self {
        match side {
            Side::Client => Self::Dialed,
            Side::Server => Self::Accepted,
        }
    }
}

/// The protocol a connection carries: the direct protocol, gossip, or any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Direct,
    Gossip,
    Other,
}

impl Protocol {
    /// The protocol a connection negotiated under the ALPN.
    #[must_use]
    pub fn of(alpn: &[u8]) -> Self {
        if alpn == DIRECT_ALPN {
            Self::Direct
        } else if alpn == GOSSIP_ALPN {
            Self::Gossip
        } else {
            Self::Other
        }
    }
}

/// A close reason estafette itself writes, or any other reason by its length alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Superseded,
    Gate,
    Unlisted { bytes: usize },
}

impl CloseReason {
    /// The reason of these close reason bytes.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        if bytes == SUPERSEDED_CONNECTION_CLOSE_REASON {
            Self::Superseded
        } else if bytes == GATE_CLOSE_REASON {
            Self::Gate
        } else {
            Self::Unlisted { bytes: bytes.len() }
        }
    }
}

/// How a connection closed: by this side, by the peer with a code and reason, by a reset, at the idle timeout, or by a transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Close {
    ThisSide,
    Peer { code: u64, reason: CloseReason },
    PeerReset,
    IdleTimeout,
    TransportFailure,
}

impl Close {
    /// The close iroh reports for a connection.
    #[must_use]
    pub fn of(error: &ConnectionError) -> Self {
        match error {
            ConnectionError::LocallyClosed => Self::ThisSide,
            ConnectionError::ApplicationClosed(close) => Self::Peer {
                code: close.error_code.into_inner(),
                reason: CloseReason::of(&close.reason),
            },
            ConnectionError::ConnectionClosed(close) => Self::Peer {
                code: u64::from(close.error_code),
                reason: CloseReason::of(&close.reason),
            },
            ConnectionError::Reset => Self::PeerReset,
            ConnectionError::TimedOut => Self::IdleTimeout,
            ConnectionError::VersionMismatch
            | ConnectionError::TransportError(_)
            | ConnectionError::CidsExhausted => Self::TransportFailure,
        }
    }
}

/// What happened to a path of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathChange {
    Opened,
    Closed,
    Selected,
}

/// Why the node rebinds: it heard no gossip payload for the deaf timeout, or its join gained no neighbor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebindCause {
    Deaf,
    JoinGainedNoNeighbor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use iroh::endpoint::{
        ConnectError, ConnectWithOptsError, ConnectingError, Connection, ConnectionError,
        ReadExactError, WriteError,
    };
    use iroh::{EndpointId, TransportAddr};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::{Duration, Instant};
    use tokio::time::timeout;

    const SPEC_DIRECT_ALPN: &[u8] = b"estafette/direct/1";
    const SPEC_GATE_KEY_CONTEXT: &str = "messaging gate v1";
    const SPEC_GATE_TIMEOUT: Duration = Duration::from_millis(3000);
    const SPEC_GATE_TIMEOUT_WITH_SLACK: Duration = Duration::from_millis(4000);
    const PEER_LIVES_PAST_ITS_FIRST_KEEP_ALIVE: Duration = Duration::from_secs(6);
    const SPEC_QUIC_IDLE_TIMEOUT_EARLIEST: Duration = Duration::from_secs(9);
    const SPEC_QUIC_IDLE_TIMEOUT_LATEST: Duration = Duration::from_secs(11);
    const MEMBER_SEED: u8 = 1;
    const SECOND_MEMBER_SEED: u8 = 2;
    const OUTSIDER_SEED: u8 = 3;
    const SPEC_PEER_SEED: u8 = 4;
    const SILENT_PEER_SEED: u8 = 5;
    const STRANGER_SEED: u8 = 6;
    const VANISHING_PEER_SEED: u8 = 7;
    const SPEC_PEER_NONCE: [u8; KEY_LEN] = [44; KEY_LEN];
    const NONCE_DRAW_SPREAD: u8 = 0x9D;
    const CONNECTIONS_EACH_DRAWING_A_NONCE: u8 = 2;
    const GATE_CLOSE_CODE_ON_THE_WIRE: u32 = 403;
    const GATE_CLOSE_REASON_ON_THE_WIRE: &[u8] = b"gate";
    const NONCE_CUT_SHORT: &[u8] = b"short";
    const NO_STREAMS_GRANTED: VarInt = VarInt::from_u32(0);
    const PEER_CLOSE_CODE: VarInt = VarInt::from_u32(77);
    const PEER_CLOSE_REASON: &[u8] = b"gone before the challenge";
    const MEMBER_SECRET: [u8; KEY_LEN] = [21; KEY_LEN];
    const OUTSIDER_SECRET: [u8; KEY_LEN] = [12; KEY_LEN];
    const FIRST_STREAM_BYTES: &[u8] = b"the first stream after the gate";
    const FIRST_HOST_RELAY: &str = "https://relay-one.estafette.invalid/";
    const SECOND_HOST_RELAY: &str = "https://relay-two.estafette.invalid/";
    const UNKNOWN_HOST_RELAY: &str = "https://relay-unknown.estafette.invalid/";
    const LOOPBACK_ANY_PORT: &str = "127.0.0.1:0";
    const LOCAL_DIAL_WAIT: Duration = Duration::from_millis(2000);
    const PEER_STOP_CODE: VarInt = VarInt::from_u32(59);

    /// The nonce a node's source fills at its `draw`-th gate challenge: no two bytes alike, and no two draws alike.
    fn drawn_nonce(draw: u8) -> [u8; KEY_LEN] {
        std::array::from_fn(|index| {
            u8::try_from(index).unwrap() ^ draw.wrapping_mul(NONCE_DRAW_SPREAD)
        })
    }

    fn gate_key_of(secret: [u8; KEY_LEN]) -> GateKey {
        *NetworkKeys::derive(&NetworkSecret::from_bytes(secret)).gate_key()
    }

    async fn bind_node(seed: u8, secret: [u8; KEY_LEN]) -> NodeEndpoint {
        bind_node_with_relays(seed, secret, Vec::new()).await
    }

    async fn bind_node_with_relays(
        seed: u8,
        secret: [u8; KEY_LEN],
        relays: Vec<RelayUrl>,
    ) -> NodeEndpoint {
        bind_node_reporting_to(seed, secret, relays, |_ignored_reason: GateFailure| {}).await
    }

    async fn bind_node_reporting_to(
        seed: u8,
        secret: [u8; KEY_LEN],
        relays: Vec<RelayUrl>,
        report_gate_closing: impl Fn(GateFailure) + Send + Sync + 'static,
    ) -> NodeEndpoint {
        let draws = AtomicU8::new(0);
        NodeEndpoint::bind(
            SecretKey::from_bytes(&[seed; KEY_LEN]),
            gate_key_of(secret),
            relays,
            move |nonce: &mut [u8; KEY_LEN]| {
                *nonce = drawn_nonce(draws.fetch_add(1, Ordering::SeqCst));
            },
            report_gate_closing,
            HeldConnections::new(Arc::new(tokio::time::Instant::now)),
        )
        .await
        .unwrap()
    }

    async fn bind_node_recording_gate_closings(
        seed: u8,
        secret: [u8; KEY_LEN],
    ) -> (NodeEndpoint, Arc<Mutex<Vec<GateFailure>>>) {
        let closing_reasons = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&closing_reasons);
        let node = bind_node_reporting_to(seed, secret, Vec::new(), move |reason| {
            sink.lock().unwrap().push(reason);
        })
        .await;
        (node, closing_reasons)
    }

    fn relay_url(text: &str) -> RelayUrl {
        text.parse().unwrap()
    }

    async fn dial(
        dialer: &Endpoint,
        acceptor: &Endpoint,
        alpn: &[u8],
    ) -> (
        Result<Connection, ConnectError>,
        Result<Connection, ConnectingError>,
    ) {
        tokio::join!(dialer.connect(acceptor.addr(), alpn), accept_next(acceptor))
    }

    async fn accept_next(acceptor: &Endpoint) -> Result<Connection, ConnectingError> {
        acceptor.accept().await.unwrap().await
    }

    #[tokio::test]
    async fn spec_3_6_relay_map_holds_the_relays_of_the_known_hosts() {
        let host_relays = vec![relay_url(FIRST_HOST_RELAY), relay_url(SECOND_HOST_RELAY)];
        let member = bind_node_with_relays(MEMBER_SEED, MEMBER_SECRET, host_relays.clone()).await;

        for relay in &host_relays {
            assert!(member.endpoint().remove_relay(relay).await.is_some());
        }
        assert!(
            member
                .endpoint()
                .remove_relay(&relay_url(UNKNOWN_HOST_RELAY))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn spec_3_6_gate_admits_both_dial_directions_between_holders_of_the_secret() {
        let first = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let second = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;

        for (dialer, acceptor) in [(&first, &second), (&second, &first)] {
            let (dialed, accepted) =
                dial(dialer.endpoint(), acceptor.endpoint(), GOSSIP_ALPN).await;
            let (dialed, accepted) = (dialed.unwrap(), accepted.unwrap());
            assert_eq!(dialed.remote_id(), acceptor.endpoint().id());
            assert_eq!(accepted.remote_id(), dialer.endpoint().id());

            let (mut send, _unused_recv) = dialed.open_bi().await.unwrap();
            send.write_all(FIRST_STREAM_BYTES).await.unwrap();
            send.finish().unwrap();
            let (_unused_send, mut recv) = accepted.accept_bi().await.unwrap();
            assert_eq!(
                recv.read_to_end(FIRST_STREAM_BYTES.len()).await.unwrap(),
                FIRST_STREAM_BYTES
            );
        }
    }

    fn dial_is_locally_rejected(outcome: &Result<Connection, ConnectError>) -> bool {
        matches!(
            outcome,
            Err(ConnectError::Connecting {
                source: ConnectingError::LocallyRejected { .. },
                ..
            })
        )
    }

    fn accept_is_locally_rejected(outcome: &Result<Connection, ConnectingError>) -> bool {
        matches!(outcome, Err(ConnectingError::LocallyRejected { .. }))
    }

    fn is_the_gate_timeout(waited: Duration) -> bool {
        SPEC_GATE_TIMEOUT <= waited && waited < SPEC_GATE_TIMEOUT_WITH_SLACK
    }

    #[tokio::test]
    async fn spec_3_15_test_21_another_secret_is_closed_in_both_dial_directions() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let outsider = bind_node(OUTSIDER_SEED, OUTSIDER_SECRET).await;

        for (dialer, acceptor) in [(&member, &outsider), (&outsider, &member)] {
            let started = Instant::now();
            let (dialed, accepted) =
                dial(dialer.endpoint(), acceptor.endpoint(), GOSSIP_ALPN).await;
            assert!(started.elapsed() < SPEC_GATE_TIMEOUT);
            assert!(dial_is_locally_rejected(&dialed));
            assert!(accept_is_locally_rejected(&accepted));
        }
    }

    fn plain_endpoint_builder(seed: u8) -> iroh::endpoint::Builder {
        Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&[seed; KEY_LEN]))
            .alpns(vec![GOSSIP_ALPN.to_vec(), DIRECT_ALPN.to_vec()])
    }

    async fn bind_plain_endpoint(seed: u8) -> Endpoint {
        plain_endpoint_builder(seed).bind().await.unwrap()
    }

    /// A plain endpoint whose peers can open no bidirectional stream to it, so the gate's own challenge waits on the connection.
    async fn bind_plain_endpoint_granting_no_streams(seed: u8) -> Endpoint {
        plain_endpoint_builder(seed)
            .transport_config(
                QuicTransportConfig::builder()
                    .max_concurrent_bidi_streams(NO_STREAMS_GRANTED)
                    .build(),
            )
            .bind()
            .await
            .unwrap()
    }

    fn spec_gate_proof(
        secret: [u8; KEY_LEN],
        nonce: &[u8; KEY_LEN],
        verifier: EndpointId,
        prover: EndpointId,
    ) -> [u8; KEY_LEN] {
        let gate_key = blake3::derive_key(SPEC_GATE_KEY_CONTEXT, &secret);
        let message = [nonce.as_slice(), verifier.as_bytes(), prover.as_bytes()].concat();
        *blake3::keyed_hash(&gate_key, &message).as_bytes()
    }

    #[tokio::test]
    async fn spec_3_6_peer_speaking_the_spec_challenge_is_admitted_and_gets_the_spec_proof_under_a_fresh_nonce_each_connection()
     {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_plain_endpoint(SPEC_PEER_SEED).await;
        let (member_id, peer_id) = (member.endpoint().id(), peer.id());

        for draw in 0..CONNECTIONS_EACH_DRAWING_A_NONCE {
            let speak_the_spec_gate = async {
                let connection = peer
                    .connect(member.endpoint().addr(), GOSSIP_ALPN)
                    .await
                    .unwrap();
                let (mut challenge_send, mut proof_recv) = connection.open_bi().await.unwrap();
                challenge_send.write_all(&SPEC_PEER_NONCE).await.unwrap();
                challenge_send.finish().unwrap();
                let mut proof_received = [0; KEY_LEN];
                proof_recv.read_exact(&mut proof_received).await.unwrap();

                let (mut answer_send, mut nonce_recv) = connection.accept_bi().await.unwrap();
                let mut member_nonce = [0; KEY_LEN];
                nonce_recv.read_exact(&mut member_nonce).await.unwrap();
                let answer = spec_gate_proof(MEMBER_SECRET, &member_nonce, member_id, peer_id);
                answer_send.write_all(&answer).await.unwrap();
                answer_send.finish().unwrap();
                (connection, proof_received, member_nonce)
            };
            let ((_open_connection, proof_received, member_nonce), accepted) =
                tokio::join!(speak_the_spec_gate, accept_next(member.endpoint()));

            assert_eq!(
                proof_received,
                spec_gate_proof(MEMBER_SECRET, &SPEC_PEER_NONCE, peer_id, member_id)
            );
            assert_eq!(member_nonce, drawn_nonce(draw));
            assert_eq!(accepted.unwrap().remote_id(), peer_id);
        }
    }

    #[tokio::test]
    async fn spec_3_14_gate_timeout_closes_a_peer_that_never_answers_in_both_dial_directions() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let silent = bind_plain_endpoint(SILENT_PEER_SEED).await;

        let started = Instant::now();
        let (dialed, _held_by_silent) = tokio::join!(
            member.endpoint().connect(silent.addr(), GOSSIP_ALPN),
            accept_next(&silent)
        );
        assert!(is_the_gate_timeout(started.elapsed()));
        assert!(dial_is_locally_rejected(&dialed));

        let started = Instant::now();
        let (closed_by_member, accepted) = tokio::join!(
            async {
                let connection = silent
                    .connect(member.endpoint().addr(), GOSSIP_ALPN)
                    .await
                    .unwrap();
                connection.closed().await
            },
            accept_next(member.endpoint())
        );
        assert!(is_the_gate_timeout(started.elapsed()));
        assert!(accept_is_locally_rejected(&accepted));
        assert!(
            matches!(
                &closed_by_member,
                ConnectionError::ApplicationClosed(close)
                    if close.error_code == VarInt::from_u32(GATE_CLOSE_CODE_ON_THE_WIRE)
                        && close.reason.as_ref() == GATE_CLOSE_REASON_ON_THE_WIRE
            ),
            "{closed_by_member:?}"
        );
    }

    #[tokio::test]
    async fn spec_3_6_direct_alpn_accepts_any_node_id_without_the_gate() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let stranger = bind_plain_endpoint(STRANGER_SEED).await;

        let started = Instant::now();
        let (dialed, accepted) = dial(&stranger, member.endpoint(), DIRECT_ALPN).await;
        assert!(started.elapsed() < SPEC_GATE_TIMEOUT);
        let (dialed, accepted) = (dialed.unwrap(), accepted.unwrap());
        assert_eq!(accepted.remote_id(), stranger.id());
        assert_eq!(accepted.alpn(), SPEC_DIRECT_ALPN);
        assert_eq!(dialed.close_reason(), None);
        assert_eq!(
            dialed.remote_id(),
            SecretKey::from_bytes(&[MEMBER_SEED; KEY_LEN]).public()
        );
    }

    #[tokio::test]
    async fn spec_3_14_quic_idle_timeout_closes_a_connection_whose_peer_vanished() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let member_addr = member.endpoint().addr();
        let vanishing_peer = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let peer_and_connection = runtime.block_on(async move {
                let peer = bind_plain_endpoint(VANISHING_PEER_SEED).await;
                let connection = peer.connect(member_addr, DIRECT_ALPN).await.unwrap();
                tokio::time::sleep(PEER_LIVES_PAST_ITS_FIRST_KEEP_ALIVE).await;
                (peer, connection)
            });
            runtime.shutdown_background();
            peer_and_connection
        });

        let accepted = accept_next(member.endpoint()).await.unwrap();
        let _silenced_peer = tokio::task::spawn_blocking(move || vanishing_peer.join())
            .await
            .unwrap()
            .unwrap();
        let started = Instant::now();
        let closed = accepted.closed().await;
        let waited = started.elapsed();

        assert!(matches!(closed, ConnectionError::TimedOut));
        assert!(
            SPEC_QUIC_IDLE_TIMEOUT_EARLIEST <= waited && waited < SPEC_QUIC_IDLE_TIMEOUT_LATEST
        );
    }

    async fn dial_by_card_id(
        dialer: &NodeEndpoint,
        acceptor: &NodeEndpoint,
    ) -> Option<(Connection, Connection)> {
        let (dial_outcome, accept_outcome) = tokio::join!(
            timeout(
                LOCAL_DIAL_WAIT,
                dialer
                    .endpoint()
                    .connect(acceptor.endpoint().id(), DIRECT_ALPN)
            ),
            timeout(LOCAL_DIAL_WAIT, accept_next(acceptor.endpoint()))
        );
        Some((dial_outcome.ok()?.ok()?, accept_outcome.ok()?.ok()?))
    }

    #[tokio::test]
    async fn spec_3_8_dial_by_card_id_reaches_the_address_the_node_fed() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;

        member.replace_address(peer.endpoint().addr());

        let (dialed, accepted) = dial_by_card_id(&member, &peer).await.unwrap();
        assert_eq!(dialed.remote_id(), peer.endpoint().id());
        assert_eq!(accepted.remote_id(), member.endpoint().id());
    }

    #[tokio::test]
    async fn spec_3_6_no_discovery_service_finds_a_card_the_node_never_fed() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let unfed = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;

        let dialed = member
            .endpoint()
            .connect(unfed.endpoint().id(), DIRECT_ALPN)
            .await;

        assert!(matches!(
            dialed,
            Err(ConnectError::Connect {
                source: ConnectWithOptsError::NoAddress { .. },
                ..
            })
        ));
        assert_eq!(member.endpoint().address_lookup().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn spec_3_8_replacing_a_card_address_drops_the_old_one() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;
        let black_hole = std::net::UdpSocket::bind(LOOPBACK_ANY_PORT).unwrap();
        let unanswered_address = EndpointAddr::from_parts(
            peer.endpoint().id(),
            [TransportAddr::Ip(black_hole.local_addr().unwrap())],
        );

        member.replace_address(peer.endpoint().addr());
        member.replace_address(unanswered_address);
        assert!(dial_by_card_id(&member, &peer).await.is_none());

        member.replace_address(peer.endpoint().addr());
        assert!(dial_by_card_id(&member, &peer).await.is_some());
    }

    #[tokio::test]
    async fn spec_3_8_relay_loss_removes_a_relay_and_a_newer_record_adds_it_back() {
        let first_relay = relay_url(FIRST_HOST_RELAY);
        let second_relay = relay_url(SECOND_HOST_RELAY);
        let member =
            bind_node_with_relays(MEMBER_SEED, MEMBER_SECRET, vec![first_relay.clone()]).await;

        member.remove_relay(&first_relay).await;
        assert!(member.endpoint().remove_relay(&first_relay).await.is_none());

        member.add_relay(first_relay.clone()).await;
        assert!(member.endpoint().remove_relay(&first_relay).await.is_some());

        member.add_relay(second_relay.clone()).await;
        assert!(
            member
                .endpoint()
                .remove_relay(&second_relay)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn spec_3_8_adding_a_relay_the_map_holds_leaves_its_entry_untouched() {
        let first_relay = relay_url(FIRST_HOST_RELAY);
        let member =
            bind_node_with_relays(MEMBER_SEED, MEMBER_SECRET, vec![first_relay.clone()]).await;
        let entry_without_address_discovery = Arc::new(RelayConfig::new(first_relay.clone(), None));
        member
            .endpoint()
            .insert_relay(first_relay.clone(), entry_without_address_discovery)
            .await;

        member.add_relay(first_relay.clone()).await;

        let entry = member.endpoint().remove_relay(&first_relay).await.unwrap();
        assert_eq!(entry.quic, None);
    }

    #[tokio::test]
    async fn spec_3_8_a_rebound_endpoint_keeps_its_key_lookup_relay_map_and_gate_and_the_old_one_closes()
     {
        let added_relay = relay_url(SECOND_HOST_RELAY);
        let (mut member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;
        let outsider = bind_plain_endpoint(OUTSIDER_SEED).await;
        member.replace_address(peer.endpoint().addr());
        member.add_relay(added_relay.clone()).await;
        let before = member.endpoint().clone();

        member.rebind().await.unwrap();

        assert!(before.is_closed());
        assert!(!member.endpoint().is_closed());
        assert_eq!(member.endpoint().id(), before.id());
        assert!(member.endpoint().remove_relay(&added_relay).await.is_some());
        let (dialed, accepted) = dial_by_card_id(&member, &peer).await.unwrap();
        assert_eq!(dialed.remote_id(), peer.endpoint().id());
        assert_eq!(accepted.remote_id(), member.endpoint().id());
        let (dialed, _held_by_outsider) = tokio::join!(
            member.endpoint().connect(outsider.addr(), GOSSIP_ALPN),
            accept_next(&outsider)
        );
        assert!(dial_is_locally_rejected(&dialed));
        assert!(recorded_reasons_match(&recorded, |reason| matches!(
            reason,
            GateFailure::Timeout
        )));
    }

    fn recorded_reasons_match(
        recorded: &Mutex<Vec<GateFailure>>,
        expected: fn(&GateFailure) -> bool,
    ) -> bool {
        let reasons = recorded.lock().unwrap();
        reasons.len() == 1 && reasons.iter().all(expected)
    }

    #[tokio::test]
    async fn spec_3_6_gate_reports_a_wrong_answer_as_the_reason_it_closed() {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let outsider = bind_plain_endpoint(OUTSIDER_SEED).await;
        let (member_id, outsider_id) = (member.endpoint().id(), outsider.id());

        let answer_under_another_secret = async {
            let connection = outsider
                .connect(member.endpoint().addr(), GOSSIP_ALPN)
                .await
                .unwrap();
            let (mut answer_send, mut nonce_recv) = connection.accept_bi().await.unwrap();
            let mut member_nonce = [0; KEY_LEN];
            nonce_recv.read_exact(&mut member_nonce).await.unwrap();
            let answer = spec_gate_proof(OUTSIDER_SECRET, &member_nonce, member_id, outsider_id);
            answer_send.write_all(&answer).await.unwrap();
            answer_send.finish().unwrap();
            connection
        };
        let (_closed_connection, accepted) =
            tokio::join!(answer_under_another_secret, accept_next(member.endpoint()));

        assert!(accept_is_locally_rejected(&accepted));
        assert!(recorded_reasons_match(&recorded, |reason| matches!(
            reason,
            GateFailure::WrongProof
        )));
    }

    #[tokio::test]
    async fn spec_3_6_gate_reports_a_nonce_cut_short_as_the_stream_read_failure_it_closed_on() {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_plain_endpoint(SPEC_PEER_SEED).await;

        let challenge_with_a_nonce_cut_short = async {
            let connection = peer
                .connect(member.endpoint().addr(), GOSSIP_ALPN)
                .await
                .unwrap();
            let (mut challenge_send, _proof_recv) = connection.open_bi().await.unwrap();
            challenge_send.write_all(NONCE_CUT_SHORT).await.unwrap();
            challenge_send.finish().unwrap();
            connection
        };
        let (_closed_connection, accepted) = tokio::join!(
            challenge_with_a_nonce_cut_short,
            accept_next(member.endpoint())
        );

        assert!(accept_is_locally_rejected(&accepted));
        assert!(recorded_reasons_match(&recorded, |reason| matches!(
            reason,
            GateFailure::StreamRead(ReadExactError::FinishedEarly(read)) if *read == NONCE_CUT_SHORT.len()
        )));
    }

    #[tokio::test]
    async fn spec_3_6_gate_reports_a_proof_the_peer_stopped_reading_as_the_stream_write_failure_it_closed_on()
     {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_plain_endpoint(SPEC_PEER_SEED).await;

        let challenge_then_stop_reading_the_proof = async {
            let connection = peer
                .connect(member.endpoint().addr(), GOSSIP_ALPN)
                .await
                .unwrap();
            let (mut challenge_send, mut proof_recv) = connection.open_bi().await.unwrap();
            proof_recv.stop(PEER_STOP_CODE).unwrap();
            challenge_send.write_all(&SPEC_PEER_NONCE).await.unwrap();
            challenge_send.finish().unwrap();
            connection
        };
        let (_closed_connection, accepted) = tokio::join!(
            challenge_then_stop_reading_the_proof,
            accept_next(member.endpoint())
        );

        assert!(accept_is_locally_rejected(&accepted));
        assert!(recorded_reasons_match(&recorded, |reason| matches!(
            reason,
            GateFailure::StreamWrite(WriteError::Stopped(code)) if *code == PEER_STOP_CODE
        )));
    }

    #[tokio::test]
    async fn spec_3_14_gate_reports_the_timeout_as_the_reason_it_closed_on_a_silent_peer() {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let silent = bind_plain_endpoint(SILENT_PEER_SEED).await;

        let (dialed, _held_by_silent) = tokio::join!(
            member.endpoint().connect(silent.addr(), GOSSIP_ALPN),
            accept_next(&silent)
        );

        assert!(dial_is_locally_rejected(&dialed));
        assert!(recorded_reasons_match(&recorded, |reason| matches!(
            reason,
            GateFailure::Timeout
        )));
    }

    #[tokio::test]
    async fn spec_3_6_gate_reports_a_peer_that_closes_before_the_challenge_as_the_transport_failure_it_closed_on()
     {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_plain_endpoint_granting_no_streams(SPEC_PEER_SEED).await;

        let close_before_the_challenge = async {
            let connection = peer
                .connect(member.endpoint().addr(), GOSSIP_ALPN)
                .await
                .unwrap();
            connection.close(PEER_CLOSE_CODE, PEER_CLOSE_REASON);
            connection
        };
        let (_closed_connection, accepted) =
            tokio::join!(close_before_the_challenge, accept_next(member.endpoint()));

        assert!(accept_is_locally_rejected(&accepted));
        assert!(recorded_reasons_match(&recorded, |reason| matches!(
            reason,
            GateFailure::Transport(ConnectionError::ApplicationClosed(close))
                if close.error_code == PEER_CLOSE_CODE && close.reason == PEER_CLOSE_REASON
        )));
    }
}
