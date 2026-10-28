use std::collections::{HashMap, VecDeque};
use std::time::Duration;

const MAX_INJECTIONS_PER_WINDOW: usize = 10;
const INJECTION_WINDOW: Duration = Duration::from_secs(60);
const REPEATED_BODY_QUIET_PERIOD: Duration = Duration::from_secs(30);

pub enum Admission {
    Admitted,
    Dropped,
}

struct ReceivedBody {
    at: Duration,
    body: String,
}

#[derive(Default)]
struct SenderHistory {
    injection_times: VecDeque<Duration>,
    previous_received: Option<ReceivedBody>,
}

#[derive(Default)]
pub struct SenderRateLimit {
    history_by_sender: HashMap<String, SenderHistory>,
    dropped_frames: u64,
}

impl SenderRateLimit {
    /// Decides whether a frame from this sender may be injected at this moment.
    pub fn admit(&mut self, sender: &str, body: &str, now: Duration) -> Admission {
        let history = self.history_by_sender.entry(sender.to_owned()).or_default();
        history
            .injection_times
            .retain(|injected_at| now.saturating_sub(*injected_at) < INJECTION_WINDOW);
        let repeats_previous_body = history.previous_received.as_ref().is_some_and(|previous| {
            previous.body == body && now.saturating_sub(previous.at) < REPEATED_BODY_QUIET_PERIOD
        });
        history.previous_received = Some(ReceivedBody {
            at: now,
            body: body.to_owned(),
        });
        if history.injection_times.len() >= MAX_INJECTIONS_PER_WINDOW || repeats_previous_body {
            self.dropped_frames = self.dropped_frames.saturating_add(1);
            return Admission::Dropped;
        }
        history.injection_times.push_back(now);
        Admission::Admitted
    }

    /// Counts the frames dropped so far.
    #[must_use]
    pub const fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }
}
