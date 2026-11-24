#[cfg(test)]
#[path = "support/frozen_run.rs"]
mod frozen_run;
#[cfg(test)]
#[path = "support/stuck_files.rs"]
mod stuck_files;

/// The stop of spec/observability.md decision 12, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::Duration;

    use super::frozen_run::{DATA_HOME_VARIABLE, NOW_MS, RUN_BYTES, frozen_clocks_at};
    use super::stuck_files::stuck_files;
    use estafette_trace::{
        EstafetteRun, ListFiles, RunId, TraceBounds, TraceDirectory, TraceEffects, TraceFileKind,
        TraceStop,
    };

    const DATA_HOME_NEVER_CREATED: &str = "/estafette-trace-test-never-created";
    const ONE_EVENT: &str = "process.clock_before_unix_epoch";
    const SHORT_DRAIN: Duration = Duration::from_millis(20);

    #[test]
    fn observability_decision_12_a_writer_still_holding_lines_at_its_bound_is_reported_not_drained_for_its_own_file()
     {
        let (to_the_test, held_unsticks) = mpsc::channel();
        let list_nothing: ListFiles = Arc::new(|_directory| Ok(Vec::new()));
        let effects = TraceEffects {
            create_file: stuck_files(to_the_test),
            list_files: list_nothing,
            ..TraceEffects::of_the_operating_system(frozen_clocks_at(NOW_MS))
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
