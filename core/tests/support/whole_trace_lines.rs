use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use super::file_growth::{GrowthWatch, WaitEnded, first_found_as_files_grow};
use super::trace_reader::{
    SPEC_TRACE_FILE_SIZED_AHEAD_BYTES, TraceLine, WritingProcess, parse_trace_lines,
};

/// How long a test waits for a running process's trace file to come to hold what it waits for.
pub const WHOLE_LINE_WAIT: Duration = Duration::from_secs(10);

/// The whole lines of the trace file of a run whose process ended, failing unless all that follows them is at most one unended start of the run's own next line and then zero bytes to the next 64 KiB boundary.
pub fn whole_lines_of_a_run_file(path: &Path) -> Vec<TraceLine> {
    whole_lines_of_a_run_file_read_as(path, WritingProcess::Ended)
}

/// The whole lines of a run's trace file read as the caller states its writing process, failing unless all that follows the lines landed whole is at most one unended start of the run's own next line and then zero bytes to the next 64 KiB boundary.
fn whole_lines_of_a_run_file_read_as(path: &Path, writing: WritingProcess) -> Vec<TraceLine> {
    let name = path.file_name().unwrap().to_str().unwrap();
    let run = name.split('-').nth(1).unwrap();
    let format_version = estafette::trace::FORMAT_VERSION;
    let own_line_start = format!("{{\"v\":{format_version},\"run\":\"{run}\"");
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
    let written = &bytes[..content_end];
    if writing == WritingProcess::Ended {
        assert!(
            !written.contains(&0),
            "{name}: a zero byte before the file's last byte that is not one"
        );
    }
    let unended = writing
        .landed_part(written)
        .rsplit(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    assert!(
        unended.starts_with(own_line_start.as_bytes())
            || own_line_start.as_bytes().starts_with(unended),
        "{name}: the {} bytes after its last whole line are not the start of this run's own line",
        unended.len()
    );
    let lines =
        parse_trace_lines(written, writing).unwrap_or_else(|failure| panic!("{name}: {failure}"));
    for line in &lines {
        assert_eq!(line.run, run, "{name}");
    }
    lines
}

/// Whether the whole lines of a running process's trace file come to satisfy `holds` within `WHOLE_LINE_WAIT`, looking at once and again each time the file may have grown.
pub fn holds_within_the_wait(path: &Path, holds: impl Fn(&[TraceLine]) -> bool) -> bool {
    let mut watch = GrowthWatch::of_path(path).unwrap_or_else(|failure| panic!("{failure}"));
    let held = first_found_as_files_grow(
        &mut watch,
        WHOLE_LINE_WAIT,
        Instant::now,
        || {},
        |_watch| {
            holds(&whole_lines_of_a_run_file_read_as(
                path,
                WritingProcess::Running,
            ))
            .then_some(())
        },
    );
    match held {
        Ok(()) => true,
        Err(WaitEnded::RanOut) => false,
        Err(WaitEnded::WatcherFailed(failure)) => {
            panic!("{}: the file watcher failed: {failure}", path.display())
        }
        Err(WaitEnded::WatcherEnded) => panic!("{}: the file watcher ended", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use crate::fresh_directory::fresh_directory_in;
    use crate::remove_scratch::remove_scratch;

    use super::super::trace_reader::tests::{
        MEMBER_LINE, MEMBER_RUN, member_line_numbered,
        second_line_still_zero_at_its_start_sized_ahead,
    };
    use super::*;

    /// The name of the raw trace file of the run `MEMBER_LINE` belongs to.
    fn run_file_name() -> String {
        format!("20261112T225914Z-{MEMBER_RUN}-estafette.raw.jsonl")
    }

    fn fresh_run_file(label: &str, bytes: &[u8]) -> std::path::PathBuf {
        let directory = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("whole-trace-lines-{}-{label}", std::process::id()),
        )
        .unwrap();
        let run_file = directory.join(run_file_name());
        fs::write(&run_file, bytes).unwrap();
        run_file
    }

    #[test]
    fn observability_decision_12_the_wait_on_a_live_file_looks_again_while_its_last_line_still_shows_zeros()
     {
        let run_file = fresh_run_file(
            "still-zero",
            &second_line_still_zero_at_its_start_sized_ahead(),
        );
        let mut landed = MEMBER_LINE.as_bytes().to_vec();
        landed.extend_from_slice(member_line_numbered(5).as_bytes());
        landed.resize(SPEC_TRACE_FILE_SIZED_AHEAD_BYTES, 0);
        let (looked, first_look) = mpsc::channel::<Vec<u64>>();
        let landing_file = run_file.clone();
        let landing = std::thread::spawn(move || {
            let numbers_at_the_first_look = first_look.recv();
            fs::write(&landing_file, landed).unwrap();
            numbers_at_the_first_look
        });

        let held = holds_within_the_wait(&run_file, |lines| {
            let numbers: Vec<u64> = lines.iter().map(|line| line.n).collect();
            drop(looked.send(numbers.clone()));
            numbers == [4, 5]
        });
        let numbers_at_the_first_look = landing.join().unwrap();
        remove_scratch(run_file.parent().unwrap()).unwrap();

        assert_eq!(numbers_at_the_first_look, Ok(vec![4]));
        assert!(held);
    }

    #[test]
    fn observability_decision_12_the_wait_on_a_live_file_still_refuses_a_whole_line_that_is_no_trace_line()
     {
        let mut bytes = MEMBER_LINE.as_bytes().to_vec();
        bytes.extend_from_slice(b"not a trace line\n");
        bytes.resize(SPEC_TRACE_FILE_SIZED_AHEAD_BYTES, 0);
        let run_file = fresh_run_file("no-trace-line", &bytes);

        let refusal = std::panic::catch_unwind(|| holds_within_the_wait(&run_file, |_lines| true))
            .map_err(|payload| payload.downcast_ref::<String>().cloned());
        remove_scratch(run_file.parent().unwrap()).unwrap();

        let refusal_text = refusal.unwrap_err().unwrap();
        assert!(
            refusal_text.starts_with(&format!(
                "{}: line 2 is not a trace line: ",
                run_file_name()
            )),
            "{refusal_text}"
        );
    }

    #[test]
    fn observability_decision_12_the_wait_on_a_live_file_refuses_an_unended_tail_that_is_no_start_of_the_runs_own_line()
     {
        let mut bytes = MEMBER_LINE.as_bytes().to_vec();
        bytes.extend_from_slice(b"junk that is no start of a line");
        bytes.resize(SPEC_TRACE_FILE_SIZED_AHEAD_BYTES, 0);
        let run_file = fresh_run_file("junk-tail", &bytes);

        let refusal = std::panic::catch_unwind(|| holds_within_the_wait(&run_file, |_lines| true))
            .map_err(|payload| payload.downcast_ref::<String>().cloned());
        remove_scratch(run_file.parent().unwrap()).unwrap();

        let refusal_text = refusal.unwrap_err().unwrap();
        assert!(
            refusal_text.ends_with("are not the start of this run's own line"),
            "{refusal_text}"
        );
    }
}
