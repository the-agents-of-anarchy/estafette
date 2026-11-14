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
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::thread;
    use std::time::Duration;

    use super::remove_scratch::remove_scratch;
    use super::scratch_run::{ONE_EVENT, frozen_clocks, one_event_line, scratch_run};
    use estafette_trace::{TraceBounds, TraceClocks, TraceEffects, TraceFileKind, TraceStop};
    use serde_json::Value;

    const PANICKED_EVENT: &str = "process.panicked";
    const EVENT_FIELD: &str = "event";
    const NUMBER_FIELD: &str = "n";
    const STAND_IN_CLOCK_PANIC: &str = "the stand-in clock panics while a line is stamped";
    const PANIC_ENDS_WITHIN: Duration = Duration::from_secs(10);
    static PANIC_HOOK_HELD: Mutex<()> = Mutex::new(());

    /// Frozen clocks whose monotonic reading panics once, on the first reading after the test armed it, so a line's stamp panics under the queue's lock.
    fn clocks_panicking_once_armed(armed: Arc<AtomicBool>) -> TraceClocks {
        let frozen = frozen_clocks();
        let frozen_monotonic_ms = Arc::clone(&frozen.monotonic_ms);
        TraceClocks {
            monotonic_ms: Arc::new(move || {
                assert!(
                    !armed.swap(false, Ordering::SeqCst),
                    "{STAND_IN_CLOCK_PANIC}"
                );
                frozen_monotonic_ms()
            }),
            ..frozen
        }
    }

    /// Frozen clocks whose monotonic reading, the first after the test armed it, tells the test and waits for its release, so the stamping thread holds the queue's lock until then.
    fn clocks_holding_the_stamp_once_armed(
        armed: Arc<AtomicBool>,
        holding: Sender<()>,
        released: Receiver<()>,
    ) -> TraceClocks {
        let frozen = frozen_clocks();
        let frozen_monotonic_ms = Arc::clone(&frozen.monotonic_ms);
        let released = Mutex::new(released);
        TraceClocks {
            monotonic_ms: Arc::new(move || {
                if armed.swap(false, Ordering::SeqCst) {
                    holding.send(()).unwrap();
                    released
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .recv()
                        .unwrap();
                }
                frozen_monotonic_ms()
            }),
            ..frozen
        }
    }

    /// The event name and number of each line of a trace file, in file order.
    fn names_and_numbers(file: &Path) -> Vec<(String, u64)> {
        fs::read_to_string(file)
            .unwrap()
            .lines()
            .map(|line| {
                let line: Value = serde_json::from_str(line).unwrap();
                (
                    line[EVENT_FIELD].as_str().unwrap().to_owned(),
                    line[NUMBER_FIELD].as_u64().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn observability_decision_9_a_panic_raised_while_a_line_is_stamped_ends_through_a_hook_that_records_and_flushes_never_waiting_on_itself_nor_panicking_again()
     {
        let _hook_held = PANIC_HOOK_HELD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let clock_armed = Arc::new(AtomicBool::new(false));
        let scratch = scratch_run(
            "panic-while-stamping",
            &TraceEffects::of_the_operating_system(clocks_panicking_once_armed(Arc::clone(
                &clock_armed,
            ))),
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

    #[test]
    fn observability_decision_9_a_panic_raised_while_a_line_is_stamped_ends_through_a_hook_that_records_in_another_file_before_it_records_and_flushes_the_first()
     {
        let _hook_held = PANIC_HOOK_HELD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let clock_armed = Arc::new(AtomicBool::new(false));
        let scratch = scratch_run(
            "panic-while-stamping-hook-records-raw-first",
            &TraceEffects::of_the_operating_system(clocks_panicking_once_armed(Arc::clone(
                &clock_armed,
            ))),
            TraceBounds::SPEC,
        );
        let events = Arc::new(scratch.run.open_events().unwrap());
        let raw = Arc::new(scratch.run.open_raw().unwrap());
        let (stops_in_the_hook, hook_stops) = mpsc::channel();
        let hook_events = Arc::clone(&events);
        let test_gave_up = Arc::new(AtomicBool::new(false));
        let hook_sees_the_test_gave_up = Arc::clone(&test_gave_up);
        let hook_before = panic::take_hook();
        panic::set_hook(Box::new(move |_panic| {
            if hook_sees_the_test_gave_up.load(Ordering::SeqCst) {
                return;
            }
            raw.write(PANICKED_EVENT, &[]);
            let raw_stop = raw.flush();
            hook_events.write(PANICKED_EVENT, &[]);
            stops_in_the_hook
                .send((raw_stop, hook_events.flush()))
                .unwrap();
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
        let (raw_stop, events_stop) = hook_stops.try_recv().unwrap();
        assert!(panicked);
        assert!(matches!(raw_stop, TraceStop::Drained), "{raw_stop:?}");
        assert!(
            matches!(
                events_stop,
                TraceStop::NotDrained {
                    file: TraceFileKind::Events
                }
            ),
            "{events_stop:?}"
        );
        assert!(matches!(events.finish(), TraceStop::Drained));
        remove_scratch(&scratch.root).unwrap();
    }

    #[test]
    fn observability_decision_9_a_hook_on_the_thread_stamping_one_file_waits_for_another_file_whose_lock_another_thread_holds_and_records_there()
     {
        let _hook_held = PANIC_HOOK_HELD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let stamp_panics = Arc::new(AtomicBool::new(false));
        let stamp_held = Arc::new(AtomicBool::new(false));
        let (holding, other_thread_holds_the_lock) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let panicking = scratch_run(
            "marker-names-its-own-trace-panicking",
            &TraceEffects::of_the_operating_system(clocks_panicking_once_armed(Arc::clone(
                &stamp_panics,
            ))),
            TraceBounds::SPEC,
        );
        let held = scratch_run(
            "marker-names-its-own-trace-held",
            &TraceEffects::of_the_operating_system(clocks_holding_the_stamp_once_armed(
                Arc::clone(&stamp_held),
                holding,
                released,
            )),
            TraceBounds::SPEC,
        );
        let panicking_events = Arc::new(panicking.run.open_events().unwrap());
        let held_events = Arc::new(held.run.open_events().unwrap());
        let (about_to_record, hook_records) = mpsc::channel();
        let (stop_in_the_hook, hook_stops) = mpsc::channel();
        let hook_events = Arc::clone(&held_events);
        let test_gave_up = Arc::new(AtomicBool::new(false));
        let hook_sees_the_test_gave_up = Arc::clone(&test_gave_up);
        let hook_before = panic::take_hook();
        panic::set_hook(Box::new(move |_panic| {
            if hook_sees_the_test_gave_up.load(Ordering::SeqCst) {
                return;
            }
            about_to_record.send(()).unwrap();
            hook_events.write(PANICKED_EVENT, &[]);
            stop_in_the_hook.send(hook_events.flush()).unwrap();
        }));
        let holder_events = Arc::clone(&held_events);
        stamp_held.store(true, Ordering::SeqCst);
        let holder = thread::spawn(move || holder_events.write(ONE_EVENT, &[]));
        other_thread_holds_the_lock
            .recv_timeout(PANIC_ENDS_WITHIN)
            .expect("the other thread holds the held file's lock within its bound");
        let (ended, panicking_thread_ended) = mpsc::channel();
        let recorder = Arc::clone(&panicking_events);
        stamp_panics.store(true, Ordering::SeqCst);

        thread::spawn(move || {
            let stamped = panic::catch_unwind(AssertUnwindSafe(|| {
                recorder.write(ONE_EVENT, &[]);
            }));
            ended.send(stamped.is_err()).unwrap();
        });
        let hook_reached_the_held_file = hook_records.recv_timeout(PANIC_ENDS_WITHIN);
        release.send(()).unwrap();

        let ended_within_the_bound = panicking_thread_ended.recv_timeout(PANIC_ENDS_WITHIN);
        test_gave_up.store(ended_within_the_bound.is_err(), Ordering::SeqCst);
        let panicked = ended_within_the_bound
            .expect("the thread that panicked while stamping a line ended within its bound");
        panic::set_hook(hook_before);
        holder.join().unwrap();
        hook_reached_the_held_file.expect("the hook reached the held file within its bound");
        let stop = hook_stops.try_recv().unwrap();
        assert!(panicked);
        assert!(matches!(stop, TraceStop::Drained), "{stop:?}");
        assert_eq!(
            names_and_numbers(&held.run.files().events),
            [(ONE_EVENT.to_owned(), 0), (PANICKED_EVENT.to_owned(), 1)]
        );
        assert!(matches!(held_events.finish(), TraceStop::Drained));
        assert!(matches!(panicking_events.finish(), TraceStop::Drained));
        remove_scratch(&panicking.root).unwrap();
        remove_scratch(&held.root).unwrap();
    }
}
