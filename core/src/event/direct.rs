use std::convert::Infallible;

use super::{AreaEvent, EventFields};

super::event_names! {
    /// The names of the direct protocol events.
    DirectEventName {}
}

/// What a process records of the direct requests it sends and serves; none is recorded yet, so no value of it exists.
pub struct DirectEvent(Infallible);

impl AreaEvent for DirectEvent {
    type Name = DirectEventName;

    fn name(&self) -> DirectEventName {
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
