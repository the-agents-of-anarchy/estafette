use super::*;

/// The standard error lines a traced command holds once a panic it survives went through the trace's panic hook, the announcement left out.
fn standard_error_after_a_survived_panic(trace_effects: &TraceEffects) -> Vec<String> {
    const SCRATCH_PREFIX: &str = "estafette-panic-hook-flush";
    let (_scratch_removed_when_the_test_ends, environment) = scratch_environment(SCRATCH_PREFIX);
    let standard_error = SharedRecord::default();
    let mut after_the_panic = Vec::new();
    let hook_before = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_panic| {}));

    run_traced(
        TRACED_COMMAND,
        Tracing::Debug,
        environment,
        None,
        standard_error.clone(),
        TraceSetup {
            effects: trace_effects,
            bounds: TraceBounds {
                drain: STOP_REPORTED_WITHIN,
                ..TraceBounds::SPEC
            },
            install_iroh_lines: IROH_LINES_LEFT_OUT,
            record_panics: record_panic_locations,
        },
        |_directories, _process| {
            let panicked = std::thread::spawn(|| panic!("a panic the process survives")).join();
            assert!(panicked.is_err());
            after_the_panic = lines_after_the_announcement(&standard_error);
            Ok(ExitStatus::Success)
        },
    );

    std::panic::set_hook(hook_before);
    after_the_panic
}

#[test]
fn observability_decision_12_a_panic_whose_flush_cannot_cut_either_trace_file_back_names_each_on_standard_error_events_first()
 {
    let _hook_held = PANIC_HOOK_HELD
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let trace_effects = trace_effects_creating(uncuttable_trace_files(), uncuttable_trace_files());

    let after_the_panic = standard_error_after_a_survived_panic(&trace_effects);

    assert_eq!(
        after_the_panic,
        [
            spec_not_cut_line(SPEC_EVENTS_FILE_WORD),
            spec_not_cut_line(SPEC_RAW_FILE_WORD)
        ]
    );
}

#[test]
fn observability_decision_3_a_panic_whose_flush_drains_both_trace_files_adds_nothing_to_standard_error()
 {
    let _hook_held = PANIC_HOOK_HELD
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let trace_effects = TraceEffects::of_the_operating_system(trace_clocks_started_now());

    let after_the_panic = standard_error_after_a_survived_panic(&trace_effects);

    assert_eq!(after_the_panic, Vec::<String>::new());
}
