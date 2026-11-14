#[cfg(test)]
#[path = "common/remove_scratch.rs"]
mod remove_scratch;

#[cfg(test)]
#[path = "support/raw_iroh_lines_trace.rs"]
mod raw_iroh_lines_trace;

#[cfg(test)]
#[path = "support/raw_iroh_lines_header.rs"]
mod raw_iroh_lines_header;

/// The rollover of `spec/observability.md` decision 13 under iroh's lines, through the install the composition root calls, in a process of its own.
#[cfg(test)]
mod raw_iroh_lines_rollover {
    use std::fs;
    use std::sync::Arc;

    use estafette::dirs::rolled_over_trace_file;
    use estafette::trace::iroh_lines::install;
    use estafette::trace::{TraceBounds, TraceEffects, TraceStop};

    use super::raw_iroh_lines_header::{header, raw_file_opened_line};
    use super::raw_iroh_lines_trace::{frozen_clocks, scratch_trace_directory, started};
    use super::remove_scratch::remove_scratch;

    const SPEC_IROH_LINE: &str = "iroh_line";
    const DIRECT_ADDRS_LINE_AFTER_HEADER: &str = ",\"target\":\"iroh::_events::direct_addrs\",\"level\":\"DEBUG\",\"message\":\"a line\",\"fields\":{}}\n";

    /// Writes one line of `iroh::_events::direct_addrs` with the message "a line".
    fn one_direct_addrs_line() {
        tracing::debug!(target: "iroh::_events::direct_addrs", "a line");
    }

    /// The raw line numbered `n` that `one_direct_addrs_line` writes.
    fn direct_addrs_line(n: u64) -> String {
        format!(
            "{}{DIRECT_ADDRS_LINE_AFTER_HEADER}",
            header(n, None, SPEC_IROH_LINE)
        )
    }

    #[test]
    fn observability_decision_13_iroh_lines_roll_the_raw_file_over_and_each_raw_file_opens_with_its_first_line()
     {
        const LINES_WRITTEN: u64 = 3;
        let (root, directory) = scratch_trace_directory("rollover");
        let first_line_bytes = u64::try_from(raw_file_opened_line().len()).unwrap();
        let iroh_line_bytes = u64::try_from(direct_addrs_line(1).len()).unwrap();
        let trace = started(
            &directory,
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds {
                rollover_bytes: first_line_bytes + iroh_line_bytes,
                ..TraceBounds::SPEC
            },
        );

        install(Arc::clone(&trace)).unwrap();
        for _line in 0..LINES_WRITTEN {
            one_direct_addrs_line();
        }
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));

        let raw = &trace.files().raw;
        assert_eq!(
            fs::read_to_string(rolled_over_trace_file(raw)).unwrap(),
            format!("{}{}", raw_file_opened_line(), direct_addrs_line(2))
        );
        assert_eq!(
            fs::read_to_string(raw).unwrap(),
            format!("{}{}", raw_file_opened_line(), direct_addrs_line(3))
        );
        remove_scratch(&root).unwrap();
    }
}
