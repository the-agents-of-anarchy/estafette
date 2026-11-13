use iroh::EndpointId;

use super::{AreaEvent, EventFields};
use crate::net::direct::DirectError;

super::event_names! {
    /// The names of the direct protocol events.
    DirectEventName {
        ContactFailed => "direct.contact_failed",
    }
}

/// What a process records of the direct requests it sends and serves.
pub enum DirectEvent {
    /// A direct request got no valid acknowledgement, naming the node it went to and why.
    ContactFailed {
        peer: EndpointId,
        reason: DirectError,
    },
}

impl AreaEvent for DirectEvent {
    type Name = DirectEventName;

    fn name(&self) -> DirectEventName {
        match self {
            Self::ContactFailed { .. } => DirectEventName::ContactFailed,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        match self {
            Self::ContactFailed { .. } => None,
        }
    }

    fn fields(&self) -> EventFields {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use iroh::SecretKey;

    use super::*;
    use crate::crypto::KEY_LEN;
    use crate::event::{Event, write_run_record};

    const PEER_KEY_BYTE: u8 = 7;

    #[test]
    fn observability_decision_3_a_failed_contact_adds_no_line_to_standard_error() {
        let event: Event = DirectEvent::ContactFailed {
            peer: SecretKey::from_bytes(&[PEER_KEY_BYTE; KEY_LEN]).public(),
            reason: DirectError::Unreachable,
        }
        .into();
        let mut standard_error = Vec::new();

        write_run_record(&mut standard_error, &event).unwrap();

        assert_eq!(String::from_utf8(standard_error).unwrap(), "");
    }
}
