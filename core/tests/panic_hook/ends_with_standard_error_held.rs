use std::sync::mpsc;

use super::*;

const STAND_IN_WRITE_PANIC: &str = "standard error panics while the run record holds it";
const HOOK_ENDS_WITHIN: Duration = Duration::from_secs(20);

/// A standard error that panics on its first write once armed, so the panic is raised while the run record holds its lock, and records every other write.
struct StandardErrorPanickingWhenArmed {
    record: SharedRecord,
    armed: Arc<AtomicBool>,
}

impl StandardErrorPanickingWhenArmed {
    /// Panics once, on the first call after the test armed it.
    fn panic_once_armed(&self) {
        assert!(
            !self.armed.swap(false, Ordering::SeqCst),
            "{STAND_IN_WRITE_PANIC}"
        );
    }
}

impl Write for StandardErrorPanickingWhenArmed {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.panic_once_armed();
        self.record.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What a traced command left once a run record write panicked under the real panic hook, whose flush could cut neither trace file back.
#[derive(Debug, PartialEq)]
struct AfterThePanic {
    status: ExitStatus,
    recording_thread_panicked: bool,
    lines_once_the_hook_ended: Vec<String>,
    lines_once_the_command_ended: Vec<String>,
}

/// Runs a traced command whose run record write panics on a thread of its own under the real panic hook, with trace files that refuse to be cut back.
fn after_a_panic_raised_while_standard_error_is_held() -> AfterThePanic {
    const SCRATCH_PREFIX: &str = "estafette-panic-hook-standard-error-held";
    let (_scratch_removed_when_the_run_ends, environment) = scratch_environment(SCRATCH_PREFIX);
    let trace_effects = trace_effects_creating(uncuttable_trace_files(), uncuttable_trace_files());
    let standard_error = SharedRecord::default();
    let armed = Arc::new(AtomicBool::new(false));
    let mut recording_thread_panicked = false;
    let mut lines_once_the_hook_ended = Vec::new();
    let hook_before = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_panic| {}));

    let status = run_traced(
        TRACED_COMMAND,
        Tracing::Debug,
        environment,
        None,
        StandardErrorPanickingWhenArmed {
            record: standard_error.clone(),
            armed: Arc::clone(&armed),
        },
        TraceSetup {
            effects: &trace_effects,
            bounds: TraceBounds {
                drain: STOP_REPORTED_WITHIN,
                ..TraceBounds::SPEC
            },
            install_iroh_lines: IROH_LINES_LEFT_OUT,
            record_panics: record_panic_locations,
        },
        |_directories, process| {
            let record = Arc::clone(&process.record);
            armed.store(true, Ordering::SeqCst);
            recording_thread_panicked =
                std::thread::spawn(move || record(ProcessEvent::ClockBeforeUnixEpoch.into()))
                    .join()
                    .is_err();
            lines_once_the_hook_ended = lines_after_the_announcement(&standard_error);
            Ok(ExitStatus::Success)
        },
    );

    std::panic::set_hook(hook_before);
    AfterThePanic {
        status,
        recording_thread_panicked,
        lines_once_the_hook_ended,
        lines_once_the_command_ended: lines_after_the_announcement(&standard_error),
    }
}

#[test]
fn observability_decision_12_a_panic_raised_while_the_run_record_holds_standard_error_ends_though_the_flush_leaves_both_trace_files_unfinished()
-> Result<(), String> {
    let _hook_held = PANIC_HOOK_HELD
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let (ended, run_ended) = mpsc::channel();

    std::thread::spawn(move || {
        ended
            .send(after_a_panic_raised_while_standard_error_is_held())
            .unwrap();
    });

    let after_the_panic = run_ended.recv_timeout(HOOK_ENDS_WITHIN).map_err(|not_ended| {
        format!("the panic hook did not end within {HOOK_ENDS_WITHIN:?} of a panic raised while the run record held standard error: {not_ended}")
    })?;
    let expected = AfterThePanic {
        status: ExitStatus::Success,
        recording_thread_panicked: true,
        lines_once_the_hook_ended: Vec::new(),
        lines_once_the_command_ended: vec![
            spec_not_cut_line(SPEC_EVENTS_FILE_WORD),
            spec_not_cut_line(SPEC_RAW_FILE_WORD),
        ],
    };
    if after_the_panic == expected {
        Ok(())
    } else {
        Err(format!("{after_the_panic:?} where {expected:?}"))
    }
}
