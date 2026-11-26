#[cfg(test)]
#[path = "../../tests/common/fresh_directory.rs"]
mod fresh_directory;
#[cfg(test)]
#[path = "support/frozen_run.rs"]
mod frozen_run;
#[cfg(test)]
#[path = "../../tests/common/remove_scratch.rs"]
mod remove_scratch;
#[cfg(test)]
#[path = "support/scratch_run.rs"]
mod scratch_run;
#[path = "../../tests/common/this_process_directory.rs"]
mod this_process_directory;

/// The format version a written line carries, by spec/observability.md decision 11, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::fs;

    use super::remove_scratch::remove_scratch;
    use super::scratch_run::{ONE_EVENT, frozen_clocks, one_event_line, scratch_run};
    use estafette_trace::{FORMAT_VERSION, TraceBounds, TraceEffects, TraceStop};

    const FORMAT_VERSION_THE_LINES_PIN: u64 = 2;

    #[test]
    fn trace_format_version_rises_to_two_pins_the_version_every_written_line_carries() {
        let scratch = scratch_run(
            "format-version",
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds::SPEC,
        );
        let events = scratch.run.open_events().unwrap();
        events.write(ONE_EVENT, &[]);
        assert!(matches!(events.finish(), TraceStop::Drained));

        let written = fs::read_to_string(&scratch.run.files().events).unwrap();
        let line: serde_json::Value =
            serde_json::from_str(written.lines().next().unwrap()).unwrap();

        assert_eq!(line["v"], FORMAT_VERSION_THE_LINES_PIN);
        assert_eq!(written, one_event_line(0));
        assert_eq!(FORMAT_VERSION, FORMAT_VERSION_THE_LINES_PIN);
        remove_scratch(&scratch.root).unwrap();
    }
}
