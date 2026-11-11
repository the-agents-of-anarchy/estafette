use iroh::endpoint::BindError;
use tokio::task::JoinError;

use super::{AreaEvent, EventFields};
use crate::net::endpoint::GateFailure;

super::event_names! {
    /// The names of the transport events.
    TransportEventName {
        Rebinding => "transport.rebinding",
        RouterShutdownFailed => "transport.router_shutdown_failed",
        RebindFailed => "transport.rebind_failed",
        GateClosed => "transport.gate_closed",
    }
}

/// What a process records of its endpoint, its rebinds and the connections its gate closes.
pub enum TransportEvent {
    /// The node rebinds its endpoint: no gossip payload was heard for the deaf timeout, or the join gained no neighbor.
    Rebinding,
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
            Self::Rebinding => TransportEventName::Rebinding,
            Self::RouterShutdownFailed(_) => TransportEventName::RouterShutdownFailed,
            Self::RebindFailed(_) => TransportEventName::RebindFailed,
            Self::GateClosed(_) => TransportEventName::GateClosed,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        Some(match self {
            Self::Rebinding => "rebinding the endpoint: no gossip payload was heard for the deaf timeout, or the join gained no neighbor".to_owned(),
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
        Vec::new()
    }
}
