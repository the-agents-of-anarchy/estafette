use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use estafette::net::endpoint::GateFailure;

/// The closings reported so far and the signal that the count rose.
#[derive(Default)]
struct ReportedClosings {
    count: Mutex<usize>,
    rose: Condvar,
}

/// The count of connections an in-process node's gate has reported closing, awaited on the node's own sink.
#[derive(Default)]
pub struct GateClosings {
    reported: Arc<ReportedClosings>,
}

impl GateClosings {
    /// The sink that adds one closing to the count for each closing the node's gate reports to it.
    pub fn sink(&self) -> impl Fn(GateFailure) + Send + Sync + 'static {
        let reported = Arc::clone(&self.reported);
        move |_closing: GateFailure| {
            {
                let mut count = reported.count.lock().unwrap();
                *count = count.checked_add(1).unwrap();
            }
            reported.rose.notify_all();
        }
    }

    /// The count of closings once it rises above `already`, failing naming the closing awaited when the bound runs out first.
    #[track_caller]
    pub fn wait_beyond(&self, already: usize, within: Duration) -> usize {
        self.wait_beyond_after(already, within, Instant::now, || {})
    }

    /// As `wait_beyond`, reading the time from `clock` and running `once_waiting` while the wait holds the count, so a closing it lets a reporter make is counted only once the wait runs.
    #[track_caller]
    pub fn wait_beyond_after(
        &self,
        already: usize,
        within: Duration,
        mut clock: impl FnMut() -> Instant,
        once_waiting: impl FnOnce(),
    ) -> usize {
        let deadline = clock().checked_add(within).unwrap();
        let mut count = self.reported.count.lock().unwrap();
        once_waiting();
        while *count <= already {
            let remaining = deadline.saturating_duration_since(clock());
            count = self.reported.rose.wait_timeout(count, remaining).unwrap().0;
            if clock() >= deadline {
                let counted = *count;
                drop(count);
                panic!(
                    "no gate closing beyond {already} within {within:?}; {counted} reported so far"
                );
            }
        }
        *count
    }
}
