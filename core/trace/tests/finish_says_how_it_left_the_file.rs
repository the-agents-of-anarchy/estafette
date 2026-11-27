#[cfg(test)]
#[path = "support/frozen_run.rs"]
mod frozen_run;

/// The stop a finish reports for each way it can leave a trace file, by spec/observability.md decision 12, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::ffi::OsString;
    use std::io::{self, Write};
    use std::path::Path;
    use std::sync::Arc;

    use super::frozen_run::{DATA_HOME_VARIABLE, NOW_MS, RUN_BYTES, frozen_clocks_at};
    use estafette_trace::{
        CreateTraceFile, EstafetteRun, ListFiles, RunId, TraceBounds, TraceDirectory, TraceEffects,
        TraceFile, TraceFileKind, TraceStop,
    };

    const DATA_HOME_NEVER_CREATED: &str = "/estafette-trace-test-never-created";
    const ONE_EVENT: &str = "process.clock_before_unix_epoch";
    const STAND_IN_SHRINK_REFUSAL: &str = "the file refuses to shrink";
    const STAND_IN_WRITE_REFUSAL: &str = "the file refuses every write";

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

    const LINES_THE_FILE_REFUSES: u64 = 3;

    /// A trace file that refuses every write and can be cut back to any length.
    struct RefusingFile;

    impl Write for RefusingFile {
        fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
            Err(io::Error::other(STAND_IN_WRITE_REFUSAL))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for RefusingFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    /// A run on the frozen clocks that creates every trace file through `create_file`, listing no old files and creating no directory.
    fn run_creating(create_file: CreateTraceFile) -> EstafetteRun {
        let list_nothing: ListFiles = Arc::new(|_directory| Ok(Vec::new()));
        let effects = TraceEffects {
            create_file,
            list_files: list_nothing,
            ..TraceEffects::of_the_operating_system(frozen_clocks_at(NOW_MS))
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
            TraceBounds::SPEC,
        )
        .unwrap()
    }

    #[test]
    fn observability_decision_12_a_second_finish_answers_what_the_first_finish_answered() {
        let run = run_creating(Arc::new(|_path: &Path| -> io::Result<Box<dyn TraceFile>> {
            Ok(Box::new(UncuttableFile { sized: 0 }))
        }));
        let events = run.open_events().unwrap();
        events.write(ONE_EVENT, &[]);

        let first = events.finish();
        let second = events.finish();

        let (
            TraceStop::NotCut {
                file: TraceFileKind::Events,
                source: first_source,
            },
            TraceStop::NotCut {
                file: TraceFileKind::Events,
                source: second_source,
            },
        ) = (&first, &second)
        else {
            panic!("{first:?} then {second:?}");
        };
        assert_eq!(first_source.to_string(), STAND_IN_SHRINK_REFUSAL);
        assert_eq!(second_source.to_string(), STAND_IN_SHRINK_REFUSAL);
    }

    #[test]
    fn observability_decision_12_a_finish_whose_file_refuses_the_dropped_line_reports_the_count_it_could_not_write()
     {
        let run = run_creating(Arc::new(|_path: &Path| -> io::Result<Box<dyn TraceFile>> {
            Ok(Box::new(RefusingFile))
        }));
        let events = run.open_events().unwrap();
        for _ in 0..LINES_THE_FILE_REFUSES {
            events.write(ONE_EVENT, &[]);
        }

        let stop = events.finish();

        assert!(
            matches!(
                stop,
                TraceStop::NotReported {
                    file: TraceFileKind::Events,
                    unreported: LINES_THE_FILE_REFUSES
                }
            ),
            "{stop:?}"
        );
    }
}
