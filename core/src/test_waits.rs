//! The bounded waits a test of an in-process node uses in place of a sleep.

use std::time::Duration;

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
