#[cfg(test)]
#[path = "support/raw_iroh_lines_trace.rs"]
mod raw_iroh_lines_trace;

#[cfg(test)]
#[path = "support/raw_iroh_lines_header.rs"]
mod raw_iroh_lines_header;

/// The spans in scope at a raw line of `spec/observability.md` decision 7, through the install the composition root calls, in a process of its own.
#[cfg(test)]
mod raw_iroh_lines_spans {
    use std::fs;
    use std::sync::Arc;

    use estafette::trace::iroh_lines::install;
    use estafette::trace::{TraceBounds, TraceEffects, TraceStop};

    use super::raw_iroh_lines_header::{header, raw_file_opened_line};
    use super::raw_iroh_lines_trace::{frozen_clocks, scratch_trace_directory, started};

    const SPEC_IROH_LINE: &str = "iroh_line";

    /// How iroh-gossip's `Origin` reads in its lines.
    #[derive(Debug)]
    enum Origin {
        Accept,
    }

    #[test]
    fn observability_decision_7_a_raw_line_names_each_span_in_scope_outermost_first_with_its_fields_and_a_line_outside_every_span_names_none()
     {
        let (root, directory) = scratch_trace_directory("spans");
        let trace = started(
            &directory,
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds::SPEC,
        );

        install(Arc::clone(&trace)).unwrap();
        let of_a_target_the_raw_file_refuses =
            tracing::info_span!(target: "iroh::endpoint", "connect", remote = %"ef56a");
        of_a_target_the_raw_file_refuses.in_scope(|| {
            let gossip = tracing::error_span!(target: "iroh_gossip::net", "gossip", me = %"ab12c");
            gossip.in_scope(|| {
                let connection = tracing::error_span!(target: "iroh_gossip::net", "conn", peer = %"cd34e", stream = 2_u64);
                connection.in_scope(|| {
                    tracing::debug!(
                        target: "iroh_gossip::net",
                        origin = ?Origin::Accept,
                        "connection established"
                    );
                });
            });
        });
        tracing::debug!(target: "iroh_gossip::net", "outside every span");
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));

        assert_eq!(
            fs::read_to_string(&trace.files().raw).unwrap(),
            format!(
                "{}\
                 {},\"target\":\"iroh_gossip::net\",\"level\":\"DEBUG\",\"message\":\"connection established\",\"fields\":{{\"origin\":\"Accept\"}},\"spans\":[{{\"fields\":{{\"me\":\"ab12c\"}},\"name\":\"gossip\"}},{{\"fields\":{{\"peer\":\"cd34e\",\"stream\":\"2\"}},\"name\":\"conn\"}}]}}\n\
                 {},\"target\":\"iroh_gossip::net\",\"level\":\"DEBUG\",\"message\":\"outside every span\",\"fields\":{{}}}}\n",
                raw_file_opened_line(),
                header(1, None, SPEC_IROH_LINE),
                header(2, None, SPEC_IROH_LINE),
            )
        );
        drop(fs::remove_dir_all(&root));
    }
}
