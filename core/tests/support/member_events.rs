use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::file_growth::{GrowthWatch, WaitEnded, first_found_as_files_grow};
use crate::trace_reader::{
    TraceLine, WritingProcess, events_files_in, lines_of_files_still_present, timeline_read_as,
};

/// The counters snapshot a running node records at its start, at each rebind and every 60 s (spec/observability.md decision 9).
pub const COUNTERS_EVENT: &str = "state.counters";

/// The events one member of a test records in the trace directory of its own, awaited on the operating system's notice that an events file grew.
#[derive(Clone)]
pub struct MemberEvents {
    label: String,
    traces_directory: PathBuf,
}

impl MemberEvents {
    /// The events the member of that label records in the trace directory, which must exist when a wait begins.
    pub fn new(label: &str, traces_directory: &Path) -> Self {
        Self {
            label: label.to_owned(),
            traces_directory: traces_directory.to_path_buf(),
        }
    }

    /// The first value `found_in` makes of every line of the member's events files, looking at once and again each time an events file may have grown, failing with what was awaited and the member's timeline so far when the bound runs out first.
    #[track_caller]
    pub fn wait_until<Found>(
        &self,
        awaited: &str,
        within: Duration,
        found_in: impl FnMut(Vec<TraceLine>) -> Option<Found>,
    ) -> Found {
        self.wait_until_after(awaited, within, Instant::now, || {}, found_in)
    }

    /// As `wait_until`, reading the time from `clock` and running `once_waiting` once the first look found nothing, so a test can write the line the wait must then be woken for.
    #[track_caller]
    pub fn wait_until_after<Found>(
        &self,
        awaited: &str,
        within: Duration,
        clock: impl FnMut() -> Instant,
        once_waiting: impl FnOnce(),
        mut found_in: impl FnMut(Vec<TraceLine>) -> Option<Found>,
    ) -> Found {
        let mut watch = GrowthWatch::of_path(&self.traces_directory)
            .unwrap_or_else(|failure| panic!("{}: {failure}", self.label));
        let found = first_found_as_files_grow(&mut watch, within, clock, once_waiting, |watch| {
            self.watch_new_events_files(watch);
            found_in(self.lines())
        });
        match found {
            Ok(found) => found,
            Err(WaitEnded::RanOut) => panic!("{}", self.ran_out(awaited, within)),
            Err(WaitEnded::WatcherFailed(failure)) => {
                panic!("{}: the file watcher failed: {failure}", self.label)
            }
            Err(WaitEnded::WatcherEnded) => panic!("{}: the file watcher ended", self.label),
        }
    }

    /// What a wait says when its bound ran out: what it awaited and the member's timeline so far.
    fn ran_out(&self, awaited: &str, within: Duration) -> String {
        format!(
            "{}: no {awaited} within {within:?}; its timeline so far:\n{}",
            self.label,
            timeline_read_as(&self.traces_directory, WritingProcess::MayStillBeWriting)
        )
    }

    /// Starts watching each events file of the directory not yet watched, because a kqueue reports a file's growth to a watch on the file alone.
    #[track_caller]
    fn watch_new_events_files(&self, watch: &mut GrowthWatch) {
        let events_files = events_files_in(&self.traces_directory)
            .unwrap_or_else(|failure| panic!("{}: {failure}", self.label));
        for file in events_files {
            watch
                .watch_path(&file)
                .unwrap_or_else(|failure| panic!("{}: {failure}", self.label));
        }
    }

    /// Every line of the member's events files written so far, passing over a file removed since the listing named it, so the wait looks again on the next notice.
    #[track_caller]
    fn lines(&self) -> Vec<TraceLine> {
        events_files_in(&self.traces_directory)
            .and_then(|files| lines_of_files_still_present(&files))
            .unwrap_or_else(|failure| panic!("{}: {failure}", self.label))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::remove_scratch::remove_scratch;
    use crate::this_process_directory::fresh_directory_of_this_process_in;
    use estafette_trace_reader::line_fixtures::{
        MEMBER_LINE, MEMBER_RUN, second_line_still_zero_at_its_start_sized_ahead,
    };
    use estafette_trace_reader::parse_trace_lines;

    const LABEL: &str = "the member";
    const SCRATCH_NAME: &str = "member-events-still-zero";
    const NEVER_WRITTEN: &str = "a line never written";
    const WAIT_BOUND: Duration = Duration::from_secs(10);

    #[test]
    fn observability_decision_12_a_live_wait_that_runs_out_prints_the_lines_before_one_still_zero_filled()
     {
        let traces =
            fresh_directory_of_this_process_in(&std::env::temp_dir(), SCRATCH_NAME).unwrap();
        fs::write(
            traces.join(format!(
                "20261112T225914Z-{MEMBER_RUN}-estafette.events.jsonl"
            )),
            second_line_still_zero_at_its_start_sized_ahead(),
        )
        .unwrap();
        let started = Instant::now();
        let past_the_bound = started.checked_add(WAIT_BOUND).unwrap();
        let mut readings = std::iter::once(started);
        let expected_row = parse_trace_lines(MEMBER_LINE.as_bytes(), WritingProcess::Ended)
            .unwrap()[0]
            .to_string();

        let ran_out = std::panic::catch_unwind(|| {
            MemberEvents::new(LABEL, &traces).wait_until_after(
                NEVER_WRITTEN,
                WAIT_BOUND,
                move || readings.next().unwrap_or(past_the_bound),
                || {},
                |_lines| None::<()>,
            );
        })
        .map_err(|payload| payload.downcast_ref::<String>().cloned());
        remove_scratch(&traces).unwrap();

        assert_eq!(
            ran_out,
            Err(Some(format!(
                "{LABEL}: no {NEVER_WRITTEN} within {WAIT_BOUND:?}; its timeline so far:\n{expected_row}"
            )))
        );
    }
}
