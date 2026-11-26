use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use super::file_growth::{GrowthWatch, WaitEnded, first_found_as_files_grow};
use super::trace_reader::{
    NO_EVENT_RECORDED, SPEC_TRACE_FILE_SIZED_AHEAD_BYTES, TraceLine, WritingProcess,
    parse_trace_lines,
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
    let landed = writing.landed_part(written);
    let unended = landed
        .rsplit(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    assert!(
        unended.starts_with(own_line_start.as_bytes())
            || own_line_start.as_bytes().starts_with(unended),
        "{name}: the {} bytes after its last whole line are not the start of this run's own line",
        unended.len()
    );
    let lines = parse_trace_lines(landed, WritingProcess::Ended)
        .unwrap_or_else(|failure| panic!("{name}: {failure}"));
    for line in &lines {
        assert_eq!(line.run, run, "{name}");
    }
    lines
}

/// Waits until the whole lines of a running process's trace file come to satisfy `holds` within `WHOLE_LINE_WAIT`, looking at once and again each time the file may have grown, and otherwise answers what was awaited and the lines the file held when the wait ran out.
pub fn wait_for_whole_lines(
    path: &Path,
    awaited: &str,
    holds: impl Fn(&[TraceLine]) -> bool,
) -> Result<(), String> {
    wait_for_whole_lines_within(path, awaited, WHOLE_LINE_WAIT, Instant::now, holds)
}

/// As `wait_for_whole_lines`, within the bound the caller gives, reading the time from `clock`.
fn wait_for_whole_lines_within(
    path: &Path,
    awaited: &str,
    bound: Duration,
    clock: impl FnMut() -> Instant,
    holds: impl Fn(&[TraceLine]) -> bool,
) -> Result<(), String> {
    let mut watch = GrowthWatch::of_path(path)?;
    let held = first_found_as_files_grow(
        &mut watch,
        bound,
        clock,
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
        Ok(()) => Ok(()),
        Err(WaitEnded::RanOut) => Err(format!(
            "{}: no {awaited} within {bound:?}; its lines so far:\n{}",
            path.display(),
            lines_held_so_far(path)
        )),
        Err(WaitEnded::WatcherFailed(failure)) => Err(format!(
            "{}: the file watcher failed: {failure}",
            path.display()
        )),
        Err(WaitEnded::WatcherEnded) => Err(format!("{}: the file watcher ended", path.display())),
    }
}

/// The whole lines of a running process's trace file one to a row, or the words saying it holds none.
fn lines_held_so_far(path: &Path) -> String {
    let lines = whole_lines_of_a_run_file_read_as(path, WritingProcess::Running);
    if lines.is_empty() {
        NO_EVENT_RECORDED.to_owned()
    } else {
        lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use crate::fresh_directory::fresh_directory_in;
    use crate::remove_scratch::remove_scratch;

    use super::super::trace_reader::tests::{
        MEMBER_LINE, MEMBER_RUN, member_line_numbered,
        second_line_still_zero_at_its_start_sized_ahead, sized_ahead,
    };
    use super::*;

    const SECOND_LINE_LANDED: &str = "a second line to land";
    const ANY_LINE: &str = "any line";
    const NEVER_WRITTEN: &str = "a line never written";
    const WAIT_BOUND: Duration = Duration::from_secs(10);

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
        let landed = sized_ahead(&landed);
        let (looked, first_look) = mpsc::channel::<Vec<u64>>();
        let landing_file = run_file.clone();
        let landing = std::thread::spawn(move || {
            let numbers_at_the_first_look = first_look.recv();
            fs::write(&landing_file, landed).unwrap();
            numbers_at_the_first_look
        });

        let held = wait_for_whole_lines(&run_file, SECOND_LINE_LANDED, |lines| {
            let numbers: Vec<u64> = lines.iter().map(|line| line.n).collect();
            drop(looked.send(numbers.clone()));
            numbers == [4, 5]
        });
        let numbers_at_the_first_look = landing.join().unwrap();
        remove_scratch(run_file.parent().unwrap()).unwrap();

        assert_eq!(numbers_at_the_first_look, Ok(vec![4]));
        assert_eq!(held, Ok(()));
    }

    #[test]
    fn observability_decision_12_the_wait_on_a_live_file_still_refuses_a_whole_line_that_is_no_trace_line()
     {
        let mut bytes = MEMBER_LINE.as_bytes().to_vec();
        bytes.extend_from_slice(b"not a trace line\n");
        let run_file = fresh_run_file("no-trace-line", &sized_ahead(&bytes));

        let refusal =
            std::panic::catch_unwind(|| wait_for_whole_lines(&run_file, ANY_LINE, |_lines| true))
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
        let run_file = fresh_run_file("junk-tail", &sized_ahead(&bytes));

        let refusal =
            std::panic::catch_unwind(|| wait_for_whole_lines(&run_file, ANY_LINE, |_lines| true))
                .map_err(|payload| payload.downcast_ref::<String>().cloned());
        remove_scratch(run_file.parent().unwrap()).unwrap();

        let refusal_text = refusal.unwrap_err().unwrap();
        assert!(
            refusal_text.ends_with("are not the start of this run's own line"),
            "{refusal_text}"
        );
    }

    #[test]
    fn observability_decision_12_a_wait_on_a_live_file_that_runs_out_answers_what_it_awaited_and_the_lines_so_far()
     {
        let run_file = fresh_run_file(
            "runs-out",
            &second_line_still_zero_at_its_start_sized_ahead(),
        );
        let started = Instant::now();
        let past_the_bound = started.checked_add(WAIT_BOUND).unwrap();
        let mut readings = std::iter::once(started);
        let expected_row = parse_trace_lines(MEMBER_LINE.as_bytes(), WritingProcess::Ended)
            .unwrap()[0]
            .to_string();

        let ran_out = wait_for_whole_lines_within(
            &run_file,
            NEVER_WRITTEN,
            WAIT_BOUND,
            move || readings.next().unwrap_or(past_the_bound),
            |_lines| false,
        );
        remove_scratch(run_file.parent().unwrap()).unwrap();

        assert_eq!(
            ran_out,
            Err(format!(
                "{}: no {NEVER_WRITTEN} within {WAIT_BOUND:?}; its lines so far:\n{expected_row}",
                run_file.display()
            ))
        );
    }
}
