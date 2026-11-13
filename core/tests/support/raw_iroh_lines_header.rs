use super::raw_iroh_lines_trace::{FROZEN_MONOTONIC_MS, NOW_MS};

const RUN_HEX: &str = "a1b2c3d4e5f60718";
const SPEC_RAW_FILE_OPENED: &str = "raw_file_opened";

/// The header of the raw line numbered `n`, through its event name.
pub fn header(n: u64, node: Option<&str>, event: &str) -> String {
    let node = node.map_or_else(String::new, |node| format!(",\"node\":\"{node}\""));
    format!(
        "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":{n},\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{NOW_MS}{node},\"event\":\"{event}\""
    )
}

/// The raw file's first line.
pub fn raw_file_opened_line() -> String {
    format!(
        "{},\"safe_to_post\":false}}\n",
        header(0, None, SPEC_RAW_FILE_OPENED)
    )
}
