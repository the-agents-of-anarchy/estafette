use serde_json::json;

use crate::member_events::COUNTERS_EVENT;
use crate::trace_reader::TraceLine;

/// The field of a counters snapshot naming its moment, and the moment a node records once it runs on a bound endpoint (spec/observability.md decision 9).
const MOMENT_FIELD: &str = "moment";
pub const NODE_START_MOMENT: &str = "start";

/// Whether the events line is the counters snapshot a node records at its start.
pub fn is_node_start_snapshot(line: &TraceLine) -> bool {
    line.event == COUNTERS_EVENT && line.value(MOMENT_FIELD) == Some(&json!(NODE_START_MOMENT))
}
