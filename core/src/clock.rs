use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, sleep_until};

/// Milliseconds in a second, the one conversion between the clocks' seconds and milliseconds.
pub(crate) const MILLIS_PER_SECOND: u64 = 1000;

/// A monotonic clock, read for the instant now and waited on until it reads a deadline.
pub trait MonotonicTime: Send + Sync {
    /// The instant the clock reads now.
    fn now(&self) -> Instant;

    /// Waits until the clock reads the deadline or a later instant.
    fn reached(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// The runtime's own clock, read with the runtime's `Instant::now` and waited on with the runtime's timer.
pub struct RuntimeClock;

impl MonotonicTime for RuntimeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn reached(&self, deadline: Instant) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(sleep_until(deadline))
    }
}

/// The monotonic clock the composition root hands down.
pub type MonotonicClock = Arc<dyn MonotonicTime>;

/// The instant a span after `at`, or `at` itself past the clock's range.
pub(crate) fn later_by(at: Instant, span: Duration) -> Instant {
    at.checked_add(span).unwrap_or(at)
}

/// What the work ends with, or none once the clock reads the deadline first; work with no deadline is waited for whole.
pub(crate) async fn before_deadline<Ended>(
    clock: &dyn MonotonicTime,
    deadline: Option<Instant>,
    work: impl Future<Output = Ended>,
) -> Option<Ended> {
    let Some(deadline) = deadline else {
        return Some(work.await);
    };
    tokio::select! {
        biased;
        ended = work => Some(ended),
        () = clock.reached(deadline) => None,
    }
}

/// Waits until the clock reads the deadline, or forever when there is none.
pub(crate) async fn deadline_reached(clock: &dyn MonotonicTime, deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => clock.reached(deadline).await,
        None => std::future::pending().await,
    }
}
