use iroh::EndpointId;

use super::{AreaEvent, EventFields};
use crate::keys::render_public_key;
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
        Some(match self {
            Self::ContactFailed { peer, reason } => format!(
                "a direct request to {} got no valid acknowledgement: {reason}",
                render_public_key(peer)
            ),
        })
    }

    fn fields(&self) -> EventFields {
        Vec::new()
    }
}
