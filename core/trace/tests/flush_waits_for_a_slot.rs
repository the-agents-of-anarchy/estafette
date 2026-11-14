#[cfg(test)]
#[path = "../../tests/common/fresh_directory.rs"]
mod fresh_directory;
#[cfg(test)]
#[path = "../../tests/common/remove_scratch.rs"]
mod remove_scratch;
#[cfg(test)]
#[path = "support/scratch_run.rs"]
mod scratch_run;

/// A flush that meets a full queue waits for a slot within its one drain bound, by spec/observability.md decision 12, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::remove_scratch::remove_scratch;
    use super::scratch_run::{ONE_EVENT, frozen_clocks, one_event_line, scratch_run};
    use estafette_trace::{
        FileTrace, SpawnThread, TraceBounds, TraceEffects, TraceFileKind, TraceStop,
    };

    const QUEUE_LINES: usize = 3;
    const HELD_WRITER_DRAIN: Duration = Duration::from_secs(3);
    const ONE_DRAIN_BOUND_OVERRUN_ADMITTED: Duration = Duration::from_millis(1500);
    const FLUSH_ANSWERS_WITHIN: Duration = Duration::from_secs(10);
    const TEST_GONE: &str = "the test that takes the writer is gone";

    /// A writer's work as the trace hands it to its thread, which these tests start or hold.
    type WriterWork = Box<dyn FnOnce() + Send>;

    /// An events trace on the operating system's files under a fresh scratch root, its writer's work handed to the test instead of a thread, with the root to remove and the file it writes.
    struct HeldWriterTrace {
        root: PathBuf,
        file: PathBuf,
        events: Arc<FileTrace>,
        writer: Receiver<WriterWork>,
    }

    /// An events trace of `QUEUE_LINES` queued lines under a fresh scratch root named for the test, stopped within `drain`, whose writer runs only when the test starts it.
    fn held_writer_trace(test_name: &str, drain: Duration) -> HeldWriterTrace {
        let (to_the_test, writer) = mpsc::channel();
        let hand_the_writer_to_the_test: SpawnThread =
            Arc::new(move |_name: &str, work: WriterWork| {
                to_the_test
                    .send(work)
                    .map_err(|_test_gone| io::Error::other(TEST_GONE))
            });
        let scratch = scratch_run(
            test_name,
            &TraceEffects {
                spawn_thread: hand_the_writer_to_the_test,
                ..TraceEffects::of_the_operating_system(frozen_clocks())
            },
            TraceBounds {
                queue_lines: QUEUE_LINES,
                drain,
                ..TraceBounds::SPEC
            },
        );
        HeldWriterTrace {
            events: Arc::new(scratch.run.open_events().unwrap()),
            file: scratch.run.files().events.clone(),
            root: scratch.root,
            writer,
        }
    }

    /// Fills the queue of a writer that has not started, so its next line or marker finds no slot.
    fn fill_the_queue(events: &FileTrace) {
        for _ in 0..QUEUE_LINES {
            events.write(ONE_EVENT, &[]);
        }
    }

    #[test]
    fn observability_decision_12_a_flush_meeting_a_full_queue_waits_for_a_slot_and_reports_drained_with_every_earlier_line_on_disk()
     {
        let trace = held_writer_trace("flush-slot-live", TraceBounds::SPEC.drain);
        fill_the_queue(&trace.events);
        let (flushing, about_to_flush) = mpsc::channel();
        let (answer, flush_answered) = mpsc::channel();
        let events = Arc::clone(&trace.events);
        thread::spawn(move || {
            flushing.send(()).unwrap();
            answer.send(events.flush()).unwrap();
        });
        about_to_flush
            .recv_timeout(FLUSH_ANSWERS_WITHIN)
            .expect("the flushing thread started within its bound");

        thread::spawn(trace.writer.recv().unwrap());

        let stop = flush_answered
            .recv_timeout(FLUSH_ANSWERS_WITHIN)
            .expect("the flush answered within its bound");
        let on_disk = fs::read_to_string(&trace.file).unwrap();
        assert!(matches!(stop, TraceStop::Drained), "{stop:?}");
        assert_eq!(
            on_disk,
            (0..QUEUE_LINES)
                .map(|n| one_event_line(u64::try_from(n).unwrap()))
                .collect::<String>()
        );
        assert!(matches!(trace.events.finish(), TraceStop::Drained));
        remove_scratch(&trace.root).unwrap();
    }

    #[test]
    fn observability_decision_12_a_flush_meeting_a_full_queue_whose_writer_never_takes_a_line_reports_not_drained_once_its_one_drain_bound_passed_and_no_second()
     {
        let trace = held_writer_trace("flush-slot-held", HELD_WRITER_DRAIN);
        fill_the_queue(&trace.events);
        let (answer, flush_answered) = mpsc::channel();
        let events = Arc::clone(&trace.events);

        thread::spawn(move || {
            let started = Instant::now();
            let stop = events.flush();
            answer.send((stop, started.elapsed())).unwrap();
        });

        let (stop, waited) = flush_answered
            .recv_timeout(FLUSH_ANSWERS_WITHIN)
            .expect("the flush answered within its bound");
        assert!(
            matches!(
                stop,
                TraceStop::NotDrained {
                    file: TraceFileKind::Events
                }
            ),
            "{stop:?}"
        );
        assert!(waited >= HELD_WRITER_DRAIN, "{waited:?}");
        assert!(
            waited < HELD_WRITER_DRAIN + ONE_DRAIN_BOUND_OVERRUN_ADMITTED,
            "{waited:?}"
        );
        drop(trace.writer);
        remove_scratch(&trace.root).unwrap();
    }
}
