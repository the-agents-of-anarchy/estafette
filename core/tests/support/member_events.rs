use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::file_growth::{GrowthWatch, WaitEnded, first_found_as_files_grow};
use crate::trace_reader::{TraceLine, events_files_in, lines_of_files_still_present, timeline_in};

/// The counters snapshot a running node records at its start, at each rebind and every 60 s (spec/observability.md decision 9).
pub const COUNTERS_EVENT: &str = "state.counters";
/// The longest a running node goes without a counters snapshot: one every 60 s, and one at each rebind, which starts the 60 s afresh (spec/observability.md decisions 9 and 13).
const SPEC_COUNTERS_INTERVAL: Duration = Duration::from_secs(60);
/// What a snapshot may lag its interval by on a loaded machine.
const SNAPSHOT_SLACK: Duration = Duration::from_secs(15);

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

    /// Waits until the member records a counters snapshot at least `span` after the wall-clock instant, the event its own time produces, proof that much of it passed.
    #[track_caller]
    pub fn wait_for_time_to_pass(&self, since_wall_ms: u64, span: Duration) {
        let due_wall_ms = since_wall_ms.saturating_add(u64::try_from(span.as_millis()).unwrap());
        let within = span
            .saturating_add(SPEC_COUNTERS_INTERVAL)
            .saturating_add(SNAPSHOT_SLACK);
        self.wait_until(
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

    /// What a wait says when its bound ran out: what it awaited and the member's timeline so far.
    fn ran_out(&self, awaited: &str, within: Duration) -> String {
        format!(
            "{}: no {awaited} within {within:?}; its timeline so far:\n{}",
            self.label,
            timeline_in(&self.traces_directory)
        )
    }

    /// Starts watching each events file of the directory not yet watched, because a kqueue reports a file's growth to a watch on the file alone.
    #[track_caller]
    fn watch_new_events_files(&self, watch: &mut GrowthWatch) {
        let events_files = events_files_in(&self.traces_directory)
            .unwrap_or_else(|failure| panic!("{}: {failure}", self.label));
        for file in events_files {
            watch
                .watch_file(&file)
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
