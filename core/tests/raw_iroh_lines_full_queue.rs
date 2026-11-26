#[cfg(test)]
use estafette::trace::{TraceBounds, TraceEffects, TraceFile};

#[cfg(test)]
#[path = "common/held_back_trace_file.rs"]
mod held_back_trace_file;

#[cfg(test)]
#[path = "common/remove_scratch.rs"]
mod remove_scratch;

#[cfg(test)]
#[path = "support/raw_iroh_lines_trace.rs"]
mod raw_iroh_lines_trace;

/// The queue bound and the `dropped` line of `spec/observability.md` decision 12 under iroh's lines, through the install the composition root calls, in a process of its own.
#[cfg(test)]
mod raw_iroh_lines_full_queue {
    use std::path::Path;
    use std::sync::Arc;

    use estafette::dirs::TraceDirectory;
    use estafette::event::{EventName, TraceLineName};
    use estafette::trace::iroh_lines::install;
    use estafette::trace::{Trace, TraceEffects, TraceStop};
    use serde_json::Value;

    use super::held_back_trace_file::{
        HeldBackEnds, SMALL_QUEUE_LINES, effects_holding_back, small_queue_bounds, within_bound,
    };
    use super::raw_iroh_lines_trace::{frozen_clocks, scratch_trace_directory, started};
    use super::remove_scratch::remove_scratch;

    const SPEC_DROPPED: &str = "dropped";
    const RAW_FILE_SUFFIX: &str = ".raw.jsonl";

    /// Writes one line of a target the raw file takes.
    fn one_iroh_line() {
        tracing::debug!(target: "iroh::_events::direct_addrs", "a line");
    }

    /// Whether the path names a raw file.
    fn is_raw_file(path: &Path) -> bool {
        path.to_str()
            .is_some_and(|path| path.ends_with(RAW_FILE_SUFFIX))
    }

    /// A trace whose raw file the test holds back and whose events file is discarded, its queues `SMALL_QUEUE_LINES` long.
    fn held_back_trace(directory: &TraceDirectory) -> (Arc<Trace>, HeldBackEnds) {
        let (effects, raw) = effects_holding_back(
            TraceEffects::of_the_operating_system(frozen_clocks()),
            is_raw_file,
        );
        let trace = started(directory, &effects, small_queue_bounds());
        (trace, raw)
    }

    /// The next line the held-back raw file writes once the test releases it: its number, its event and the count a `dropped` line carries.
    fn released_line(raw: &HeldBackEnds) -> (u64, String, Option<u64>) {
        raw.release.send(()).unwrap();
        let line: Value =
            serde_json::from_str(&within_bound(&raw.written, "a released line is written"))
                .unwrap();
        (
            line["n"].as_u64().unwrap(),
            line["event"].as_str().unwrap().to_owned(),
            line["count"].as_u64(),
        )
    }

    #[test]
    fn observability_decision_12_the_iroh_lines_a_full_raw_queue_loses_are_counted_and_the_next_line_is_a_dropped_line_where_n_skips()
     {
        const LINES_PAST_THE_HELD_FIRST_LINE: usize = SMALL_QUEUE_LINES + 2;
        let (root, directory) = scratch_trace_directory("full-queue");
        let (trace, raw) = held_back_trace(&directory);
        within_bound(&raw.entering, "the writer holds the raw file's first line");

        install(Arc::clone(&trace)).unwrap();
        for _queued_or_lost in 0..LINES_PAST_THE_HELD_FIRST_LINE {
            one_iroh_line();
        }
        let held: Vec<_> = (0..=SMALL_QUEUE_LINES)
            .map(|_line| released_line(&raw))
            .collect();
        one_iroh_line();

        assert_eq!(
            held,
            [
                (0, TraceLineName::RawFileOpened.as_str().to_owned(), None),
                (1, TraceLineName::IrohLine.as_str().to_owned(), None),
                (2, TraceLineName::IrohLine.as_str().to_owned(), None),
            ]
        );
        assert_eq!(released_line(&raw), (5, SPEC_DROPPED.to_owned(), Some(2)));
        assert_eq!(
            released_line(&raw),
            (6, TraceLineName::IrohLine.as_str().to_owned(), None)
        );
        drop(raw.release);
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));
        remove_scratch(&root).unwrap();
    }
}
