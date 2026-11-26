use crate::event::Event;
use crate::handle::LocalHandle;
use crate::payload::{PayloadDropRule, PayloadFacts};

/// The record of a received payload the node took, or dropped by the rule the admission named, naming its signer by the handle the table holds for it, if any.
pub(super) fn admission_record<Taken>(
    facts: PayloadFacts,
    signer_handle: Option<LocalHandle>,
    admission: &Result<Taken, PayloadDropRule>,
) -> Event {
    match admission {
        Ok(_taken) => {
            crate::event::GossipEvent::PayloadAccepted((facts, signer_handle).into()).into()
        }
        Err(rule) => payload_dropped_record(facts, signer_handle, *rule),
    }
}

/// The record of a payload dropped by the rule, naming its signer by the handle given, if any.
pub(super) fn payload_dropped_record(
    facts: PayloadFacts,
    signer_handle: Option<LocalHandle>,
    rule: PayloadDropRule,
) -> Event {
    crate::event::GossipEvent::PayloadDropped {
        payload: Some((facts, signer_handle).into()),
        rule,
    }
    .into()
}
