#[cfg(test)]
#[path = "../../tests/common/fresh_directory.rs"]
mod fresh_directory;
#[cfg(test)]
#[path = "../../tests/common/remove_scratch.rs"]
mod remove_scratch;
#[cfg(test)]
#[path = "support/scratch_run.rs"]
mod scratch_run;

/// A panic raised inside a trace file's record path, its queue's lock held, ends through a panic hook that records and flushes, by spec/observability.md decisions 9 and 12, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::fs;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::remove_scratch::remove_scratch;
    use super::scratch_run::{ONE_EVENT, frozen_clocks, one_event_line, scratch_run};
    use estafette_trace::{TraceBounds, TraceClocks, TraceEffects, TraceFileKind, TraceStop};

    const PANICKED_EVENT: &str = "process.panicked";
    const STAND_IN_CLOCK_PANIC: &str = "the stand-in clock panics while a line is stamped";
    const PANIC_ENDS_WITHIN: Duration = Duration::from_secs(10);

    #[test]
    fn observability_decision_9_a_panic_raised_while_a_line_is_stamped_ends_through_a_hook_that_records_and_flushes_never_waiting_on_itself_nor_panicking_again()
     {
        let clock_armed = Arc::new(AtomicBool::new(false));
        let armed = Arc::clone(&clock_armed);
        let frozen = frozen_clocks();
        let frozen_monotonic_ms = Arc::clone(&frozen.monotonic_ms);
        let scratch = scratch_run(
            "panic-while-stamping",
            &TraceEffects::of_the_operating_system(TraceClocks {
                monotonic_ms: Arc::new(move || {
                    assert!(
                        !armed.swap(false, Ordering::SeqCst),
                        "{STAND_IN_CLOCK_PANIC}"
                    );
                    frozen_monotonic_ms()
                }),
                ..frozen
            }),
            TraceBounds::SPEC,
        );
        let events = Arc::new(scratch.run.open_events().unwrap());
        let (stop_in_the_hook, hook_stops) = mpsc::channel();
        let hook_events = Arc::clone(&events);
        let test_gave_up = Arc::new(AtomicBool::new(false));
        let hook_sees_the_test_gave_up = Arc::clone(&test_gave_up);
        let hook_before = panic::take_hook();
        panic::set_hook(Box::new(move |_panic| {
            if hook_sees_the_test_gave_up.load(Ordering::SeqCst) {
                return;
            }
            hook_events.write(PANICKED_EVENT, &[]);
            stop_in_the_hook.send(hook_events.flush()).unwrap();
        }));
        let (ended, panicking_thread_ended) = mpsc::channel();
        let recorder = Arc::clone(&events);
        clock_armed.store(true, Ordering::SeqCst);

        thread::spawn(move || {
            let stamped = panic::catch_unwind(AssertUnwindSafe(|| {
                recorder.write(ONE_EVENT, &[]);
            }));
            ended.send(stamped.is_err()).unwrap();
        });

        let ended_within_the_bound = panicking_thread_ended.recv_timeout(PANIC_ENDS_WITHIN);
        test_gave_up.store(ended_within_the_bound.is_err(), Ordering::SeqCst);
        let panicked = ended_within_the_bound
            .expect("the thread that panicked while stamping a line ended within its bound");
        panic::set_hook(hook_before);
        let stop = hook_stops.try_recv().unwrap();
        assert!(panicked);
        assert!(
            matches!(
                stop,
                TraceStop::NotDrained {
                    file: TraceFileKind::Events
                }
            ),
            "{stop:?}"
        );
        events.write(ONE_EVENT, &[]);
        assert!(matches!(events.finish(), TraceStop::Drained));
        assert_eq!(
            fs::read_to_string(&scratch.run.files().events).unwrap(),
            one_event_line(0)
        );
        remove_scratch(&scratch.root).unwrap();
    }
}
