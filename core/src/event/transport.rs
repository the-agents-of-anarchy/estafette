use iroh::endpoint::{BindError, PathId};
use serde_json::Value;
use tokio::task::JoinError;

use super::short_id::ShortId;
use super::{AreaEvent, CAUSE_FIELD, CODE_FIELD, EventFields, KIND_FIELD};
use crate::net::endpoint::{
    Close, CloseReason, ConnectionId, Direction, GateFailure, GateFailureReason, Location,
    LocationKind, PathChange, Protocol, RebindCause, RelayMapEdit,
};
use crate::node::{HostUnparsed, SkippedHost};

const EDIT_FIELD: &str = "edit";
const INDEX_FIELD: &str = "index";
const HOST_FIELD: &str = "host";
const CONNECTION_FIELD: &str = "connection";
const DIRECTION_FIELD: &str = "direction";
const PROTOCOL_FIELD: &str = "protocol";
const SIDE_FIELD: &str = "side";
const REASON_FIELD: &str = "reason";
const REASON_BYTES_FIELD: &str = "reason_bytes";
const PATH_FIELD: &str = "path";
const ROUND_TRIP_FIELD: &str = "rtt_ms";
const MISSED_FIELD: &str = "missed";
const CONNECTED_FIELD: &str = "connected";
const ANSWERED_FIELD: &str = "answered";
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
        RelayMapEdited => "transport.relay_map_edited",
        KnownHostSkipped => "transport.known_host_skipped",
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
        peer: ShortId,
        direction: Direction,
        protocol: Protocol,
    },
    /// This process closes a connection it holds, with the code and reason it writes.
    ConnectionClosing {
        connection: ConnectionId,
        peer: ShortId,
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
    /// The relay map this node dials through gained or lost a relay, naming the known host it belongs to when one has it, and where it is.
    RelayMapEdited {
        edit: RelayMapEdit,
        host: Option<ShortId>,
        location: Location,
    },
    /// A rebind left out a configured host because its id or relay does not parse, naming its place in the configuration and the part.
    KnownHostSkipped(SkippedHost),
    /// The old endpoint's listeners did not shut down cleanly before the rebind.
    RouterShutdownFailed(JoinError),
    /// The endpoint was not bound again, so the node announces nothing more.
    RebindFailed(BindError),
    /// A gossip connection was closed at the network secret gate: which connection, its peer, the side that opened it, and why.
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
            Self::RelayMapEdited { .. } => TransportEventName::RelayMapEdited,
            Self::KnownHostSkipped(_) => TransportEventName::KnownHostSkipped,
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
            | Self::RelayProbed { .. }
            | Self::RelayMapEdited { .. }
            | Self::KnownHostSkipped(_) => return None,
            Self::Rebinding(_) => "rebinding the endpoint: no gossip payload was heard for the deaf timeout, or the join gained no neighbor".to_owned(),
            Self::RouterShutdownFailed(source) => format!(
                "the old endpoint's listeners did not shut down cleanly before the rebind: {source}"
            ),
            Self::RebindFailed(source) => format!(
                "the endpoint was not bound again, so this node announces nothing more: {source}"
            ),
            Self::GateClosed(failure) => format!(
                "a gossip connection was closed at the network secret gate: {}",
                failure.reason
            ),
        })
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::Rebinding(cause) => vec![(CAUSE_FIELD, Value::from(cause_text(*cause)))],
            Self::RouterShutdownFailed(_) | Self::RebindFailed(_) => Vec::new(),
            Self::GateClosed(failure) => vec![
                connection_field(failure.connection),
                failure.peer.field(),
                (
                    DIRECTION_FIELD,
                    Value::from(direction_text(failure.direction)),
                ),
                (
                    REASON_FIELD,
                    Value::from(gate_failure_reason_text(&failure.reason)),
                ),
            ],
            Self::ConnectionOpened {
                connection,
                peer,
                direction,
                protocol,
            } => vec![
                connection_field(*connection),
                peer.field(),
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
                    peer.field(),
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
            Self::KnownHostSkipped(skipped) => vec![
                (INDEX_FIELD, Value::from(skipped.index)),
                (
                    REASON_FIELD,
                    Value::from(host_unparsed_text(skipped.reason)),
                ),
            ],
            Self::RelayMapEdited {
                edit,
                host,
                location,
            } => [
                vec![(EDIT_FIELD, Value::from(relay_map_edit_text(*edit)))],
                host.map(|host| (HOST_FIELD, Value::from(host.text())))
                    .into_iter()
                    .collect(),
                location_fields(*location),
            ]
            .concat(),
        }
    }
}

/// The part of a configured host that did not parse, as the events file writes it.
const fn host_unparsed_text(reason: HostUnparsed) -> &'static str {
    match reason {
        HostUnparsed::Id => "id",
        HostUnparsed::Relay => "relay",
    }
}

/// The relay map edit as the events file writes it.
const fn relay_map_edit_text(edit: RelayMapEdit) -> &'static str {
    match edit {
        RelayMapEdit::Added => "added",
        RelayMapEdit::Removed => "removed",
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
pub(super) fn connection_field(connection: ConnectionId) -> (&'static str, Value) {
    (CONNECTION_FIELD, Value::from(connection.value()))
}

/// The direction of a connection as the events file writes it.
const fn direction_text(direction: Direction) -> &'static str {
    match direction {
        Direction::Dialed => "dialed",
        Direction::Accepted => "accepted",
    }
}

/// The reason the gate closed a connection as the events file writes it.
const fn gate_failure_reason_text(reason: &GateFailureReason) -> &'static str {
    match reason {
        GateFailureReason::Transport(_) => "transport",
        GateFailureReason::StreamWrite(_) => "stream_write",
        GateFailureReason::StreamFinish(_) => "stream_finish",
        GateFailureReason::StreamRead(_) => "stream_read",
        GateFailureReason::WrongProof => "wrong_proof",
        GateFailureReason::Timeout => "timeout",
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

#[cfg(test)]
mod tests {
    use iroh::endpoint::{ClosedStream, ReadExactError, VarInt, WriteError};

    use super::*;
    use crate::each_arm::assert_each_arm;
    use crate::net::direct::tests::test_salt;
    use crate::net::endpoint::LocationHash;

    const SPEC_SUPERSEDED_CLOSE_CODE: u64 = 410;
    const UNLISTED_REASON_BYTES: usize = 37;
    const STOP_CODE: u32 = 59;
    const DIRECT_PORT: u16 = 7842;
    const RELAY_PORT: u16 = 443;
    const RELAY_HOST_BYTE: u8 = 9;
    const SKIPPED_HOST_INDEX: usize = 7;

    #[test]
    fn observability_decision_9_each_protocol_a_connection_carries_is_written_as_a_word_of_its_own()
    {
        let cases = [
            (Protocol::Direct, "direct"),
            (Protocol::Gossip, "gossip"),
            (Protocol::Other, "other"),
        ];

        assert_each_arm(&cases, |protocol| protocol_text(*protocol), cases.len());
    }

    #[test]
    fn observability_decision_9_a_close_reason_estafette_writes_is_named_and_any_other_is_written_by_its_length_alone()
     {
        let cases = [
            (
                CloseReason::Superseded,
                vec![("reason", Value::from("superseded"))],
            ),
            (CloseReason::Gate, vec![("reason", Value::from("gate"))]),
            (
                CloseReason::Unlisted {
                    bytes: UNLISTED_REASON_BYTES,
                },
                vec![("reason_bytes", Value::from(UNLISTED_REASON_BYTES))],
            ),
        ];

        assert_each_arm(&cases, |reason| reason_fields(*reason), cases.len());
    }

    #[test]
    fn observability_decision_9_each_way_a_connection_closes_is_written_with_its_side_and_a_peers_close_with_its_code_and_reason()
     {
        let cases = [
            (Close::ThisSide, vec![("side", Value::from("this_side"))]),
            (
                Close::Peer {
                    code: SPEC_SUPERSEDED_CLOSE_CODE,
                    reason: CloseReason::Gate,
                },
                vec![
                    ("side", Value::from("peer")),
                    ("code", Value::from(SPEC_SUPERSEDED_CLOSE_CODE)),
                    ("reason", Value::from("gate")),
                ],
            ),
            (Close::PeerReset, vec![("side", Value::from("peer_reset"))]),
            (
                Close::IdleTimeout,
                vec![("side", Value::from("idle_timeout"))],
            ),
            (
                Close::TransportFailure,
                vec![("side", Value::from("transport_failure"))],
            ),
        ];

        assert_each_arm(&cases, |close| close_fields(*close), cases.len());
    }

    #[test]
    fn observability_decision_9_each_way_the_gate_closes_is_written_as_a_word_of_its_own() {
        let cases = [
            (GateFailureReason::Transport(Close::PeerReset), "transport"),
            (
                GateFailureReason::StreamWrite(WriteError::Stopped(VarInt::from_u32(STOP_CODE))),
                "stream_write",
            ),
            (
                GateFailureReason::StreamFinish(ClosedStream::default()),
                "stream_finish",
            ),
            (
                GateFailureReason::StreamRead(ReadExactError::FinishedEarly(0)),
                "stream_read",
            ),
            (GateFailureReason::WrongProof, "wrong_proof"),
            (GateFailureReason::Timeout, "timeout"),
        ];

        assert_each_arm(
            &cases,
            |reason| gate_failure_reason_text(reason),
            cases.len(),
        );
    }

    #[test]
    fn observability_decision_9_each_part_of_a_configured_host_that_does_not_parse_is_written_as_a_word_of_its_own()
     {
        let cases = [
            (
                SkippedHost {
                    index: 0,
                    reason: HostUnparsed::Id,
                },
                vec![("index", Value::from(0)), ("reason", Value::from("id"))],
            ),
            (
                SkippedHost {
                    index: SKIPPED_HOST_INDEX,
                    reason: HostUnparsed::Relay,
                },
                vec![
                    ("index", Value::from(SKIPPED_HOST_INDEX)),
                    ("reason", Value::from("relay")),
                ],
            ),
        ];

        assert_each_arm(
            &cases,
            |skipped| TransportEvent::KnownHostSkipped(*skipped).fields(),
            cases.len(),
        );
    }

    #[test]
    fn observability_decision_9_each_cause_of_a_rebind_is_written_as_a_word_of_its_own() {
        let cases = [
            (RebindCause::Deaf, "deaf"),
            (RebindCause::JoinGainedNoNeighbor, "join_gained_no_neighbor"),
        ];

        assert_each_arm(&cases, |cause| cause_text(*cause), cases.len());
    }

    #[test]
    fn observability_decision_9_each_relay_map_edit_is_written_as_a_word_with_its_host_when_it_has_one()
     {
        let relay_hash = LocationHash::of(&test_salt(), "https://relay.example/");
        let relay = Location {
            kind: LocationKind::Relay,
            port: Some(RELAY_PORT),
            hash: relay_hash,
        };
        let host = ShortId::from(&crate::wire::PublicKey::from_bytes([RELAY_HOST_BYTE; 32]));
        let relay_fields = vec![
            ("kind", Value::from("relay")),
            ("port", Value::from(RELAY_PORT)),
            ("location", Value::from(relay_hash.hex())),
        ];
        let cases = [
            (
                (RelayMapEdit::Added, Some(host)),
                [
                    vec![
                        ("edit", Value::from("added")),
                        ("host", Value::from(host.text())),
                    ],
                    relay_fields.clone(),
                ]
                .concat(),
            ),
            (
                (RelayMapEdit::Removed, Some(host)),
                [
                    vec![
                        ("edit", Value::from("removed")),
                        ("host", Value::from(host.text())),
                    ],
                    relay_fields.clone(),
                ]
                .concat(),
            ),
            (
                (RelayMapEdit::Removed, None),
                [vec![("edit", Value::from("removed"))], relay_fields].concat(),
            ),
        ];

        assert_each_arm(
            &cases,
            |(edit, host)| {
                TransportEvent::RelayMapEdited {
                    edit: *edit,
                    host: *host,
                    location: relay,
                }
                .fields()
            },
            cases.len(),
        );
    }

    #[test]
    fn observability_decision_6_each_location_is_written_as_its_kind_its_port_when_it_has_one_and_its_salted_hash()
     {
        let direct = LocationHash::of(&test_salt(), "192.0.2.7:7842");
        let relay = LocationHash::of(&test_salt(), "https://relay.example/");
        let other = LocationHash::of(&test_salt(), "a custom transport address");
        let cases = [
            (
                Location {
                    kind: LocationKind::Direct,
                    port: Some(DIRECT_PORT),
                    hash: direct,
                },
                vec![
                    ("kind", Value::from("direct")),
                    ("port", Value::from(DIRECT_PORT)),
                    ("location", Value::from(direct.hex())),
                ],
            ),
            (
                Location {
                    kind: LocationKind::Relay,
                    port: Some(RELAY_PORT),
                    hash: relay,
                },
                vec![
                    ("kind", Value::from("relay")),
                    ("port", Value::from(RELAY_PORT)),
                    ("location", Value::from(relay.hex())),
                ],
            ),
            (
                Location {
                    kind: LocationKind::Other,
                    port: None,
                    hash: other,
                },
                vec![
                    ("kind", Value::from("other")),
                    ("location", Value::from(other.hex())),
                ],
            ),
        ];

        assert_each_arm(&cases, |location| location_fields(*location), cases.len());
    }
}
