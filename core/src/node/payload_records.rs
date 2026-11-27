//! What every node kind records of the gossip it receives and sends, naming a signer by the handle its peer table holds at the clock's reading.

use iroh::SecretKey;

use super::peer_rows::PeerRows;
use crate::event::Event;
use crate::handle::LocalHandle;
use crate::net::gossip::{GossipError, GossipEvent, NetworkTopic};
use crate::payload::{DescribedPayload, PayloadDropRule, PayloadFacts, PayloadRefusal};
use crate::wire::Payload;

/// The record of a received payload the node took, or dropped by the rule the admission named, naming its signer by the handle the table holds for it, if any.
pub(super) fn admission_record(
    facts: PayloadFacts,
    signer_handle: Option<LocalHandle>,
    dropped_by: Option<PayloadDropRule>,
) -> Event {
    match dropped_by {
        None => crate::event::GossipEvent::PayloadAccepted((facts, signer_handle).into()).into(),
        Some(rule) => payload_dropped_record(facts, signer_handle, rule),
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

/// The handle the table of a node that keeps one holds for the signer of the facts at that instant in milliseconds, if any.
fn held_signer_handle_of_node(
    table_at: Option<(&PeerRows, u64)>,
    facts: &PayloadFacts,
) -> Option<LocalHandle> {
    table_at.and_then(|(rows, now_ms)| rows.table().handle_of(facts.signer, now_ms))
}

/// The record of a received payload the node took, or dropped by the rule the admission named, naming its signer by the handle its peer table holds for it at that instant in milliseconds when the node keeps one.
pub(super) fn named_admission_record(
    table_at: Option<(&PeerRows, u64)>,
    facts: PayloadFacts,
    dropped_by: Option<PayloadDropRule>,
) -> Event {
    let signer_handle = held_signer_handle_of_node(table_at, &facts);
    admission_record(facts, signer_handle, dropped_by)
}

/// The record of bytes the payload layer dropped, naming the signer they named by the handle its peer table holds for it at that instant in milliseconds when the node keeps one.
fn refusal_record(table_at: Option<(&PeerRows, u64)>, refusal: PayloadRefusal) -> Event {
    let Some(facts) = refusal.facts else {
        return crate::event::GossipEvent::PayloadDropped {
            payload: None,
            rule: refusal.rule,
        }
        .into();
    };
    let signer_handle = held_signer_handle_of_node(table_at, &facts);
    payload_dropped_record(facts, signer_handle, refusal.rule)
}

/// The record of a verified payload a node takes without a row of spec 3.8 to apply to it, naming its signer by the handle its peer table holds for it at that instant in milliseconds when the node keeps one.
pub fn taken_without_a_row_record(
    facts: PayloadFacts,
    table_at: Option<(&PeerRows, u64)>,
) -> Event {
    named_admission_record(table_at, facts, None)
}

/// What every node records of a topic event before acting on it: the join and each neighbor up and down, a payload being recorded where it is judged.
fn topic_record(event: &GossipEvent) -> Option<Event> {
    match event {
        GossipEvent::Joined => Some(crate::event::GossipEvent::TopicJoined.into()),
        GossipEvent::NeighborUp(node) => {
            Some(crate::event::GossipEvent::NeighborUp((*node).into()).into())
        }
        GossipEvent::NeighborDown(node) => {
            Some(crate::event::GossipEvent::NeighborDown((*node).into()).into())
        }
        GossipEvent::JoinTimedOut | GossipEvent::Payload(_) => None,
    }
}

/// The next event of the topic, recorded as every node records the gossip it receives: each payload the payload layer drops, the join, and each neighbor up and down, a signer named by the handle the node's peer table holds for it at the clock's reading when it keeps one.
pub async fn next_recorded_gossip_event(
    topic: &mut NetworkTopic,
    table_and_clock: Option<(&PeerRows, &(dyn Fn() -> u64 + Send + Sync))>,
    report: &(dyn Fn(Event) + Send + Sync),
) -> Option<Result<GossipEvent, GossipError>> {
    let record_drop = |refusal: PayloadRefusal| {
        let table_at = table_and_clock.map(|(rows, clock)| (rows, clock()));
        report(refusal_record(table_at, refusal));
    };
    let event = topic.next_event_recording_drops(&record_drop).await;
    if let Some(Ok(received)) = &event
        && let Some(record) = topic_record(received)
    {
        report(record);
    }
    event
}

/// Signs and broadcasts the payload on the topic under the signer's key, recording its kind and stamp once sent.
pub async fn broadcast_recorded(
    topic: &NetworkTopic,
    signer: &SecretKey,
    payload: Payload,
    report: &(dyn Fn(Event) + Send + Sync),
) -> Result<(), GossipError> {
    let facts = payload.facts();
    topic.broadcast(signer, payload).await?;
    report(crate::event::GossipEvent::PayloadBroadcast(facts.into()).into());
    Ok(())
}

/// Signs and broadcasts the payload on the topic under the signer's key, recording its kind and stamp once sent and the failure when it is not, for a node whose failed broadcast ends nothing.
pub async fn broadcast_recording_outcome(
    topic: &NetworkTopic,
    signer: &SecretKey,
    payload: Payload,
    report: &(dyn Fn(Event) + Send + Sync),
) {
    if let Err(failure) = broadcast_recorded(topic, signer, payload, report).await {
        report(crate::event::GossipEvent::BroadcastFailed(failure).into());
    }
}
