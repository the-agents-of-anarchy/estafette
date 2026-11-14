use std::time::Duration;

use crate::member_events::{COUNTERS_EVENT, MemberEvents};

/// The longest a running node goes without a counters snapshot: one every 60 s, and one at each rebind, which starts the 60 s afresh (spec/observability.md decisions 9 and 13).
const SPEC_COUNTERS_INTERVAL: Duration = Duration::from_secs(60);
/// What a snapshot may lag its interval by on a loaded machine.
const SNAPSHOT_SLACK: Duration = Duration::from_secs(15);

/// Waits until the member records a counters snapshot at least `span` after the wall-clock instant, the event its own time produces, proof that much of it passed.
#[track_caller]
pub fn wait_for_time_to_pass(events: &MemberEvents, since_wall_ms: u64, span: Duration) {
    let due_wall_ms = since_wall_ms.saturating_add(u64::try_from(span.as_millis()).unwrap());
    let within = span
        .saturating_add(SPEC_COUNTERS_INTERVAL)
        .saturating_add(SNAPSHOT_SLACK);
    events.wait_until(
        &format!("{COUNTERS_EVENT} at or after {due_wall_ms} ms"),
        within,
        |lines| {
            lines
                .iter()
                .any(|line| line.event == COUNTERS_EVENT && line.wall_ms >= due_wall_ms)
                .then_some(())
        },
    );
}
