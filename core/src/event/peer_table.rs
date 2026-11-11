use std::convert::Infallible;

use super::{AreaEvent, EventFields};

super::event_names! {
    /// The names of the peer table events.
    PeerTableEventName {}
}

/// What a node records of its peer table's entries, handles and lookups; none is recorded yet, so no value of it exists.
pub struct PeerTableEvent(Infallible);

impl AreaEvent for PeerTableEvent {
    type Name = PeerTableEventName;

    fn name(&self) -> PeerTableEventName {
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
