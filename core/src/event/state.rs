use std::convert::Infallible;

use super::{AreaEvent, EventFields};

super::event_names! {
    /// The names of the state events.
    StateEventName {}
}

/// What a process records of its socket and gossip counters at intervals; none is recorded yet, so no value of it exists.
pub struct StateEvent(Infallible);

impl AreaEvent for StateEvent {
    type Name = StateEventName;

    fn name(&self) -> StateEventName {
        let never = self.0;
        match never {}
    }

    fn run_record_line(&self) -> Option<String> {
        let never = self.0;
        match never {}
    }

    fn fields(&self) -> EventFields {
        let never = self.0;
        match never {}
    }
}
