use std::time::Duration;

use crate::member_events::{COUNTERS_EVENT, MemberEvents};
use crate::trace_reader::WallMs;

/// The longest a running node goes without a counters snapshot: one every 60 s, and one at each rebind, which starts the 60 s afresh (spec/observability.md decisions 9 and 13).
const SPEC_COUNTERS_INTERVAL: Duration = Duration::from_secs(60);
/// What a snapshot may lag its interval by on a loaded machine.
const SNAPSHOT_SLACK: Duration = Duration::from_secs(15);
/// The longest a running node takes, once a span of its time has passed, to record the counters snapshot that proves it.
pub const SNAPSHOT_PROVING_A_SPAN_WITHIN: Duration =
    SPEC_COUNTERS_INTERVAL.saturating_add(SNAPSHOT_SLACK);

/// Waits until the member records a counters snapshot at least `span` after the wall-clock instant, the event its own time produces, proof that much of it passed.
#[track_caller]
pub fn wait_for_time_to_pass(events: &MemberEvents, since: WallMs, span: Duration) {
    let due = WallMs(
        since
            .0
            .saturating_add(u64::try_from(span.as_millis()).unwrap()),
    );
    let within = span.saturating_add(SNAPSHOT_PROVING_A_SPAN_WITHIN);
    events.wait_until(
        &format!("{COUNTERS_EVENT} at or after {due} ms"),
        within,
        |lines| {
            lines
                .iter()
                .any(|line| line.event == COUNTERS_EVENT && line.wall_ms >= due)
                .then_some(())
        },
    );
}
