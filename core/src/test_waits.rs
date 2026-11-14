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
    let mut seen = Vec::new();
    let found = tokio::time::timeout(within, async {
        loop {
            let (recorded_name, fields) = receiver.recv().await.unwrap();
            if recorded_name == name && meets(&fields) {
                return fields;
            }
            seen.push(recorded_name);
        }
    })
    .await;
    found.unwrap_or_else(|_elapsed| {
        panic!("no {name} meeting the condition within {within:?}; seen {seen:?}")
    })
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
