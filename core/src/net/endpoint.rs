use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{
    BindError, ConnectError, Connection, ConnectionError, QuicTransportConfig, RelayStatus, Side,
    VarInt, presets,
};
use iroh::metrics::SocketMetrics;
use iroh::protocol::{Router, RouterBuilder};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey,
    TransportAddr, Watcher,
};
use iroh_gossip::ALPN as GOSSIP_ALPN;
use iroh_gossip::Gossip;

use super::direct::{HeldConnections, SUPERSEDED_CONNECTION_CLOSE_REASON};
use super::gate::{
    FillGateNonce, GATE_REFUSAL_REASON, GateNonce, NetworkSecretGate, ReportGateClosing,
};
use crate::crypto::GateKey;
use crate::event::{RunRecord, ShortId, TransportEvent};
use crate::trace::LocationSalt;

pub use super::gate::{GateFailure, GateFailureReason};

pub const DIRECT_ALPN: &[u8] = b"estafette/direct/1";
const QUIC_IDLE_TIMEOUT_MILLIS: VarInt = VarInt::from_u32(10_000);
const QUIC_KEEP_ALIVES_PER_IDLE_TIMEOUT: u64 = 20;
pub const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(
    QUIC_IDLE_TIMEOUT_MILLIS.into_inner() / QUIC_KEEP_ALIVES_PER_IDLE_TIMEOUT,
);
const LOCATION_HASH_BYTES: usize = 8;

/// The node's iroh endpoint, gated on the network secret for every gossip connection, with the address lookup the node feeds and the relay map it edits, both kept across rebinds.
pub struct NodeEndpoint {
    endpoint: BoundEndpoint,
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
        let endpoint =
            BoundEndpoint::of(bind_endpoint(&binding, &relay_map, &address_lookup).await?);
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
        self.endpoint.iroh.close().await;
        self.endpoint = BoundEndpoint::of(
            bind_endpoint(&self.binding, &self.relay_map, &self.address_lookup).await?,
        );
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

    /// Hands the transport event, built against the transport record, to the record the endpoint's register holds.
    pub(crate) fn record<Recorded: Into<crate::event::Event>>(
        &self,
        event: impl FnOnce(&TransportRecord) -> Recorded,
    ) {
        self.binding.connections.record(event);
    }

    /// The bound endpoint the gossip layer and the direct protocol run on, without the means to edit its relays.
    #[must_use]
    pub const fn endpoint(&self) -> &BoundEndpoint {
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

    /// Adds the relay of the known host to the relay map unless the map already holds it, so a held relay keeps its entry, recording an addition.
    pub async fn add_relay(&self, relay: RelayUrl, host: ShortId) {
        if !self.relay_map.contains(&relay) {
            let entry = Arc::new(RelayConfig::from(relay.clone()));
            self.endpoint.iroh.insert_relay(relay.clone(), entry).await;
            self.record_relay_map_edit(RelayMapEdit::Added, Some(host), &relay);
        }
    }

    /// Removes the relay from the relay map, recording a removal when the map held it and naming the known host it belongs to, if any.
    pub async fn remove_relay(&self, relay: &RelayUrl, host: Option<ShortId>) {
        if self.relay_map.contains(relay) {
            self.endpoint.iroh.remove_relay(relay).await;
            self.record_relay_map_edit(RelayMapEdit::Removed, host, relay);
        }
    }

    /// Records an edit of the relay map: what it did, the known host the relay belongs to, and where the relay is.
    fn record_relay_map_edit(&self, edit: RelayMapEdit, host: Option<ShortId>, relay: &RelayUrl) {
        self.record(|transport| TransportEvent::RelayMapEdited {
            edit,
            host,
            location: transport.locate(&TransportAddr::Relay(relay.clone())),
        });
    }

    /// The relay map the endpoint dials through, shared, so a test reads each relay it gains or loses without editing it.
    #[cfg(test)]
    pub(crate) fn relay_map(&self) -> RelayMap {
        self.relay_map.clone()
    }
}

/// A bound iroh endpoint as every caller outside the relay guard sees it: it dials, accepts, names itself and carries gossip and routers, and no relay of its map is edited through it.
#[derive(Clone)]
pub struct BoundEndpoint {
    iroh: Endpoint,
}

impl BoundEndpoint {
    const fn of(iroh: Endpoint) -> Self {
        Self { iroh }
    }

    /// The id the endpoint is reached by.
    #[must_use]
    pub fn id(&self) -> EndpointId {
        self.iroh.id()
    }

    /// The address the endpoint is reached at now.
    #[must_use]
    pub fn addr(&self) -> EndpointAddr {
        self.iroh.addr()
    }

    /// Dials the address under the ALPN, as the endpoint's own connect does.
    ///
    /// # Errors
    /// The iroh connect error when the dial fails.
    pub async fn connect(
        &self,
        target: impl Into<EndpointAddr>,
        alpn: &[u8],
    ) -> Result<Connection, ConnectError> {
        self.iroh.connect(target, alpn).await
    }

    /// Each change of the endpoint's own address.
    #[must_use]
    pub(crate) fn watch_addr(&self) -> impl Watcher<Value = EndpointAddr> + use<> {
        self.iroh.watch_addr()
    }

    /// Each change of the endpoint's home relays and their connection status.
    #[must_use]
    pub(crate) fn home_relay_status(&self) -> impl Watcher<Value = Vec<RelayStatus>> + use<> {
        self.iroh.home_relay_status()
    }

    /// The counters iroh keeps for the endpoint's socket.
    #[must_use]
    pub(crate) fn socket_metrics(&self) -> &SocketMetrics {
        &self.iroh.metrics().socket
    }

    /// Whether the endpoint has been closed.
    #[cfg(test)]
    pub(crate) fn is_closed(&self) -> bool {
        self.iroh.is_closed()
    }

    /// Waits for the next connection attempt a peer makes to the endpoint.
    #[cfg(test)]
    pub(crate) fn accept(&self) -> iroh::endpoint::Accept<'_> {
        self.iroh.accept()
    }

    /// Closes the endpoint.
    #[cfg(test)]
    pub(crate) async fn close(&self) {
        self.iroh.close().await;
    }

    /// The address lookup the endpoint dials by.
    #[cfg(test)]
    pub(crate) fn address_lookup(
        &self,
    ) -> Result<&iroh::address_lookup::AddressLookupServices, iroh::endpoint::EndpointError> {
        self.iroh.address_lookup()
    }

    /// Puts the relay in the endpoint's map past the guard, so a test shows what the guard keeps out.
    #[cfg(test)]
    pub(crate) async fn insert_relay_past_the_guard(
        &self,
        relay: RelayUrl,
        entry: Arc<RelayConfig>,
    ) -> Option<Arc<RelayConfig>> {
        self.iroh.insert_relay(relay, entry).await
    }

    /// Takes the relay out of the endpoint's map past the guard, returning its entry if the map held it.
    #[cfg(test)]
    pub(crate) async fn remove_relay_past_the_guard(
        &self,
        relay: &RelayUrl,
    ) -> Option<Arc<RelayConfig>> {
        self.iroh.remove_relay(relay).await
    }

    /// Spawns a gossip instance that runs on this endpoint.
    #[must_use]
    pub fn spawn_gossip(&self) -> Gossip {
        Gossip::builder().spawn(self.iroh.clone())
    }

    /// Starts a router of protocol handlers on this endpoint.
    #[must_use]
    pub fn router_builder(&self) -> RouterBuilder {
        Router::builder(self.iroh.clone())
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
        .max_idle_timeout(Some(QUIC_IDLE_TIMEOUT_MILLIS.into()))
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
    pub fn record(&self, event: impl Into<crate::event::Event>) {
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
pub struct ConnectionId(usize);

impl ConnectionId {
    /// The identifier iroh gave the connection.
    #[must_use]
    pub(crate) fn of(connection: &Connection) -> Self {
        Self(connection.stable_id())
    }

    /// The identifier as the number iroh's trace lines print.
    #[must_use]
    pub(crate) const fn value(self) -> usize {
        self.0
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
        } else if bytes == GATE_REFUSAL_REASON {
            Self::Gate
        } else {
            Self::Unlisted { bytes: bytes.len() }
        }
    }
}

impl fmt::Display for CloseReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Superseded => formatter.write_str("the reason superseded"),
            Self::Gate => formatter.write_str("the reason gate"),
            Self::Unlisted { bytes } => write!(formatter, "a reason of {bytes} bytes"),
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

impl fmt::Display for Close {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThisSide => formatter.write_str("closed by this side"),
            Self::Peer { code, reason } => {
                write!(
                    formatter,
                    "closed by the peer with code {code} and {reason}"
                )
            }
            Self::PeerReset => formatter.write_str("reset by the peer"),
            Self::IdleTimeout => formatter.write_str("timed out"),
            Self::TransportFailure => formatter.write_str("failed in the transport"),
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

/// What an edit did to the relay map: it added a relay the map did not hold, or removed one it held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayMapEdit {
    Added,
    Removed,
}

/// When a snapshot of the counters was taken: at start, at a rebind before its old endpoint closes, once a rebind's fresh endpoint and gossip are in place, at an interval, or at stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMoment {
    Start,
    Rebind,
    Rebound,
    Interval,
    Stop,
}

/// The counters iroh keeps for the socket and for gossip, each named for its group and field, read at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterSnapshot {
    moment: SnapshotMoment,
    counters: Vec<(&'static str, u64)>,
}

/// Reads each named counter field of a metrics group under its group's name.
macro_rules! counters_of {
    ($group:literal, $metrics:expr, [$($field:ident),* $(,)?]) => {
        [$((concat!($group, ".", stringify!($field)), $metrics.$field.get())),*]
    };
}

impl CounterSnapshot {
    /// A snapshot of no counters, for a process that has no endpoint yet.
    #[must_use]
    pub(crate) const fn without_endpoint(moment: SnapshotMoment) -> Self {
        Self {
            moment,
            counters: Vec::new(),
        }
    }

    /// Every counter iroh keeps for an endpoint's socket and for a gossip instance, read now.
    #[must_use]
    pub(crate) fn read(
        moment: SnapshotMoment,
        socket: &iroh::metrics::SocketMetrics,
        gossip: &iroh_gossip::metrics::Metrics,
    ) -> Self {
        let mut counters = counters_of!(
            "socket",
            socket,
            [
                update_direct_addrs,
                send_ipv4,
                send_ipv6,
                send_relay,
                recv_data_relay,
                recv_data_custom,
                recv_data_ipv4,
                recv_data_ipv6,
                recv_datagrams,
                recv_gro_datagrams,
                relay_home_change,
                relay_conns_success,
                relay_conns_failed,
                relay_conns_closed,
                relay_conns_ratelimited,
                holepunch_attempts,
                paths_direct,
                paths_relay,
                paths_custom,
                num_conns_direct,
                num_conns_opened,
                num_conns_closed,
                transport_ip_paths_added,
                transport_ip_paths_removed,
                transport_relay_paths_added,
                transport_relay_paths_removed,
                transport_custom_paths_added,
                transport_custom_paths_removed,
                actor_tick_main,
                actor_tick_msg,
                actor_tick_re_stun,
                actor_tick_portmap_changed,
                actor_tick_direct_addr_heartbeat,
                actor_link_change,
                actor_tick_other
            ]
        )
        .to_vec();
        counters.extend(counters_of!(
            "gossip",
            gossip,
            [
                msgs_ctrl_sent,
                msgs_ctrl_recv,
                msgs_data_sent,
                msgs_data_recv,
                msgs_data_sent_size,
                msgs_data_recv_size,
                msgs_ctrl_sent_size,
                msgs_ctrl_recv_size,
                neighbor_up,
                neighbor_down,
                actor_tick_main,
                actor_tick_rx,
                actor_tick_endpoint,
                actor_tick_dialer,
                actor_tick_dialer_success,
                actor_tick_dialer_failure,
                actor_tick_in_event_rx,
                actor_tick_timers
            ]
        ));
        Self { moment, counters }
    }

    /// When the counters were read.
    #[must_use]
    pub(crate) const fn moment(&self) -> SnapshotMoment {
        self.moment
    }

    /// Each counter's name under its group, with the count read, socket counters first.
    #[must_use]
    pub(crate) fn counters(&self) -> &[(&'static str, u64)] {
        &self.counters
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::{Duration, Instant};

    use iroh::endpoint::{
        ConnectError, ConnectWithOptsError, ConnectingError, Connection, ConnectionError,
        ReadExactError, WriteError,
    };
    use iroh::{EndpointId, TransportAddr};
    use tokio::time::timeout;

    use super::*;
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use crate::drawn_units::{CATCH_ALL_CASES, DrawnUnit, bytes_beside};
    use crate::each_arm::assert_each_arm;
    use crate::event::ShortId;
    use crate::seeded_draws::Draws;
    use crate::test_waits::found_within;

    const SPEC_DIRECT_ALPN: &[u8] = b"estafette/direct/1";
    const SPEC_GATE_KEY_CONTEXT: &str = "messaging gate v1";
    const SPEC_GATE_TIMEOUT: Duration = Duration::from_millis(3000);
    const SPEC_GATE_TIMEOUT_WITH_SLACK: Duration = Duration::from_millis(4000);
    const KEEP_ALIVES_SENT_WHILE_THE_PEER_LIVES: u32 = 12;
    const PEER_LIFE_WAIT_MARGIN_FACTOR: u32 = 2;
    const PEER_LIFE_WAIT: Duration = QUIC_KEEP_ALIVE_INTERVAL
        .saturating_mul(KEEP_ALIVES_SENT_WHILE_THE_PEER_LIVES * PEER_LIFE_WAIT_MARGIN_FACTOR);

    const SPEC_QUIC_IDLE_TIMEOUT_EARLIEST: Duration = Duration::from_secs(9);
    const SPEC_QUIC_IDLE_TIMEOUT_LATEST: Duration = Duration::from_secs(11);
    const MEMBER_SEED: u8 = 1;
    const SECOND_MEMBER_SEED: u8 = 2;
    const OUTSIDER_SEED: u8 = 3;
    const SPEC_PEER_SEED: u8 = 4;
    const SILENT_PEER_SEED: u8 = 5;
    const STRANGER_SEED: u8 = 6;
    const VANISHING_PEER_SEED: u8 = 7;
    const SPEC_PEER_NONCE: [u8; KEY_LEN] = *b"0123456789abcdefghijklmnopqrstuv";
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
    const RELAY_MAP_EDITED_EVENT: &str = "transport.relay_map_edited";
    const LOOPBACK_ANY_PORT: &str = "127.0.0.1:0";
    const LOCAL_DIAL_WAIT: Duration = Duration::from_millis(2000);
    const QUIC_IDLE_TIMEOUT: Duration =
        Duration::from_millis(QUIC_IDLE_TIMEOUT_MILLIS.into_inner());
    const UNANSWERED_DIAL_IDLE_TIMEOUTS: u32 = 2;
    /// Past the QUIC idle timeout, which ends a dial whose handshake no packet answers.
    const UNANSWERED_DIAL_ENDS_WITHIN: Duration =
        QUIC_IDLE_TIMEOUT.saturating_mul(UNANSWERED_DIAL_IDLE_TIMEOUTS);
    const PEER_STOP_CODE: VarInt = VarInt::from_u32(59);

    /// The nonce a node's source fills at its `draw`-th gate challenge: no two bytes alike, and no two draws alike.
    fn drawn_nonce(draw: u8) -> GateNonce {
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
        bind_node_holding(
            seed,
            secret,
            relays,
            report_gate_closing,
            crate::net::direct::tests::unrecorded_register(),
        )
        .await
    }

    async fn bind_node_holding(
        seed: u8,
        secret: [u8; KEY_LEN],
        relays: Vec<RelayUrl>,
        report_gate_closing: impl Fn(GateFailure) + Send + Sync + 'static,
        connections: HeldConnections,
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
            connections,
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
            assert!(member.endpoint.iroh.remove_relay(relay).await.is_some());
        }
        assert!(
            member
                .endpoint
                .iroh
                .remove_relay(&relay_url(UNKNOWN_HOST_RELAY))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn spec_3_6_a_gossip_spawned_on_the_bound_endpoint_runs_on_the_nodes_own_id() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;

        let gossip = member.endpoint().spawn_gossip();
        let router = member
            .endpoint()
            .router_builder()
            .accept(GOSSIP_ALPN, gossip)
            .spawn();

        assert_eq!(router.endpoint().id(), member.endpoint().id());
        router.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn spec_3_6_gate_admits_both_dial_directions_between_holders_of_the_secret() {
        let first = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let second = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;

        for (dialer, acceptor) in [(&first, &second), (&second, &first)] {
            let (dialed, accepted) =
                dial(&dialer.endpoint.iroh, &acceptor.endpoint.iroh, GOSSIP_ALPN).await;
            let (dialed, accepted) = (dialed.unwrap(), accepted.unwrap());
            assert_eq!(dialed.remote_id(), acceptor.endpoint.iroh.id());
            assert_eq!(accepted.remote_id(), dialer.endpoint.iroh.id());

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
                dial(&dialer.endpoint.iroh, &acceptor.endpoint.iroh, GOSSIP_ALPN).await;
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
        let (member_id, peer_id) = (member.endpoint.iroh.id(), peer.id());

        for draw in 0..CONNECTIONS_EACH_DRAWING_A_NONCE {
            let speak_the_spec_gate = async {
                let connection = peer
                    .connect(member.endpoint.iroh.addr(), GOSSIP_ALPN)
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
                tokio::join!(speak_the_spec_gate, accept_next(&member.endpoint.iroh));

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
            member.endpoint.iroh.connect(silent.addr(), GOSSIP_ALPN),
            accept_next(&silent)
        );
        assert!(is_the_gate_timeout(started.elapsed()));
        assert!(dial_is_locally_rejected(&dialed));

        let started = Instant::now();
        let (closed_by_member, accepted) = tokio::join!(
            async {
                let connection = silent
                    .connect(member.endpoint.iroh.addr(), GOSSIP_ALPN)
                    .await
                    .unwrap();
                connection.closed().await
            },
            accept_next(&member.endpoint.iroh)
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
        let (dialed, accepted) = dial(&stranger, &member.endpoint.iroh, DIRECT_ALPN).await;
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
        let member_addr = member.endpoint.iroh.addr();
        let (vanish, vanish_order) = tokio::sync::oneshot::channel::<()>();
        let vanishing_peer = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let peer_and_connection = runtime.block_on(async move {
                let peer = bind_plain_endpoint(VANISHING_PEER_SEED).await;
                let connection = peer.connect(member_addr, DIRECT_ALPN).await.unwrap();
                let _ordered_or_dropped = vanish_order.await;
                (peer, connection)
            });
            runtime.shutdown_background();
            peer_and_connection
        });

        let accepted = accept_next(&member.endpoint.iroh).await.unwrap();
        let keep_alives_before = accepted.stats().frame_tx.ping;
        let awaited =
            format!("{KEEP_ALIVES_SENT_WHILE_THE_PEER_LIVES} keep-alives sent to the live peer");
        found_within(PEER_LIFE_WAIT, &awaited, async || {
            (accepted.stats().frame_tx.ping
                >= keep_alives_before + u64::from(KEEP_ALIVES_SENT_WHILE_THE_PEER_LIVES))
            .then_some(())
        })
        .await
        .unwrap_or_else(|missed| missed.fail_the_test());
        vanish.send(()).unwrap();
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

    #[must_use]
    async fn dial_by_card_id(
        dialer: &NodeEndpoint,
        acceptor: &NodeEndpoint,
    ) -> Option<(Connection, Connection)> {
        let (dial_outcome, accept_outcome) = tokio::join!(
            timeout(
                LOCAL_DIAL_WAIT,
                dialer
                    .endpoint
                    .iroh
                    .connect(acceptor.endpoint.iroh.id(), DIRECT_ALPN)
            ),
            timeout(LOCAL_DIAL_WAIT, accept_next(&acceptor.endpoint.iroh))
        );
        Some((dial_outcome.ok()?.ok()?, accept_outcome.ok()?.ok()?))
    }

    #[tokio::test]
    async fn spec_3_8_dial_by_card_id_reaches_the_address_the_node_fed() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;

        member.replace_address(peer.endpoint.iroh.addr());

        let (dialed, accepted) = dial_by_card_id(&member, &peer).await.expect(
            "the member dialed the peer by its card id and the peer accepted within LOCAL_DIAL_WAIT",
        );
        assert_eq!(dialed.remote_id(), peer.endpoint.iroh.id());
        assert_eq!(accepted.remote_id(), member.endpoint.iroh.id());
    }

    #[tokio::test]
    async fn spec_3_6_no_discovery_service_finds_a_card_the_node_never_fed() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let unfed = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;

        let dialed = member
            .endpoint
            .iroh
            .connect(unfed.endpoint.iroh.id(), DIRECT_ALPN)
            .await;

        assert!(matches!(
            dialed,
            Err(ConnectError::Connect {
                source: ConnectWithOptsError::NoAddress { .. },
                ..
            })
        ));
        assert_eq!(member.endpoint.iroh.address_lookup().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn spec_3_8_replacing_a_card_address_drops_the_old_one() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;
        let black_hole = std::net::UdpSocket::bind(LOOPBACK_ANY_PORT).unwrap();
        let unanswered_address = EndpointAddr::from_parts(
            peer.endpoint.iroh.id(),
            [TransportAddr::Ip(black_hole.local_addr().unwrap())],
        );

        member.replace_address(peer.endpoint.iroh.addr());
        member.replace_address(unanswered_address);
        let accepting = tokio::spawn({
            let peer = peer.endpoint.iroh.clone();
            async move { accept_next(&peer).await }
        });
        let dialed_through_the_unanswered_address = timeout(
            UNANSWERED_DIAL_ENDS_WITHIN,
            member
                .endpoint
                .iroh
                .connect(peer.endpoint.iroh.id(), DIRECT_ALPN),
        )
        .await
        .unwrap_or_else(|_elapsed| {
            panic!("the dial by card id did not end within {UNANSWERED_DIAL_ENDS_WITHIN:?}")
        });
        accepting.abort();
        assert!(
            dialed_through_the_unanswered_address.is_err(),
            "a dial by card id reached the peer though its only address is unanswered: {dialed_through_the_unanswered_address:?}"
        );

        member.replace_address(peer.endpoint.iroh.addr());
        assert!(dial_by_card_id(&member, &peer).await.is_some());
    }

    #[tokio::test]
    async fn spec_3_8_relay_loss_removes_a_relay_and_a_newer_record_adds_it_back() {
        let first_relay = relay_url(FIRST_HOST_RELAY);
        let second_relay = relay_url(SECOND_HOST_RELAY);
        let member =
            bind_node_with_relays(MEMBER_SEED, MEMBER_SECRET, vec![first_relay.clone()]).await;

        member.remove_relay(&first_relay, None).await;
        assert!(
            member
                .endpoint
                .iroh
                .remove_relay(&first_relay)
                .await
                .is_none()
        );

        member
            .add_relay(first_relay.clone(), ShortId::from(tested_peer()))
            .await;
        assert!(
            member
                .endpoint
                .iroh
                .remove_relay(&first_relay)
                .await
                .is_some()
        );

        member
            .add_relay(second_relay.clone(), ShortId::from(tested_peer()))
            .await;
        assert!(
            member
                .endpoint
                .iroh
                .remove_relay(&second_relay)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn spec_3_8_removing_a_relay_the_map_does_not_hold_records_no_relay_map_edit_and_a_held_one_records_one()
     {
        let held_relay = relay_url(FIRST_HOST_RELAY);
        let (record, mut recorded) = crate::test_waits::recorded_events();
        let connections = HeldConnections::new(
            Arc::new(crate::clock::RuntimeClock),
            TransportRecord::new(record, crate::net::direct::tests::test_salt()),
        );
        let member = bind_node_holding(
            MEMBER_SEED,
            MEMBER_SECRET,
            vec![held_relay.clone()],
            |_ignored_reason: GateFailure| {},
            connections,
        )
        .await;
        crate::net::direct::tests::recorded_so_far(&mut recorded);

        member
            .remove_relay(&relay_url(UNKNOWN_HOST_RELAY), None)
            .await;
        let after_the_absent_relay = crate::net::direct::tests::recorded_so_far(&mut recorded);
        member.remove_relay(&held_relay, None).await;
        let after_the_held_relay = crate::net::direct::tests::recorded_so_far(&mut recorded);

        let edits = |events: &[crate::test_waits::RecordedEvent]| {
            events
                .iter()
                .filter(|(name, _fields)| *name == RELAY_MAP_EDITED_EVENT)
                .count()
        };
        assert_eq!(
            edits(&after_the_absent_relay),
            0,
            "{after_the_absent_relay:?}"
        );
        assert_eq!(edits(&after_the_held_relay), 1, "{after_the_held_relay:?}");
    }

    #[tokio::test]
    async fn spec_3_8_adding_a_relay_the_map_holds_leaves_its_entry_untouched() {
        let first_relay = relay_url(FIRST_HOST_RELAY);
        let member =
            bind_node_with_relays(MEMBER_SEED, MEMBER_SECRET, vec![first_relay.clone()]).await;
        let entry_without_address_discovery = Arc::new(RelayConfig::new(first_relay.clone(), None));
        member
            .endpoint
            .iroh
            .insert_relay(first_relay.clone(), entry_without_address_discovery)
            .await;

        member
            .add_relay(first_relay.clone(), ShortId::from(tested_peer()))
            .await;

        let entry = member
            .endpoint
            .iroh
            .remove_relay(&first_relay)
            .await
            .unwrap();
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
        member.replace_address(peer.endpoint.iroh.addr());
        member
            .add_relay(added_relay.clone(), ShortId::from(tested_peer()))
            .await;
        let before = member.endpoint.iroh.clone();

        member.rebind().await.unwrap();

        assert!(before.is_closed());
        assert!(!member.endpoint.iroh.is_closed());
        assert_eq!(member.endpoint.iroh.id(), before.id());
        assert!(
            member
                .endpoint
                .iroh
                .remove_relay(&added_relay)
                .await
                .is_some()
        );
        let (dialed, accepted) = dial_by_card_id(&member, &peer).await.expect(
            "the member dialed the peer by its card id and the peer accepted within LOCAL_DIAL_WAIT",
        );
        assert_eq!(dialed.remote_id(), peer.endpoint.iroh.id());
        assert_eq!(accepted.remote_id(), member.endpoint.iroh.id());
        let (dialed, _held_by_outsider) = tokio::join!(
            member.endpoint.iroh.connect(outsider.addr(), GOSSIP_ALPN),
            accept_next(&outsider)
        );
        assert!(dial_is_locally_rejected(&dialed));
        let closing = the_one_closing(&recorded);
        assert!(matches!(closing.reason, GateFailureReason::Timeout));
        assert_written_as(closing, outsider.id(), "dialed", "timeout");
    }

    #[track_caller]
    fn the_one_closing(recorded: &Mutex<Vec<GateFailure>>) -> GateFailure {
        let mut closings = recorded.lock().unwrap();
        assert_eq!(closings.len(), 1, "{closings:?}");
        closings.pop().unwrap()
    }

    /// Requires the closing to be written with its connection's id, its peer, the side that opened it and the reason's word, in that order.
    #[track_caller]
    fn assert_written_as(closing: GateFailure, peer: EndpointId, direction: &str, reason: &str) {
        let written = crate::event::Event::from(TransportEvent::GateClosed(closing)).fields();
        let (connection, rest) = written.split_first().unwrap();
        assert_eq!(connection.0, "connection");
        assert!(connection.1.is_u64(), "{connection:?}");
        assert_eq!(
            rest,
            [
                ("peer", serde_json::Value::from(ShortId::from(peer).text())),
                ("direction", serde_json::Value::from(direction)),
                ("reason", serde_json::Value::from(reason)),
            ]
        );
    }

    #[tokio::test]
    async fn spec_3_6_gate_reports_a_wrong_answer_as_the_reason_it_closed() {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let outsider = bind_plain_endpoint(OUTSIDER_SEED).await;
        let (member_id, outsider_id) = (member.endpoint.iroh.id(), outsider.id());

        let answer_under_another_secret = async {
            let connection = outsider
                .connect(member.endpoint.iroh.addr(), GOSSIP_ALPN)
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
        let (_closed_connection, accepted) = tokio::join!(
            answer_under_another_secret,
            accept_next(&member.endpoint.iroh)
        );

        assert!(accept_is_locally_rejected(&accepted));
        let closing = the_one_closing(&recorded);
        assert!(matches!(closing.reason, GateFailureReason::WrongProof));
        assert_written_as(closing, outsider_id, "accepted", "wrong_proof");
    }

    #[tokio::test]
    async fn spec_3_6_gate_reports_a_nonce_cut_short_as_the_stream_read_failure_it_closed_on() {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_plain_endpoint(SPEC_PEER_SEED).await;

        let challenge_with_a_nonce_cut_short = async {
            let connection = peer
                .connect(member.endpoint.iroh.addr(), GOSSIP_ALPN)
                .await
                .unwrap();
            let (mut challenge_send, _proof_recv) = connection.open_bi().await.unwrap();
            challenge_send.write_all(NONCE_CUT_SHORT).await.unwrap();
            challenge_send.finish().unwrap();
            connection
        };
        let (_closed_connection, accepted) = tokio::join!(
            challenge_with_a_nonce_cut_short,
            accept_next(&member.endpoint.iroh)
        );

        assert!(accept_is_locally_rejected(&accepted));
        let closing = the_one_closing(&recorded);
        assert!(matches!(
            &closing.reason,
            GateFailureReason::StreamRead(ReadExactError::FinishedEarly(read)) if *read == NONCE_CUT_SHORT.len()
        ));
        assert_written_as(closing, peer.id(), "accepted", "stream_read");
    }

    #[tokio::test]
    async fn spec_3_6_gate_reports_a_proof_the_peer_stopped_reading_as_the_stream_write_failure_it_closed_on()
     {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_plain_endpoint(SPEC_PEER_SEED).await;

        let challenge_then_stop_reading_the_proof = async {
            let connection = peer
                .connect(member.endpoint.iroh.addr(), GOSSIP_ALPN)
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
            accept_next(&member.endpoint.iroh)
        );

        assert!(accept_is_locally_rejected(&accepted));
        let closing = the_one_closing(&recorded);
        assert!(matches!(
            &closing.reason,
            GateFailureReason::StreamWrite(WriteError::Stopped(code)) if *code == PEER_STOP_CODE
        ));
        assert_written_as(closing, peer.id(), "accepted", "stream_write");
    }

    #[tokio::test]
    async fn spec_3_14_gate_reports_the_timeout_as_the_reason_it_closed_on_a_silent_peer() {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let silent = bind_plain_endpoint(SILENT_PEER_SEED).await;

        let (dialed, _held_by_silent) = tokio::join!(
            member.endpoint.iroh.connect(silent.addr(), GOSSIP_ALPN),
            accept_next(&silent)
        );

        assert!(dial_is_locally_rejected(&dialed));
        let closing = the_one_closing(&recorded);
        assert!(matches!(closing.reason, GateFailureReason::Timeout));
        assert_written_as(closing, silent.id(), "dialed", "timeout");
    }

    #[tokio::test]
    async fn spec_3_6_gate_reports_a_peer_that_closes_before_the_challenge_as_the_transport_failure_it_closed_on()
     {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_plain_endpoint_granting_no_streams(SPEC_PEER_SEED).await;

        let close_before_the_challenge = async {
            let connection = peer
                .connect(member.endpoint.iroh.addr(), GOSSIP_ALPN)
                .await
                .unwrap();
            connection.close(PEER_CLOSE_CODE, PEER_CLOSE_REASON);
            connection
        };
        let (_closed_connection, accepted) = tokio::join!(
            close_before_the_challenge,
            accept_next(&member.endpoint.iroh)
        );

        assert!(accept_is_locally_rejected(&accepted));
        let closing = the_one_closing(&recorded);
        assert!(matches!(
            &closing.reason,
            GateFailureReason::Transport(Close::Peer { code, reason: CloseReason::Unlisted { bytes } })
                if *code == u64::from(PEER_CLOSE_CODE) && *bytes == PEER_CLOSE_REASON.len()
        ));
        assert_written_as(closing, peer.id(), "accepted", "transport");
    }

    #[test]
    fn observability_decision_8_a_gate_closing_renders_the_line_of_each_reason_but_the_timeout() {
        const LINE_OPENING: &str = "a gossip connection was closed at the network secret gate: ";
        let cases = [
            (
                GateFailureReason::Transport(Close::PeerReset),
                "the connection failed while opening or accepting the gate stream: ",
            ),
            (
                GateFailureReason::StreamWrite(WriteError::Stopped(PEER_STOP_CODE)),
                "the gate stream failed to write: ",
            ),
            (
                GateFailureReason::StreamFinish(iroh::endpoint::ClosedStream::default()),
                "the gate stream failed to finish: ",
            ),
            (
                GateFailureReason::StreamRead(ReadExactError::FinishedEarly(0)),
                "the gate stream failed to read: ",
            ),
            (
                GateFailureReason::WrongProof,
                "the peer's proof is not the gate proof\n",
            ),
        ];

        for (reason, reason_text) in cases {
            let mut record = Vec::new();
            crate::event::write_run_record(
                &mut record,
                &TransportEvent::GateClosed(tested_gate_failure(reason)).into(),
            )
            .unwrap();

            let line = String::from_utf8(record).unwrap();
            assert!(
                line.starts_with(&format!("{LINE_OPENING}{reason_text}")),
                "{line}"
            );
            assert!(line.ends_with('\n'), "{line}");
        }
    }

    #[test]
    fn spec_3_6_a_gate_closing_appends_exactly_one_line_after_whatever_the_record_holds() {
        for case in 0..CATCH_ALL_CASES {
            let held = bytes_beside(&[b"held".as_slice()], &mut Draws::seeded(case));
            let mut record = held.clone();
            let closing = tested_gate_failure(GateFailureReason::Timeout);

            crate::event::write_run_record(
                &mut record,
                &TransportEvent::GateClosed(closing).into(),
            )
            .unwrap();

            let mut expected = held;
            expected.extend(
                b"a gossip connection was closed at the network secret gate: the gate timeout passed before both proofs\n",
            );
            assert_eq!(record, expected, "case {case}");
        }
    }

    #[tokio::test]
    async fn observability_decision_9_each_connection_the_gate_closes_is_written_under_an_id_of_its_own()
     {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let first_peer = bind_plain_endpoint_granting_no_streams(SPEC_PEER_SEED).await;
        let second_peer = bind_plain_endpoint_granting_no_streams(SILENT_PEER_SEED).await;

        for peer in [&first_peer, &second_peer] {
            let close_before_the_challenge = async {
                let connection = peer
                    .connect(member.endpoint.iroh.addr(), GOSSIP_ALPN)
                    .await
                    .unwrap();
                connection.close(PEER_CLOSE_CODE, PEER_CLOSE_REASON);
                connection
            };
            let (_closed_connection, accepted) = tokio::join!(
                close_before_the_challenge,
                accept_next(&member.endpoint.iroh)
            );
            assert!(accept_is_locally_rejected(&accepted));
        }

        let closings = std::mem::take(&mut *recorded.lock().unwrap());
        let [first, second] = closings.as_slice() else {
            panic!("two closings were recorded: {closings:?}");
        };
        assert_ne!(first.connection, second.connection);
    }

    const HOSTILE_HEX_RUN: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const HOSTILE_CLOSE_REASON: &[u8] = b"\x1b[2J\r\nforged record 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\x07";
    const HOSTILE_PHRASE: &str = "forged record";
    const CONTROL_CHARACTERS: [char; 4] = ['\x1b', '\r', '\n', '\x07'];

    #[tokio::test]
    async fn spec_3_6_a_peer_that_closes_the_gate_with_a_hostile_reason_reaches_neither_standard_error_nor_the_events_line()
     {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_plain_endpoint_granting_no_streams(SPEC_PEER_SEED).await;

        let close_with_a_hostile_reason = async {
            let connection = peer
                .connect(member.endpoint.iroh.addr(), GOSSIP_ALPN)
                .await
                .unwrap();
            connection.close(PEER_CLOSE_CODE, HOSTILE_CLOSE_REASON);
            connection
        };
        let (_closed_connection, accepted) = tokio::join!(
            close_with_a_hostile_reason,
            accept_next(&member.endpoint.iroh)
        );
        assert!(accept_is_locally_rejected(&accepted));

        let closing = recorded.lock().unwrap().pop().unwrap();
        let event: crate::event::Event = TransportEvent::GateClosed(closing).into();
        let standard_error_line = event.run_record_line().unwrap();
        let events_line = format!("{:?}", event.fields());
        for written in [&standard_error_line, &events_line] {
            assert!(!written.contains(HOSTILE_HEX_RUN), "{written}");
            assert!(!written.contains(HOSTILE_PHRASE), "{written}");
        }
        assert!(
            !standard_error_line.contains(CONTROL_CHARACTERS),
            "{standard_error_line:?}"
        );
    }

    const VENDORED_GOSSIP_ALPN: &[u8] = b"/iroh-gossip/1";
    const SPEC_SUPERSEDED_CLOSE_REASON: &[u8] = b"superseded";
    const SPEC_GATE_CLOSE_REASON: &[u8] = b"gate";
    const UNLISTED_CLOSE_REASON: &[u8] = b"a reason estafette never writes";
    const UNLISTED_ALPN: &[u8] = b"h3";
    const SPEC_SUPERSEDED_CLOSE_CODE: u32 = 410;
    const PEER_TRANSPORT_CLOSE_CODE: u32 = 0x0a;
    const CLOSES_APART: usize = 6;
    const LONGEST_DRAWN_CUSTOM_DATA: u64 = 32;

    #[test]
    fn observability_decision_9_each_protocol_is_read_from_the_alpn_it_negotiated() {
        let cases = [
            (SPEC_DIRECT_ALPN, Protocol::Direct),
            (VENDORED_GOSSIP_ALPN, Protocol::Gossip),
            (UNLISTED_ALPN, Protocol::Other),
        ];

        assert_each_arm(&cases, |alpn| Protocol::of(alpn), cases.len());
    }

    #[test]
    fn observability_decision_9_every_alpn_but_the_direct_and_the_gossip_one_reads_as_another_protocol()
     {
        let named = [SPEC_DIRECT_ALPN, VENDORED_GOSSIP_ALPN];
        for case in 0..CATCH_ALL_CASES {
            let alpn = bytes_beside(&named, &mut Draws::seeded(case));
            if named.contains(&alpn.as_slice()) {
                continue;
            }
            assert_eq!(Protocol::of(&alpn), Protocol::Other, "alpn {alpn:?}");
        }
    }

    #[test]
    fn observability_decision_9_every_close_reason_estafette_does_not_write_reads_as_unlisted_by_its_length()
     {
        let named = [SPEC_SUPERSEDED_CLOSE_REASON, SPEC_GATE_CLOSE_REASON];
        for case in 0..CATCH_ALL_CASES {
            let reason = bytes_beside(&named, &mut Draws::seeded(case));
            if named.contains(&reason.as_slice()) {
                continue;
            }
            assert_eq!(
                CloseReason::of(&reason),
                CloseReason::Unlisted {
                    bytes: reason.len()
                },
                "reason {reason:?}"
            );
        }
    }

    #[test]
    fn observability_decision_9_a_close_reason_is_read_as_one_estafette_writes_or_by_its_length_alone()
     {
        let cases = [
            (SPEC_SUPERSEDED_CLOSE_REASON, CloseReason::Superseded),
            (SPEC_GATE_CLOSE_REASON, CloseReason::Gate),
            (
                UNLISTED_CLOSE_REASON,
                CloseReason::Unlisted {
                    bytes: UNLISTED_CLOSE_REASON.len(),
                },
            ),
        ];

        assert_each_arm(&cases, |reason| CloseReason::of(reason), cases.len());
    }

    #[test]
    fn observability_decision_9_each_close_iroh_reports_is_read_as_its_side_and_a_peers_close_with_its_code_and_reason()
     {
        let cases = [
            (ConnectionError::LocallyClosed, Close::ThisSide),
            (
                ConnectionError::ApplicationClosed(iroh::endpoint::ApplicationClose {
                    error_code: VarInt::from_u32(SPEC_SUPERSEDED_CLOSE_CODE),
                    reason: SPEC_SUPERSEDED_CLOSE_REASON.into(),
                }),
                Close::Peer {
                    code: u64::from(SPEC_SUPERSEDED_CLOSE_CODE),
                    reason: CloseReason::Superseded,
                },
            ),
            (
                ConnectionError::ConnectionClosed(iroh::endpoint::ConnectionClose::from(
                    iroh::endpoint::TransportError::new(
                        VarInt::from_u32(PEER_TRANSPORT_CLOSE_CODE).into(),
                        String::from_utf8(SPEC_GATE_CLOSE_REASON.to_vec()).unwrap(),
                    ),
                )),
                Close::Peer {
                    code: u64::from(PEER_TRANSPORT_CLOSE_CODE),
                    reason: CloseReason::Gate,
                },
            ),
            (ConnectionError::Reset, Close::PeerReset),
            (ConnectionError::TimedOut, Close::IdleTimeout),
            (ConnectionError::VersionMismatch, Close::TransportFailure),
            (
                ConnectionError::TransportError(iroh::endpoint::TransportError::new(
                    VarInt::from_u32(PEER_TRANSPORT_CLOSE_CODE).into(),
                    String::new(),
                )),
                Close::TransportFailure,
            ),
            (ConnectionError::CidsExhausted, Close::TransportFailure),
        ];

        assert_each_arm(&cases, Close::of, CLOSES_APART);
    }

    const SPEC_RELAY_PORT: u16 = 443;
    const TESTED_DIRECT_HOST: &str = "192.0.2.7";
    const TESTED_DIRECT_PORT: u16 = 7842;
    const CUSTOM_TRANSPORT_ID: u64 = 7;
    const CUSTOM_TRANSPORT_DATA: &[u8] = b"custom transport data";
    const TESTED_CONNECTION: usize = 7;
    const PEER_KEY_BYTE: u8 = 9;
    const MISSED_PATH_EVENTS: u64 = 5;
    const ELAPSED_MS: u64 = 12;

    fn tested_gate_failure(reason: GateFailureReason) -> GateFailure {
        GateFailure {
            connection: ConnectionId(TESTED_CONNECTION),
            peer: ShortId::from(tested_peer()),
            direction: Direction::Accepted,
            reason,
        }
    }

    fn tested_peer() -> EndpointId {
        SecretKey::from_bytes(&[PEER_KEY_BYTE; crate::crypto::KEY_LEN]).public()
    }

    #[test]
    fn observability_decision_6_each_kind_of_address_is_located_by_its_kind_its_port_and_the_salted_hash_of_its_text()
     {
        let salt = crate::net::direct::tests::test_salt();
        let custom = TransportAddr::Custom((CUSTOM_TRANSPORT_ID, CUSTOM_TRANSPORT_DATA).into());
        let direct_address_text = format!("{TESTED_DIRECT_HOST}:{TESTED_DIRECT_PORT}");
        let cases = [
            (
                TransportAddr::Ip(direct_address_text.parse().unwrap()),
                Location {
                    kind: LocationKind::Direct,
                    port: Some(TESTED_DIRECT_PORT),
                    hash: LocationHash::of(&salt, &direct_address_text),
                },
            ),
            (
                TransportAddr::Relay("https://relay.example/".parse().unwrap()),
                Location {
                    kind: LocationKind::Relay,
                    port: Some(SPEC_RELAY_PORT),
                    hash: LocationHash::of(&salt, "https://relay.example/"),
                },
            ),
            (
                custom.clone(),
                Location {
                    kind: LocationKind::Other,
                    port: None,
                    hash: LocationHash::of(&salt, &format!("{custom:?}")),
                },
            ),
        ];

        assert_each_arm(&cases, |address| Location::of(address, &salt), cases.len());
    }

    /// A custom transport address whose id has a drawn number of its high bits cleared and whose data is drawn.
    fn drawn_custom_address(draws: &mut Draws) -> TransportAddr {
        let cleared_high_bits = u32::try_from(draws.below(u64::from(u64::BITS))).unwrap();
        let transport = draws.below(u64::MAX.checked_shr(cleared_high_bits).unwrap());
        let data: Vec<u8> = (0..draws.below(LONGEST_DRAWN_CUSTOM_DATA))
            .map(|_| u8::drawn(draws))
            .collect();
        TransportAddr::Custom((transport, data.as_slice()).into())
    }

    #[test]
    fn observability_decision_6_every_address_neither_ip_nor_relay_is_located_as_another_kind_with_no_port_by_the_salted_hash_of_its_text()
     {
        let salt = crate::net::direct::tests::test_salt();
        for case in 0..CATCH_ALL_CASES {
            let address = drawn_custom_address(&mut Draws::seeded(case));
            assert_eq!(
                Location::of(&address, &salt),
                Location {
                    kind: LocationKind::Other,
                    port: None,
                    hash: LocationHash::of(&salt, &format!("{address:?}")),
                },
                "address {address:?}"
            );
        }
    }

    #[test]
    fn observability_decision_6_two_addresses_of_another_kind_are_located_by_different_hashes_and_one_address_by_the_same_hash_each_time()
     {
        let salt = crate::net::direct::tests::test_salt();
        for case in 0..CATCH_ALL_CASES {
            let mut draws = Draws::seeded(case);
            let first = drawn_custom_address(&mut draws);
            let second = drawn_custom_address(&mut draws);
            assert_eq!(
                Location::of(&first, &salt).hash,
                Location::of(&first, &salt).hash,
                "address {first:?}"
            );
            if first != second {
                assert_ne!(
                    Location::of(&first, &salt).hash,
                    Location::of(&second, &salt).hash,
                    "addresses {first:?} and {second:?}"
                );
            }
        }
    }

    /// The error iroh returns when an endpoint's required bind meets a socket already taken.
    async fn bind_failure_on_a_taken_socket() -> BindError {
        let taken_socket = std::net::UdpSocket::bind(LOOPBACK_ANY_PORT).unwrap();
        Endpoint::builder(presets::Minimal)
            .bind_addr(taken_socket.local_addr().unwrap())
            .unwrap()
            .bind()
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn observability_decision_11_each_transport_event_is_written_under_its_own_name() {
        let salt = crate::net::direct::tests::test_salt();
        let location = Location::of(
            &TransportAddr::Relay("https://relay.example/".parse().unwrap()),
            &salt,
        );
        let path_change = |change| TransportEvent::PathChanged {
            connection: ConnectionId(TESTED_CONNECTION),
            change,
            path: iroh::endpoint::PathId::ZERO,
            location,
            round_trip_ms: None,
        };
        let cancelled = tokio::spawn(std::future::pending::<()>());
        cancelled.abort();
        let join_error = cancelled.await.unwrap_err();
        let bind_failure = bind_failure_on_a_taken_socket().await;
        let cases: Vec<(crate::event::Event, &str)> = vec![
            (
                TransportEvent::Rebinding(RebindCause::Deaf).into(),
                "transport.rebinding",
            ),
            (
                TransportEvent::ConnectionOpened {
                    connection: ConnectionId(TESTED_CONNECTION),
                    peer: ShortId::from(tested_peer()),
                    direction: Direction::Dialed,
                    protocol: Protocol::Direct,
                }
                .into(),
                "transport.connection_opened",
            ),
            (
                TransportEvent::ConnectionClosing {
                    connection: ConnectionId(TESTED_CONNECTION),
                    peer: ShortId::from(tested_peer()),
                    code: SPEC_SUPERSEDED_CLOSE_CODE,
                    reason: CloseReason::Superseded,
                }
                .into(),
                "transport.connection_closing",
            ),
            (
                TransportEvent::ConnectionClosed {
                    connection: ConnectionId(TESTED_CONNECTION),
                    close: Close::ThisSide,
                }
                .into(),
                "transport.connection_closed",
            ),
            (
                path_change(PathChange::Opened).into(),
                "transport.path_opened",
            ),
            (
                path_change(PathChange::Closed).into(),
                "transport.path_closed",
            ),
            (
                path_change(PathChange::Selected).into(),
                "transport.path_selected",
            ),
            (
                TransportEvent::PathEventsMissed {
                    connection: ConnectionId(TESTED_CONNECTION),
                    missed: MISSED_PATH_EVENTS,
                }
                .into(),
                "transport.path_events_missed",
            ),
            (
                TransportEvent::HomeRelay {
                    location,
                    connected: true,
                }
                .into(),
                "transport.home_relay",
            ),
            (
                TransportEvent::RelayProbed {
                    location,
                    answered: false,
                }
                .into(),
                "transport.relay_probed",
            ),
            (
                TransportEvent::RouterShutdownFailed(join_error).into(),
                "transport.router_shutdown_failed",
            ),
            (
                TransportEvent::RebindFailed(bind_failure).into(),
                "transport.rebind_failed",
            ),
            (
                TransportEvent::GateClosed(tested_gate_failure(GateFailureReason::WrongProof))
                    .into(),
                "transport.gate_closed",
            ),
        ];

        assert_each_arm(&cases, |event| event.name(), cases.len());
    }

    #[test]
    fn observability_decision_11_each_direct_event_is_written_under_its_own_name() {
        let cases: Vec<(crate::event::Event, &str)> = vec![
            (
                crate::event::DirectEvent::ContactFailed {
                    peer: tested_peer(),
                    reason: crate::net::direct::DirectError::Timeout,
                }
                .into(),
                "direct.contact_failed",
            ),
            (
                crate::event::DirectEvent::RequestSent(
                    crate::net::direct::SentRequest::OverAConnection {
                        request: crate::net::direct::RequestSummary::Probe,
                        peer: ShortId::from(tested_peer()),
                        connection: ConnectionId(TESTED_CONNECTION),
                        source: crate::net::direct::ConnectionSource::Dialed,
                        outcome: crate::net::direct::SentOutcome::Acknowledged,
                        elapsed_ms: ELAPSED_MS,
                    },
                )
                .into(),
                "direct.request_sent",
            ),
            (
                crate::event::DirectEvent::RequestServed(
                    crate::net::direct::ServedRequest::Opened {
                        request: crate::net::direct::RequestSummary::Probe,
                        peer: ShortId::from(tested_peer()),
                        connection: ConnectionId(TESTED_CONNECTION),
                        outcome: crate::net::direct::ServedOutcome::Answered,
                        elapsed_ms: ELAPSED_MS,
                    },
                )
                .into(),
                "direct.request_served",
            ),
        ];

        assert_each_arm(&cases, |event| event.name(), cases.len());
    }
}
