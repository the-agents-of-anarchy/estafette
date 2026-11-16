//! The bounded waits a test of an in-process node uses in place of a sleep: on the events its run record was handed, and on a condition it can look at; and the clock a test moves where time itself must pass.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::event::{Event, EventFields, RunRecord};
use crate::net::direct::{MonotonicClock, MonotonicTime};

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
            let (recorded_name, fields) = receiver.recv().await.unwrap();
            if recorded_name == name && meets(&fields) {
                return fields;
            }
            before.push((recorded_name, fields));
        }
    })
    .await;
    match found {
        Ok(fields) => (before, fields),
        Err(_elapsed) => {
            let seen: Vec<&str> = before.iter().map(|(seen, _fields)| *seen).collect();
            panic!("no {name} meeting the condition within {within:?}; seen {seen:?}")
        }
    }
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

/// A monotonic clock standing at the instant the test last moved it to, waking each waiter once it reads that waiter's deadline.
#[derive(Clone)]
pub struct StandingClock(Arc<watch::Sender<Instant>>);

impl StandingClock {
    /// A clock standing at the runtime's now.
    pub fn standing_now() -> Self {
        Self(Arc::new(watch::Sender::new(Instant::now())))
    }

    /// Moves the clock forward by the span, waking each waiter whose deadline it then reads.
    pub fn move_forward(&self, by: Duration) {
        self.0
            .send_modify(|now| *now = now.checked_add(by).unwrap());
    }

    /// The clock as a node or a register takes it.
    pub fn monotonic(&self) -> MonotonicClock {
        Arc::new(self.clone())
    }
}

impl MonotonicTime for StandingClock {
    fn now(&self) -> Instant {
        *self.0.borrow()
    }

    fn reached(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let mut readings = self.0.subscribe();
        Box::pin(async move {
            if readings.wait_for(|now| *now >= deadline).await.is_err() {
                std::future::pending::<()>().await;
            }
        })
    }
}
