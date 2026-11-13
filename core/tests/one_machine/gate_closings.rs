use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

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
        self.wait_beyond_after(already, within, || {})
    }

    /// As `wait_beyond`, running `once_waiting` while the wait holds the count, so a closing it lets a reporter make is counted only once the wait runs.
    #[track_caller]
    pub fn wait_beyond_after(
        &self,
        already: usize,
        within: Duration,
        once_waiting: impl FnOnce(),
    ) -> usize {
        let mut once_waiting = Some(once_waiting);
        let (count, outcome) = self
            .reported
            .rose
            .wait_timeout_while(self.reported.count.lock().unwrap(), within, |count| {
                if let Some(release_while_held) = once_waiting.take() {
                    release_while_held();
                }
                *count <= already
            })
            .unwrap();
        let counted = *count;
        drop(count);
        assert!(
            !outcome.timed_out(),
            "no gate closing beyond {already} within {within:?}; {counted} reported so far"
        );
        counted
    }
}
