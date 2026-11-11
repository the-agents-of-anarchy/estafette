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

impl SenderHistory {
    fn no_longer_limits_at(&self, now: Duration) -> bool {
        self.injection_times
            .iter()
            .all(|injected_at| now.saturating_sub(*injected_at) >= INJECTION_WINDOW)
            && self.previous_received.as_ref().is_none_or(|previous| {
                now.saturating_sub(previous.at) >= REPEATED_BODY_QUIET_PERIOD
            })
    }
}

#[derive(Default)]
pub struct SenderRateLimit {
    history_by_sender: HashMap<String, SenderHistory>,
    dropped_frames: u64,
}

impl SenderRateLimit {
    /// Decides whether a frame from this sender may be injected at this moment.
    pub fn admit(&mut self, sender: &str, body: &str, now: Duration) -> Admission {
        self.history_by_sender
            .retain(|_, history| !history.no_longer_limits_at(now));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seeded::next_below;

    const GENERATED_CASES: u64 = 300;
    const EVENTS_PER_CASE: usize = 80;
    const SENDERS: [&str; 3] = ["ana", "bo", "cy"];
    const LONGEST_GAP_SECONDS: usize = 4;
    const MINIMUM_SEEDS_REACHING_A_DROP: usize = 50;

    #[derive(Clone)]
    struct Event {
        sender: &'static str,
        at: Duration,
    }

    fn generated_events(seed: u64) -> Vec<Event> {
        let mut state = seed;
        let mut now_seconds = 0_u64;
        (0..EVENTS_PER_CASE)
            .map(|_| {
                let gap = u64::try_from(next_below(&mut state, LONGEST_GAP_SECONDS)).unwrap_or(0);
                now_seconds = now_seconds.saturating_add(gap);
                Event {
                    sender: SENDERS[next_below(&mut state, SENDERS.len())],
                    at: Duration::from_secs(now_seconds),
                }
            })
            .collect()
    }

    fn admissions(limit: &mut SenderRateLimit, events: &[Event]) -> Vec<bool> {
        events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                matches!(
                    limit.admit(event.sender, &format!("body {index}"), event.at),
                    Admission::Admitted
                )
            })
            .collect()
    }

    fn events_of(sender: &str, events: &[Event]) -> Vec<Event> {
        events
            .iter()
            .filter(|event| event.sender == sender)
            .cloned()
            .collect()
    }

    fn admissions_of(sender: &str, events: &[Event], admitted: &[bool]) -> Vec<bool> {
        events
            .iter()
            .zip(admitted)
            .filter(|(event, _)| event.sender == sender)
            .map(|(_, was_admitted)| *was_admitted)
            .collect()
    }

    fn most_admissions_in_one_window(own_events: &[Event], own_admitted: &[bool]) -> usize {
        let admitted_times: Vec<Duration> = own_events
            .iter()
            .zip(own_admitted)
            .filter(|(_, was_admitted)| **was_admitted)
            .map(|(event, _)| event.at)
            .collect();
        admitted_times
            .iter()
            .map(|time| {
                admitted_times
                    .iter()
                    .filter(|other| {
                        **other <= *time && time.saturating_sub(**other) < INJECTION_WINDOW
                    })
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn spec_3_12_no_sender_has_more_than_ten_injections_in_any_60_seconds_and_senders_do_not_share_a_window()
     {
        let mut seeds_that_dropped = 0;
        for seed in 0..GENERATED_CASES {
            let events = generated_events(seed);
            let mut limit = SenderRateLimit::default();
            let admitted = admissions(&mut limit, &events);
            for sender in SENDERS {
                let own_events = events_of(sender, &events);
                let own_admitted = admissions_of(sender, &events, &admitted);
                let own_admitted_alone = admissions(&mut SenderRateLimit::default(), &own_events);
                assert_eq!(own_admitted, own_admitted_alone, "seed {seed} {sender}");
                assert!(
                    most_admissions_in_one_window(&own_events, &own_admitted)
                        <= MAX_INJECTIONS_PER_WINDOW,
                    "seed {seed} {sender}"
                );
            }
            let dropped = admitted
                .iter()
                .filter(|was_admitted| !**was_admitted)
                .count();
            assert_eq!(
                limit.dropped_frames(),
                u64::try_from(dropped).unwrap_or(0),
                "seed {seed}"
            );
            seeds_that_dropped += usize::from(dropped > 0);
        }
        assert!(
            seeds_that_dropped > MINIMUM_SEEDS_REACHING_A_DROP,
            "{seeds_that_dropped}"
        );
    }

    #[test]
    fn spec_3_12_memory_bound_read_from_the_private_map_a_sender_is_forgotten_once_nothing_limits_it()
     {
        const SENDER_COUNT: usize = 20;
        let mut limit = SenderRateLimit::default();
        for index in 0..SENDER_COUNT {
            limit.admit(&format!("sender-{index}"), "hello", Duration::ZERO);
        }
        let just_inside_the_window = INJECTION_WINDOW.saturating_sub(Duration::from_secs(1));
        limit.admit("early", "hello", just_inside_the_window);
        assert_eq!(
            limit.history_by_sender.len(),
            SENDER_COUNT.saturating_add(1)
        );
        limit.admit("late", "hello", INJECTION_WINDOW);
        assert_eq!(limit.history_by_sender.len(), 2);
    }
}
