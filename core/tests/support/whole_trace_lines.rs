use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use super::trace_reader::{TraceLine, parse_trace_lines};

/// How long a test waits for a running process's trace file to come to hold what it waits for.
pub const WHOLE_LINE_WAIT: Duration = Duration::from_secs(10);
const SPEC_TRACE_FILE_SIZED_AHEAD_BYTES: usize = 65_536;

/// The whole lines of a run's trace file, failing unless all that follows them is at most one unended start of the run's own next line and then zero bytes to the next 64 KiB boundary.
pub fn whole_lines_of_a_run_file(path: &Path) -> Vec<TraceLine> {
    let name = path.file_name().unwrap().to_str().unwrap();
    let run = name.split('-').nth(1).unwrap();
    let own_line_start = format!("{{\"v\":1,\"run\":\"{run}\"");
    let bytes = fs::read(path).unwrap();
    let content_end = bytes
        .iter()
        .rposition(|byte| *byte != 0)
        .map_or(0, |last| last.saturating_add(1));
    assert!(
        bytes
            .len()
            .is_multiple_of(SPEC_TRACE_FILE_SIZED_AHEAD_BYTES)
            && bytes.len().saturating_sub(content_end) <= SPEC_TRACE_FILE_SIZED_AHEAD_BYTES,
        "{name}: {} bytes after {content_end} bytes of content do not end at the next 64 KiB boundary",
        bytes.len().saturating_sub(content_end)
    );
    let content = &bytes[..content_end];
    assert!(
        !content.contains(&0),
        "{name}: a zero byte before the file's last byte that is not one"
    );
    let unended = content
        .rsplit(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    assert!(
        unended.starts_with(own_line_start.as_bytes())
            || own_line_start.as_bytes().starts_with(unended),
        "{name}: the {} bytes after its last whole line are not the start of this run's own line",
        unended.len()
    );
    let lines = parse_trace_lines(content).unwrap_or_else(|failure| panic!("{name}: {failure}"));
    for line in &lines {
        assert_eq!(line.run, run, "{name}");
    }
    lines
}

/// Whether the whole lines of a running process's trace file come to satisfy `holds` within `WHOLE_LINE_WAIT`.
pub fn holds_within_the_wait(path: &Path, holds: impl Fn(&[TraceLine]) -> bool) -> bool {
    let deadline = Instant::now().checked_add(WHOLE_LINE_WAIT).unwrap();
    while !holds(&whole_lines_of_a_run_file(path)) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::yield_now();
    }
    true
}
