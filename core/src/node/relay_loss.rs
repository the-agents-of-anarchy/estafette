//! The relay-loss wait of spec 3.8: a home relay leaves the relay map only once it has stayed disconnected for the wait.

use std::collections::BTreeMap;
use std::time::Duration;

use iroh::RelayUrl;
use iroh::endpoint::RelayStatus;
use tokio::time::Instant;

const RELAY_LOSS_WAIT: Duration = Duration::from_millis(500);

/// One home relay and whether the endpoint is connected to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HomeRelay {
    url: RelayUrl,
    connected: bool,
}

/// Each home relay iroh reports, with whether the endpoint is connected to it.
pub(super) fn home_relays(statuses: &[RelayStatus]) -> Vec<HomeRelay> {
    statuses
        .iter()
        .map(|status| HomeRelay {
            url: status.url().clone(),
            connected: status.is_connected(),
        })
        .collect()
}

/// When each lost home relay is due for removal, the relay-loss wait after its loss.
#[derive(Default)]
pub(super) struct RelayLossWatch {
    due: BTreeMap<RelayUrl, Instant>,
}

impl RelayLossWatch {
    /// Starts the wait for each newly lost home relay and forgets each relay that is connected again or no longer home.
    pub(super) fn observe(&mut self, home: &[HomeRelay], now: Instant) {
        self.due.retain(|url, _| {
            home.iter()
                .any(|relay| relay.url == *url && !relay.connected)
        });
        for relay in home.iter().filter(|relay| !relay.connected) {
            self.due
                .entry(relay.url.clone())
                .or_insert_with(|| now.checked_add(RELAY_LOSS_WAIT).unwrap_or(now));
        }
    }

    /// The earliest moment a lost home relay is due for removal.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.due.values().min().copied()
    }

    /// The home relays still disconnected whose wait is over at `now`, forgotten once returned.
    pub(super) fn lost_relays(&mut self, home: &[HomeRelay], now: Instant) -> Vec<RelayUrl> {
        self.observe(home, now);
        let lost: Vec<RelayUrl> = self
            .due
            .iter()
            .filter(|(_, due)| **due <= now)
            .map(|(url, _)| url.clone())
            .collect();
        for url in &lost {
            self.due.remove(url);
        }
        lost
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::presence::tests::{
        LARGEST_STEP_MS, PROPERTY_CASES, RELAY_POOL, SPEC_RELAY_LOSS_WAIT, relay_url,
    };
    use crate::seeded_draws::Draws;

    const STEPS_PER_CASE: u64 = 40;

    fn drawn_home_relays(draws: &mut Draws) -> Vec<HomeRelay> {
        RELAY_POOL
            .iter()
            .filter_map(|relay| match draws.below(3) {
                0 => None,
                state => Some(HomeRelay {
                    url: relay_url(relay),
                    connected: state == 1,
                }),
            })
            .collect()
    }

    #[test]
    fn spec_3_8_a_home_relay_is_removed_only_once_disconnected_for_the_relay_loss_wait_and_then_without_fail()
     {
        let origin = Instant::now();
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let mut watch = RelayLossWatch::default();
            let mut lost_since: BTreeMap<RelayUrl, Instant> = BTreeMap::new();
            let mut last_removed: BTreeMap<RelayUrl, Instant> = BTreeMap::new();
            let mut now = origin;
            for step in 0..STEPS_PER_CASE {
                now += Duration::from_millis(draws.below(LARGEST_STEP_MS));
                let home = drawn_home_relays(&mut draws);
                lost_since.retain(|url, _| {
                    home.iter()
                        .any(|relay| relay.url == *url && !relay.connected)
                });
                for relay in home.iter().filter(|relay| !relay.connected) {
                    lost_since.entry(relay.url.clone()).or_insert(now);
                }
                if step % 2 == 0 {
                    watch.observe(&home, now);
                    continue;
                }
                let removed = watch.lost_relays(&home, now);
                for url in &removed {
                    let since = lost_since[url];
                    let waited_from = last_removed
                        .get(url)
                        .map_or(since, |removed_at| since.max(*removed_at));
                    assert!(
                        now >= waited_from + SPEC_RELAY_LOSS_WAIT,
                        "case {case} step {step}"
                    );
                }
                for (url, since) in &lost_since {
                    let never_removed_since = last_removed
                        .get(url)
                        .is_none_or(|removed_at| removed_at < since);
                    if never_removed_since && now >= *since + SPEC_RELAY_LOSS_WAIT {
                        assert!(removed.contains(url), "case {case} step {step}");
                    }
                }
                for url in removed {
                    last_removed.insert(url, now);
                }
            }
        }
    }
}
