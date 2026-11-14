/// A panic raised inside a trace file's record path, its queue's lock held, ends through a panic hook that records and flushes, by spec/observability.md decisions 9 and 12, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::ffi::OsString;
    use std::io::{self, Write};
    use std::panic::{self, AssertUnwindSafe};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use estafette_trace::{
        EstafetteRun, ListFiles, RUN_ID_BYTES, RunId, TraceBounds, TraceClocks, TraceDirectory,
        TraceEffects, TraceFile, TraceFileKind, TraceStop,
    };

    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const DATA_HOME_NEVER_CREATED: &str = "/estafette-trace-test-never-created";
    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
    const FROZEN_MONOTONIC_MS: u64 = 7;
    const NOW_MS: u64 = 2_000_000_000_000;
    const ONE_EVENT: &str = "process.clock_before_unix_epoch";
    const PANICKED_EVENT: &str = "process.panicked";
    const STAND_IN_CLOCK_PANIC: &str = "the stand-in clock panics while a line is stamped";
    const PANIC_ENDS_WITHIN: Duration = Duration::from_secs(10);

    /// A trace file that takes every write and every length.
    struct AcceptingFile;

    impl Write for AcceptingFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for AcceptingFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn observability_decision_9_a_panic_raised_while_a_line_is_stamped_ends_through_a_hook_that_records_and_flushes_never_waiting_on_itself_nor_panicking_again()
     {
        let clock_armed = Arc::new(AtomicBool::new(false));
        let armed = Arc::clone(&clock_armed);
        let list_nothing: ListFiles = Arc::new(|_directory| Ok(Vec::new()));
        let effects = TraceEffects {
            create_file: Arc::new(|_path: &Path| -> io::Result<Box<dyn TraceFile>> {
                Ok(Box::new(AcceptingFile))
            }),
            list_files: list_nothing,
            ..TraceEffects::of_the_operating_system(TraceClocks {
                monotonic_ms: Arc::new(move || {
                    assert!(
                        !armed.swap(false, Ordering::SeqCst),
                        "{STAND_IN_CLOCK_PANIC}"
                    );
                    FROZEN_MONOTONIC_MS
                }),
                wall_ms: Arc::new(|| NOW_MS),
            })
        };
        let directory = TraceDirectory::resolve(&|name: &str| {
            (name == DATA_HOME_VARIABLE).then(|| OsString::from(DATA_HOME_NEVER_CREATED))
        })
        .unwrap();
        let events = Arc::new(
            EstafetteRun::begin(
                &directory,
                &|_path: &Path| Ok(()),
                RunId::from_random_bytes(RUN_BYTES),
                &effects,
                TraceBounds::SPEC,
            )
            .unwrap()
            .open_events()
            .unwrap(),
        );
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
    }
}
