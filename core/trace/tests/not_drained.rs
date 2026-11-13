/// The stop of spec/observability.md decision 12, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::ffi::OsString;
    use std::io::{self, Write};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::time::Duration;

    use estafette_trace::{
        CreateTraceFile, EstafetteRun, ListFiles, RUN_ID_BYTES, RunId, TraceBounds, TraceClocks,
        TraceDirectory, TraceEffects, TraceFile, TraceFileKind, TraceStop,
    };

    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const DATA_HOME_NEVER_CREATED: &str = "/estafette-trace-test-never-created";
    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
    const FROZEN_MONOTONIC_MS: u64 = 7;
    const NOW_MS: u64 = 2_000_000_000_000;
    const ONE_EVENT: &str = "process.clock_before_unix_epoch";
    const SHORT_DRAIN: Duration = Duration::from_millis(20);

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
    fn observability_decision_12_a_writer_still_holding_lines_at_its_bound_is_reported_not_drained_for_its_own_file()
     {
        let (to_the_test, held_unsticks) = mpsc::channel();
        let list_nothing: ListFiles = Arc::new(|_directory| Ok(Vec::new()));
        let effects = TraceEffects {
            create_file: stuck_files(to_the_test),
            list_files: list_nothing,
            ..TraceEffects::of_the_operating_system(TraceClocks {
                monotonic_ms: Arc::new(|| FROZEN_MONOTONIC_MS),
                wall_ms: Arc::new(|| NOW_MS),
            })
        };
        let directory = TraceDirectory::resolve(&|name: &str| {
            (name == DATA_HOME_VARIABLE).then(|| OsString::from(DATA_HOME_NEVER_CREATED))
        })
        .unwrap();
        let run = EstafetteRun::begin(
            &directory,
            &|_path: &Path| Ok(()),
            RunId::from_random_bytes(RUN_BYTES),
            &effects,
            TraceBounds {
                drain: SHORT_DRAIN,
                ..TraceBounds::SPEC
            },
        )
        .unwrap();
        let events = run.open_events().unwrap();
        let raw = run.open_raw().unwrap();
        events.write(ONE_EVENT, &[]);
        raw.write(ONE_EVENT, &[]);

        let stops = [events.finish(), raw.finish()];

        assert!(
            matches!(
                stops,
                [
                    TraceStop::NotDrained {
                        file: TraceFileKind::Events
                    },
                    TraceStop::NotDrained {
                        file: TraceFileKind::Raw
                    }
                ]
            ),
            "{stops:?}"
        );
        drop(held_unsticks);
    }
}
