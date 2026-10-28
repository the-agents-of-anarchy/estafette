use std::collections::{HashMap, VecDeque};
use std::time::Duration;

const MAX_INJECTIONS_PER_WINDOW: usize = 10;
const INJECTION_WINDOW: Duration = Duration::from_secs(60);
const REPEATED_BODY_QUIET_PERIOD: Duration = Duration::from_secs(30);

pub enum Admission {
    Admitted,
    Dropped,
}

struct Injection {
    at: Duration,
    body: String,
}

#[derive(Default)]
pub struct SenderRateLimit {
    recent_injections_by_sender: HashMap<String, VecDeque<Injection>>,
    dropped_frames: u64,
}

impl SenderRateLimit {
    /// Decides whether a frame from this sender may be injected at this moment.
    pub fn admit(&mut self, sender: &str, body: &str, now: Duration) -> Admission {
        let recent = self
            .recent_injections_by_sender
            .entry(sender.to_owned())
            .or_default();
        recent.retain(|injection| now.saturating_sub(injection.at) < INJECTION_WINDOW);
        let repeats_previous_body = recent.back().is_some_and(|previous| {
            previous.body == body && now.saturating_sub(previous.at) < REPEATED_BODY_QUIET_PERIOD
        });
        if recent.len() >= MAX_INJECTIONS_PER_WINDOW || repeats_previous_body {
            self.dropped_frames = self.dropped_frames.saturating_add(1);
            return Admission::Dropped;
        }
        recent.push_back(Injection {
            at: now,
            body: body.to_owned(),
        });
        Admission::Admitted
    }

    /// Counts the frames dropped so far.
    #[must_use]
    pub const fn dropped_frames(&self) -> u64 {
        self.dropped_frames
    }
}
