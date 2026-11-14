#[cfg(test)]
#[path = "../../tests/common/fresh_directory.rs"]
mod fresh_directory;
#[cfg(test)]
#[path = "../../tests/common/remove_scratch.rs"]
mod remove_scratch;
#[cfg(test)]
#[path = "support/scratch_run.rs"]
mod scratch_run;

/// The stop a flush reports for each way it can leave a trace file, by spec/observability.md decision 12, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::fs;
    use std::io::{self, Write};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use super::remove_scratch::remove_scratch;
    use super::scratch_run::{ONE_EVENT, frozen_clocks, one_event_line, scratch_run};
    use estafette_trace::{
        CreateTraceFile, TraceBounds, TraceEffects, TraceFile, TraceFileKind, TraceStop,
    };

    const SHORT_DRAIN: Duration = Duration::from_millis(20);
    const STAND_IN_SHRINK_REFUSAL: &str = "the file refuses to shrink";

    /// Effects on the frozen clocks that create every trace file through `create_file`.
    fn effects_creating(create_file: CreateTraceFile) -> TraceEffects {
        TraceEffects {
            create_file,
            ..TraceEffects::of_the_operating_system(frozen_clocks())
        }
    }

    #[test]
    fn observability_decision_12_a_flush_after_the_stop_closed_the_queue_reports_drained() {
        let scratch = scratch_run(
            "flush-after-finish",
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds::SPEC,
        );
        let events = scratch.run.open_events().unwrap();
        assert!(matches!(events.finish(), TraceStop::Drained));

        let stop = events.flush();

        assert!(matches!(stop, TraceStop::Drained), "{stop:?}");
        remove_scratch(&scratch.root).unwrap();
    }

    #[test]
    fn observability_decision_12_a_flush_whose_writer_wrote_every_earlier_line_and_cut_the_file_back_reports_drained()
     {
        let scratch = scratch_run(
            "flush-drained",
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds::SPEC,
        );
        let events = scratch.run.open_events().unwrap();
        events.write(ONE_EVENT, &[]);
        events.write(ONE_EVENT, &[]);

        let stop = events.flush();

        let on_disk = fs::read_to_string(&scratch.run.files().events).unwrap();
        assert!(matches!(stop, TraceStop::Drained), "{stop:?}");
        assert_eq!(on_disk, [one_event_line(0), one_event_line(1)].concat());
        assert!(matches!(events.finish(), TraceStop::Drained));
        remove_scratch(&scratch.root).unwrap();
    }

    /// A trace file that takes every write and refuses to be cut back below the length it was sized to.
    struct UncuttableFile {
        sized: u64,
    }

    impl Write for UncuttableFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for UncuttableFile {
        fn set_length(&mut self, length: u64) -> io::Result<()> {
            if length < self.sized {
                return Err(io::Error::other(STAND_IN_SHRINK_REFUSAL));
            }
            self.sized = length;
            Ok(())
        }
    }

    #[test]
    fn observability_decision_12_a_flush_whose_file_cannot_be_cut_back_reports_not_cut_for_its_file_with_the_cause()
     {
        let scratch = scratch_run(
            "flush-not-cut",
            &effects_creating(Arc::new(|_path: &Path| -> io::Result<Box<dyn TraceFile>> {
                Ok(Box::new(UncuttableFile { sized: 0 }))
            })),
            TraceBounds::SPEC,
        );
        let events = scratch.run.open_events().unwrap();
        events.write(ONE_EVENT, &[]);

        let stop = events.flush();

        let TraceStop::NotCut {
            file: TraceFileKind::Events,
            source,
        } = stop
        else {
            panic!("{stop:?}");
        };
        assert_eq!(source.to_string(), STAND_IN_SHRINK_REFUSAL);
        remove_scratch(&scratch.root).unwrap();
    }

    /// A trace file whose write never returns while the test holds the sender that would unstick it.
    struct StuckFile {
        until_unstuck: Receiver<()>,
    }

    impl Write for StuckFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.until_unstuck.recv().map_err(io::Error::other)?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for StuckFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    /// Creates a stuck file for every path, handing the sender that would unstick each file to the test, which keeps every writer stuck by holding them.
    fn stuck_files(to_the_test: Sender<Sender<()>>) -> CreateTraceFile {
        Arc::new(move |_path: &Path| -> io::Result<Box<dyn TraceFile>> {
            let (unstick, until_unstuck) = mpsc::channel();
            to_the_test.send(unstick).map_err(io::Error::other)?;
            Ok(Box::new(StuckFile { until_unstuck }))
        })
    }

    #[test]
    fn observability_decision_12_a_flush_whose_writer_does_not_answer_within_the_drain_bound_reports_not_drained_for_its_file()
     {
        let (to_the_test, held_unsticks) = mpsc::channel();
        let scratch = scratch_run(
            "flush-not-drained",
            &effects_creating(stuck_files(to_the_test)),
            TraceBounds {
                drain: SHORT_DRAIN,
                ..TraceBounds::SPEC
            },
        );
        let events = scratch.run.open_events().unwrap();
        events.write(ONE_EVENT, &[]);

        let stop = events.flush();

        assert!(
            matches!(
                stop,
                TraceStop::NotDrained {
                    file: TraceFileKind::Events
                }
            ),
            "{stop:?}"
        );
        drop(held_unsticks);
        remove_scratch(&scratch.root).unwrap();
    }
}
