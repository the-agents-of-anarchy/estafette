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
        let (count, outcome) = self
            .reported
            .rose
            .wait_timeout_while(self.reported.count.lock().unwrap(), within, |count| {
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
