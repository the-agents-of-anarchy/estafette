//! The bounded waits a test of an in-process node uses in place of a sleep: on the events its run record was handed, and on a condition it can look at.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::event::{Event, EventFields, RunRecord};

/// The name and fields of one event a run record was handed.
type RecordedEvent = (&'static str, EventFields);

/// Every event a run record was handed, in order, waking each wait at every new one.
#[derive(Clone)]
pub struct RecordedHistory(Arc<watch::Sender<Vec<RecordedEvent>>>);

impl RecordedHistory {
    /// A history that holds no event yet.
    pub fn new() -> Self {
        Self(Arc::new(watch::Sender::new(Vec::new())))
    }

    /// A run record that adds each event it is handed to the history.
    pub fn sink(&self) -> RunRecord {
        let history = Arc::clone(&self.0);
        Arc::new(move |event: Event| {
            history.send_modify(|recorded| recorded.push((event.name(), event.fields())));
        })
    }

    /// The fields of the event of the name whose fields meet the condition, once that many such events are recorded within the bound, failing with the event's name and every event recorded so far.
    pub async fn recorded_within(
        &self,
        name: &str,
        meets: impl Fn(&EventFields) -> bool + Send + Sync,
        times: usize,
        within: Duration,
    ) -> EventFields {
        let nth_meeting = |recorded: &[RecordedEvent]| {
            recorded
                .iter()
                .filter(|(recorded_name, fields)| *recorded_name == name && meets(fields))
                .nth(times.saturating_sub(1))
                .map(|(_, fields)| fields.clone())
        };
        let mut recorded = self.0.subscribe();
        let found = match tokio::time::timeout(
            within,
            recorded.wait_for(|recorded| nth_meeting(recorded).is_some()),
        )
        .await
        {
            Ok(Ok(recorded)) => nth_meeting(&recorded),
            Ok(Err(_)) | Err(_) => None,
        };
        found.unwrap_or_else(|| {
            let names: Vec<&str> = self.0.borrow().iter().map(|(name, _)| *name).collect();
            panic!("{name} not recorded {times} time(s) within {within:?}; recorded {names:?}")
        })
    }
}

/// What the look finds, looked for again each time the runtime has run its other tasks, or none when the bound runs out first.
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
