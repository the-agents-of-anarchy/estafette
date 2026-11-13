use serde_json::Value;

use super::{AreaEvent, EventFields};
use crate::net::endpoint::{CounterSnapshot, SnapshotMoment};

const MOMENT_FIELD: &str = "moment";

super::event_names! {
    /// The names of the state events.
    StateEventName {
        Counters => "state.counters",
    }
}

/// What a process records of its socket and gossip counters at start, at each rebind, at stop and at intervals between.
pub enum StateEvent {
    /// The counters iroh keeps for the socket and for gossip, read at one moment.
    Counters(CounterSnapshot),
}

impl AreaEvent for StateEvent {
    type Name = StateEventName;

    fn name(&self) -> StateEventName {
        match self {
            Self::Counters(_) => StateEventName::Counters,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        match self {
            Self::Counters(_) => None,
        }
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::Counters(snapshot) => {
                let moment = match snapshot.moment {
                    SnapshotMoment::Start => "start",
                    SnapshotMoment::Rebind => "rebind",
                    SnapshotMoment::Interval => "interval",
                    SnapshotMoment::Stop => "stop",
                };
                std::iter::once((MOMENT_FIELD, Value::from(moment)))
                    .chain(
                        snapshot
                            .counters
                            .iter()
                            .map(|(name, count)| (*name, Value::from(*count))),
                    )
                    .collect()
            }
        }
    }
}
