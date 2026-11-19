//! The bounded waits a test of an in-process node uses in place of a sleep: on the events its run record was handed, and on a condition it can look at.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::event::{Event, EventFields, RunRecord};

/// The longest a test waits for an event when it names no bound of its own.
const RECORDED_EVENT_WITHIN: Duration = Duration::from_secs(10);

/// The name and fields of one event a run record was handed.
pub type RecordedEvent = (&'static str, EventFields);

/// A run record that hands the name and fields of each event to the receiver.
pub fn recorded_events() -> (RunRecord, UnboundedReceiver<RecordedEvent>) {
    let (sender, receiver) = unbounded_channel();
    let record: RunRecord = Arc::new(move |event: Event| {
        drop(sender.send((event.name(), event.fields())));
    });
    (record, receiver)
}

/// Waits for the next recorded event of the name, within the bound, failing with the name and the events seen.
pub async fn next_recorded(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    name: &str,
) -> EventFields {
    next_recorded_meeting(receiver, name, |_fields| true, RECORDED_EVENT_WITHIN).await
}

/// The fields of the next recorded event of the name whose fields meet the condition, consuming every event before it, failing with the name and the events seen when none comes within the bound.
pub async fn next_recorded_meeting(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    name: &str,
    meets: impl Fn(&EventFields) -> bool + Send + Sync,
    within: Duration,
) -> EventFields {
    recorded_until(receiver, name, meets, within).await.1
}

/// Every event recorded before the next one of the name whose fields meet the condition, in order, and that one's fields, failing with the name and the events seen when none comes within the bound.
pub async fn recorded_until(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    name: &str,
    meets: impl Fn(&EventFields) -> bool + Send + Sync,
    within: Duration,
) -> (Vec<RecordedEvent>, EventFields) {
    let mut before = Vec::new();
    let found = tokio::time::timeout(within, async {
        loop {
            let (recorded_name, fields) = receiver.recv().await?;
            if recorded_name == name && meets(&fields) {
                return Some(fields);
            }
            before.push((recorded_name, fields));
        }
    })
    .await
    .unwrap_or_else(|_elapsed| {
        panic!(
            "no {name} meeting the condition within {within:?}; seen {:?}",
            names_of(&before)
        )
    });
    let Some(fields) = found else {
        panic!(
            "the run record closed before any {name} meeting the condition; seen {:?}",
            names_of(&before)
        )
    };
    (before, fields)
}

fn names_of(events: &[RecordedEvent]) -> Vec<&'static str> {
    events.iter().map(|(name, _fields)| *name).collect()
}

/// The answer of the request, failing with the name of what was awaited when it does not come within the bound.
pub async fn answered_within<Answer>(
    within: Duration,
    awaited: &str,
    request: impl Future<Output = Answer>,
) -> Answer {
    tokio::time::timeout(within, request)
        .await
        .unwrap_or_else(|_elapsed| panic!("no {awaited} within {within:?}"))
}

/// What the look finds, looked for again each time the runtime has run its other tasks, or none when the bound runs out first.
#[must_use]
pub async fn found_within<Found>(
    within: Duration,
    mut look: impl AsyncFnMut() -> Option<Found>,
) -> Option<Found> {
    tokio::time::timeout(within, async {
        loop {
            if let Some(found) = look().await {
                return found;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[should_panic(
        expected = "the run record closed before any process.started meeting the condition; seen []"
    )]
    async fn a_closed_run_record_fails_naming_what_was_awaited() {
        let (record, mut receiver) = recorded_events();
        drop(record);

        next_recorded(&mut receiver, "process.started").await;
    }
}
