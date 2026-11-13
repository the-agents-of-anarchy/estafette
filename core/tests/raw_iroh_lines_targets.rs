#[cfg(test)]
#[path = "support/raw_iroh_lines_trace.rs"]
mod raw_iroh_lines_trace;

/// The targets and levels of `spec/observability.md` decision 7, through the install the composition root calls, in a process of its own.
#[cfg(test)]
mod raw_iroh_lines_targets {
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;

    use estafette::trace::iroh_lines::install;
    use estafette::trace::{TraceBounds, TraceEffects, TraceStop};
    use serde_json::Value;

    use super::raw_iroh_lines_trace::{frozen_clocks, scratch_trace_directory, started};

    const SPEC_LEVELS_COARSEST_FIRST: [&str; 5] = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"];
    const SPEC_TARGETS_AND_FINEST_LEVELS: [(&str, &str); 4] = [
        ("iroh::_events", "DEBUG"),
        ("iroh::socket::remote_map", "TRACE"),
        ("iroh_gossip::proto::hyparview", "DEBUG"),
        ("iroh_gossip::net", "DEBUG"),
    ];

    /// Writes one line at each of the five levels to each target given, saying which targets it wrote to.
    macro_rules! one_line_at_every_level_of_each {
        ($($target:literal),+ $(,)?) => {
            [$({
                tracing::error!(target: $target, "a line");
                tracing::warn!(target: $target, "a line");
                tracing::info!(target: $target, "a line");
                tracing::debug!(target: $target, "a line");
                tracing::trace!(target: $target, "a line");
                $target
            }),+]
        };
    }

    /// The position of the finest level the spec lets reach the raw file from the target, by the most specific spec target it falls under, or none when it falls under none.
    fn finest_spec_level(target: &str) -> Option<usize> {
        SPEC_TARGETS_AND_FINEST_LEVELS
            .iter()
            .filter(|(spec_target, _)| {
                target == *spec_target || target.starts_with(&format!("{spec_target}::"))
            })
            .max_by_key(|(spec_target, _)| spec_target.len())
            .and_then(|(_, finest)| {
                SPEC_LEVELS_COARSEST_FIRST
                    .iter()
                    .position(|level| level == finest)
            })
    }

    /// The target and the level of each line of the raw file after its first line.
    fn targets_and_levels_after_the_first_line(raw: &Path) -> Vec<(String, String)> {
        fs::read_to_string(raw)
            .unwrap()
            .lines()
            .skip(1)
            .map(|line| {
                let line: Value = serde_json::from_str(line).unwrap();
                (
                    line["target"].as_str().unwrap().to_owned(),
                    line["level"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn observability_decision_7_only_the_four_targets_reach_the_raw_file_each_up_to_its_own_level()
    {
        let (root, directory) = scratch_trace_directory("targets");
        let trace = started(
            &directory,
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds::SPEC,
        );

        install(Arc::clone(&trace)).unwrap();
        let emitted = one_line_at_every_level_of_each!(
            "iroh::_events",
            "iroh::_events::path::selected",
            "iroh::socket::remote_map",
            "iroh::socket::remote_map::remote_state",
            "iroh_gossip::proto::hyparview",
            "iroh_gossip::net",
            "iroh_gossip::net::util",
            "iroh::endpoint",
            "iroh::socket",
            "iroh_gossip::proto::plumtree",
            "iroh_gossip::proto::state",
            "iroh_relay::client",
            "estafette::node",
        );
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));

        let expected: Vec<(String, String)> = emitted
            .iter()
            .flat_map(|target| {
                SPEC_LEVELS_COARSEST_FIRST
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| {
                        finest_spec_level(target).is_some_and(|finest| *position <= finest)
                    })
                    .map(|(_, level)| ((*target).to_owned(), (*level).to_owned()))
            })
            .collect();
        assert_eq!(
            targets_and_levels_after_the_first_line(&trace.files().raw),
            expected
        );
        drop(fs::remove_dir_all(&root));
    }
}
