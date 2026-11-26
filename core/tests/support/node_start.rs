use serde_json::json;

use crate::member_events::COUNTERS_EVENT;
use crate::trace_reader::TraceLine;

/// The field of a counters snapshot naming its moment, and the moment a node records once it runs on a bound endpoint (spec/observability.md decision 9).
pub const MOMENT_FIELD: &str = "moment";
pub const NODE_START_MOMENT: &str = "start";

/// Whether the events line is the counters snapshot a node records at the moment.
pub fn is_counters_snapshot_of_moment(line: &TraceLine, moment: &str) -> bool {
    line.event == COUNTERS_EVENT && line.value(MOMENT_FIELD) == Some(&json!(moment))
}
