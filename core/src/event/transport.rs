use iroh::endpoint::{BindError, PathId};
use serde_json::Value;
use tokio::task::JoinError;

use super::{AreaEvent, EventFields};
use crate::net::endpoint::{
    Close, CloseReason, ConnectionId, Direction, GateFailure, Location, LocationKind, PathChange,
    PeerId, Protocol, RebindCause,
};

const CAUSE_FIELD: &str = "cause";
const CONNECTION_FIELD: &str = "connection";
const PEER_FIELD: &str = "peer";
const DIRECTION_FIELD: &str = "direction";
const PROTOCOL_FIELD: &str = "protocol";
const SIDE_FIELD: &str = "side";
const CODE_FIELD: &str = "code";
const REASON_FIELD: &str = "reason";
const REASON_BYTES_FIELD: &str = "reason_bytes";
const PATH_FIELD: &str = "path";
const ROUND_TRIP_FIELD: &str = "rtt_ms";
const MISSED_FIELD: &str = "missed";
const CONNECTED_FIELD: &str = "connected";
const ANSWERED_FIELD: &str = "answered";
const KIND_FIELD: &str = "kind";
const PORT_FIELD: &str = "port";
const LOCATION_FIELD: &str = "location";

super::event_names! {
    /// The names of the transport events.
    TransportEventName {
        Rebinding => "transport.rebinding",
        ConnectionOpened => "transport.connection_opened",
        ConnectionClosing => "transport.connection_closing",
        ConnectionClosed => "transport.connection_closed",
        PathOpened => "transport.path_opened",
        PathClosed => "transport.path_closed",
        PathSelected => "transport.path_selected",
        PathEventsMissed => "transport.path_events_missed",
        HomeRelay => "transport.home_relay",
        RelayProbed => "transport.relay_probed",
        RouterShutdownFailed => "transport.router_shutdown_failed",
        RebindFailed => "transport.rebind_failed",
        GateClosed => "transport.gate_closed",
    }
}

/// What a process records of its endpoint, its rebinds and the connections its gate closes.
pub enum TransportEvent {
    /// The node rebinds its endpoint: no gossip payload was heard for the deaf timeout, or the join gained no neighbor.
    Rebinding(RebindCause),
    /// The gate let a connection through: this side dialed it or accepted it, for one protocol.
    ConnectionOpened {
        connection: ConnectionId,
        peer: PeerId,
        direction: Direction,
        protocol: Protocol,
    },
    /// This process closes a connection it holds, with the code and reason it writes.
    ConnectionClosing {
        connection: ConnectionId,
        peer: PeerId,
        code: u32,
        reason: CloseReason,
    },
    /// A connection closed, as iroh reports the close.
    ConnectionClosed {
        connection: ConnectionId,
        close: Close,
    },
    /// A path of a connection opened, closed or was selected, named by its iroh path id, with where it leads and its round-trip time.
    PathChanged {
        connection: ConnectionId,
        change: PathChange,
        path: PathId,
        location: Location,
        round_trip_ms: Option<u64>,
    },
    /// The path events of a connection arrived faster than they were read, and this many were lost.
    PathEventsMissed {
        connection: ConnectionId,
        missed: u64,
    },
    /// The home relay's connection status changed.
    HomeRelay { location: Location, connected: bool },
    /// A rebind probed a known host's relay, which answered within the relay probe timeout or did not.
    RelayProbed { location: Location, answered: bool },
    /// The old endpoint's listeners did not shut down cleanly before the rebind.
    RouterShutdownFailed(JoinError),
    /// The endpoint was not bound again, so the node announces nothing more.
    RebindFailed(BindError),
    /// A gossip connection was closed at the network secret gate.
    GateClosed(GateFailure),
}

impl AreaEvent for TransportEvent {
    type Name = TransportEventName;

    fn name(&self) -> TransportEventName {
        match self {
            Self::Rebinding(_) => TransportEventName::Rebinding,
            Self::ConnectionOpened { .. } => TransportEventName::ConnectionOpened,
            Self::ConnectionClosing { .. } => TransportEventName::ConnectionClosing,
            Self::ConnectionClosed { .. } => TransportEventName::ConnectionClosed,
            Self::PathChanged { change, .. } => match change {
                PathChange::Opened => TransportEventName::PathOpened,
                PathChange::Closed => TransportEventName::PathClosed,
                PathChange::Selected => TransportEventName::PathSelected,
            },
            Self::PathEventsMissed { .. } => TransportEventName::PathEventsMissed,
            Self::HomeRelay { .. } => TransportEventName::HomeRelay,
            Self::RelayProbed { .. } => TransportEventName::RelayProbed,
            Self::RouterShutdownFailed(_) => TransportEventName::RouterShutdownFailed,
            Self::RebindFailed(_) => TransportEventName::RebindFailed,
            Self::GateClosed(_) => TransportEventName::GateClosed,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        Some(match self {
            Self::ConnectionOpened { .. }
            | Self::ConnectionClosing { .. }
            | Self::ConnectionClosed { .. }
            | Self::PathChanged { .. }
            | Self::PathEventsMissed { .. }
            | Self::HomeRelay { .. }
            | Self::RelayProbed { .. } => return None,
            Self::Rebinding(_) => "rebinding the endpoint: no gossip payload was heard for the deaf timeout, or the join gained no neighbor".to_owned(),
            Self::RouterShutdownFailed(source) => format!(
                "the old endpoint's listeners did not shut down cleanly before the rebind: {source}"
            ),
            Self::RebindFailed(source) => format!(
                "the endpoint was not bound again, so this node announces nothing more: {source}"
            ),
            Self::GateClosed(failure) => {
                format!("a gossip connection was closed at the network secret gate: {failure}")
            }
        })
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::Rebinding(cause) => vec![(CAUSE_FIELD, Value::from(cause_text(*cause)))],
            Self::RouterShutdownFailed(_) | Self::RebindFailed(_) | Self::GateClosed(_) => {
                Vec::new()
            }
            Self::ConnectionOpened {
                connection,
                peer,
                direction,
                protocol,
            } => vec![
                connection_field(*connection),
                peer_field(*peer),
                (DIRECTION_FIELD, Value::from(direction_text(*direction))),
                (PROTOCOL_FIELD, Value::from(protocol_text(*protocol))),
            ],
            Self::ConnectionClosing {
                connection,
                peer,
                code,
                reason,
            } => [
                vec![
                    connection_field(*connection),
                    peer_field(*peer),
                    (CODE_FIELD, Value::from(*code)),
                ],
                reason_fields(*reason),
            ]
            .concat(),
            Self::ConnectionClosed { connection, close } => {
                [vec![connection_field(*connection)], close_fields(*close)].concat()
            }
            Self::PathChanged {
                connection,
                path,
                location,
                round_trip_ms,
                ..
            } => [
                vec![
                    connection_field(*connection),
                    (PATH_FIELD, Value::from(path.to_string())),
                ],
                location_fields(*location),
                round_trip_ms
                    .map(|round_trip| (ROUND_TRIP_FIELD, Value::from(round_trip)))
                    .into_iter()
                    .collect(),
            ]
            .concat(),
            Self::PathEventsMissed { connection, missed } => vec![
                connection_field(*connection),
                (MISSED_FIELD, Value::from(*missed)),
            ],
            Self::HomeRelay {
                location,
                connected,
            } => [
                location_fields(*location),
                vec![(CONNECTED_FIELD, Value::from(*connected))],
            ]
            .concat(),
            Self::RelayProbed { location, answered } => [
                location_fields(*location),
                vec![(ANSWERED_FIELD, Value::from(*answered))],
            ]
            .concat(),
        }
    }
}

/// The cause of a rebind as the events file writes it.
const fn cause_text(cause: RebindCause) -> &'static str {
    match cause {
        RebindCause::Deaf => "deaf",
        RebindCause::JoinGainedNoNeighbor => "join_gained_no_neighbor",
    }
}

/// The connection's identifier as a field.
fn connection_field(connection: ConnectionId) -> (&'static str, Value) {
    (CONNECTION_FIELD, Value::from(connection.0))
}

/// The node's five-byte id as a field.
fn peer_field(peer: PeerId) -> (&'static str, Value) {
    (PEER_FIELD, Value::from(peer.hex()))
}

/// The direction of a connection as the events file writes it.
const fn direction_text(direction: Direction) -> &'static str {
    match direction {
        Direction::Dialed => "dialed",
        Direction::Accepted => "accepted",
    }
}

/// The protocol of a connection as the events file writes it.
const fn protocol_text(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Direct => "direct",
        Protocol::Gossip => "gossip",
        Protocol::Other => "other",
    }
}

/// A close reason as fields: its name when estafette writes it, else its length alone.
fn reason_fields(reason: CloseReason) -> EventFields {
    match reason {
        CloseReason::Superseded => vec![(REASON_FIELD, Value::from("superseded"))],
        CloseReason::Gate => vec![(REASON_FIELD, Value::from("gate"))],
        CloseReason::Unlisted { bytes } => vec![(REASON_BYTES_FIELD, Value::from(bytes))],
    }
}

/// How a connection closed as fields: the side, and the code and reason when the peer closed it.
fn close_fields(close: Close) -> EventFields {
    let side = match close {
        Close::ThisSide => "this_side",
        Close::Peer { .. } => "peer",
        Close::PeerReset => "peer_reset",
        Close::IdleTimeout => "idle_timeout",
        Close::TransportFailure => "transport_failure",
    };
    let mut fields = vec![(SIDE_FIELD, Value::from(side))];
    if let Close::Peer { code, reason } = close {
        fields.push((CODE_FIELD, Value::from(code)));
        fields.extend(reason_fields(reason));
    }
    fields
}

/// A network location as fields: its kind, its port when it has one, and its salted hash.
fn location_fields(location: Location) -> EventFields {
    let kind = match location.kind {
        LocationKind::Direct => "direct",
        LocationKind::Relay => "relay",
        LocationKind::Other => "other",
    };
    let mut fields = vec![(KIND_FIELD, Value::from(kind))];
    fields.extend(location.port.map(|port| (PORT_FIELD, Value::from(port))));
    fields.push((LOCATION_FIELD, Value::from(location.hash.hex())));
    fields
}
