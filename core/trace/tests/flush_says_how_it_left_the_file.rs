#[cfg(test)]
#[path = "../../tests/common/fresh_directory.rs"]
mod fresh_directory;
#[cfg(test)]
#[path = "../../tests/common/remove_scratch.rs"]
mod remove_scratch;

/// The stop a flush reports for each way it can leave a trace file, by spec/observability.md decision 12, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::ffi::OsString;
    use std::fs;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use super::fresh_directory::fresh_directory_in;
    use super::remove_scratch::remove_scratch;
    use estafette_trace::{
        CreateTraceFile, EstafetteRun, FileTrace, ListFiles, RUN_ID_BYTES, RunId, TraceBounds,
        TraceClocks, TraceDirectory, TraceEffects, TraceFile, TraceFileKind, TraceStop,
    };

    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const DATA_HOME_NEVER_CREATED: &str = "/estafette-trace-test-never-created";
    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
    const RUN_HEX: &str = "a1b2c3d4e5f60718";
    const FROZEN_MONOTONIC_MS: u64 = 7;
    const NOW_MS: u64 = 2_000_000_000_000;
    const ONE_EVENT: &str = "process.clock_before_unix_epoch";
    const SHORT_DRAIN: Duration = Duration::from_millis(20);
    const STAND_IN_SHRINK_REFUSAL: &str = "the file refuses to shrink";

    /// Clocks that always read `FROZEN_MONOTONIC_MS` and `NOW_MS`.
    fn frozen_clocks() -> TraceClocks {
        TraceClocks {
            monotonic_ms: Arc::new(|| FROZEN_MONOTONIC_MS),
            wall_ms: Arc::new(|| NOW_MS),
        }
    }

    /// The events file of a run whose files `create_file` creates in a trace directory that is never created, its writer stopped within `drain`.
    fn events_trace_creating(create_file: CreateTraceFile, drain: Duration) -> FileTrace {
        let list_nothing: ListFiles = Arc::new(|_directory| Ok(Vec::new()));
        let effects = TraceEffects {
            create_file,
            list_files: list_nothing,
            ..TraceEffects::of_the_operating_system(frozen_clocks())
        };
        let directory = TraceDirectory::resolve(&|name: &str| {
            (name == DATA_HOME_VARIABLE).then(|| OsString::from(DATA_HOME_NEVER_CREATED))
        })
        .unwrap();
        EstafetteRun::begin(
            &directory,
            &|_path: &Path| Ok(()),
            RunId::from_random_bytes(RUN_BYTES),
            &effects,
            TraceBounds {
                drain,
                ..TraceBounds::SPEC
            },
        )
        .unwrap()
        .open_events()
        .unwrap()
    }

    /// The line the run writes for `ONE_EVENT` as its line `n`.
    fn one_event_line(n: u64) -> String {
        format!(
            "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":{n},\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{NOW_MS},\"event\":\"{ONE_EVENT}\"}}\n"
        )
    }

    #[test]
    fn observability_decision_12_a_flush_after_the_stop_closed_the_queue_reports_drained() {
        let scratch = scratch_events_trace("flush-after-finish");
        assert!(matches!(scratch.events.finish(), TraceStop::Drained));

        let stop = scratch.events.flush();

        assert!(matches!(stop, TraceStop::Drained), "{stop:?}");
        remove_scratch(&scratch.root).unwrap();
    }

    #[test]
    fn observability_decision_12_a_flush_whose_writer_wrote_every_earlier_line_and_cut_the_file_back_reports_drained()
     {
        let scratch = scratch_events_trace("flush-drained");
        scratch.events.write(ONE_EVENT, &[]);
        scratch.events.write(ONE_EVENT, &[]);

        let stop = scratch.events.flush();

        let on_disk = fs::read_to_string(&scratch.file).unwrap();
        assert!(matches!(stop, TraceStop::Drained), "{stop:?}");
        assert_eq!(on_disk, [one_event_line(0), one_event_line(1)].concat());
        assert!(matches!(scratch.events.finish(), TraceStop::Drained));
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
        let events = events_trace_creating(
            Arc::new(|_path: &Path| -> io::Result<Box<dyn TraceFile>> {
                Ok(Box::new(UncuttableFile { sized: 0 }))
            }),
            SHORT_DRAIN,
        );
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
        let events = events_trace_creating(stuck_files(to_the_test), SHORT_DRAIN);
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
    }

    /// An events trace on the operating system's files under a fresh scratch root, with the root to remove and the file it writes.
    struct ScratchEventsTrace {
        root: PathBuf,
        file: PathBuf,
        events: FileTrace,
    }

    /// An events trace on the operating system's files under a fresh scratch root named for the test.
    fn scratch_events_trace(test_name: &str) -> ScratchEventsTrace {
        let root = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-trace-flush-{test_name}-{}", std::process::id()),
        )
        .unwrap();
        let data_home = root.join("data");
        let directory = TraceDirectory::resolve(&move |name: &str| {
            (name == DATA_HOME_VARIABLE).then(|| OsString::from(&data_home))
        })
        .unwrap();
        let run = EstafetteRun::begin(
            &directory,
            &|path: &Path| fs::create_dir_all(path),
            RunId::from_random_bytes(RUN_BYTES),
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds::SPEC,
        )
        .unwrap();
        let events = run.open_events().unwrap();
        ScratchEventsTrace {
            root,
            file: run.files().events.clone(),
            events,
        }
    }
}
