#[cfg(test)]
#[path = "common/remove_scratch.rs"]
mod remove_scratch;

#[cfg(test)]
#[path = "support/raw_iroh_lines_trace.rs"]
mod raw_iroh_lines_trace;

/// The queue bound and the `dropped` line of `spec/observability.md` decision 12 under iroh's lines, through the install the composition root calls, in a process of its own.
#[cfg(test)]
mod raw_iroh_lines_full_queue {
    use std::io::{self, Write};
    use std::path::Path;
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use estafette::dirs::TraceDirectory;
    use estafette::trace::iroh_lines::install;
    use estafette::trace::{Trace, TraceBounds, TraceEffects, TraceFile, TraceStop};
    use serde_json::Value;

    use super::raw_iroh_lines_trace::{frozen_clocks, scratch_trace_directory, started};
    use super::remove_scratch::remove_scratch;

    const SPEC_RAW_FILE_OPENED: &str = "raw_file_opened";
    const SPEC_IROH_LINE: &str = "iroh_line";
    const SPEC_DROPPED: &str = "dropped";
    const RAW_FILE_SUFFIX: &str = ".raw.jsonl";
    const SMALL_QUEUE_LINES: usize = 2;
    const WRITER_STEP_BOUND: Duration = Duration::from_secs(10);
    const TEST_DRAIN_BOUND: Duration = Duration::from_secs(10);

    /// Writes one line of a target the raw file takes.
    fn one_iroh_line() {
        tracing::debug!(target: "iroh::_events::direct_addrs", "a line");
    }

    /// A raw file the test holds back: each write announces itself, waits for the test's release, then hands the line over.
    struct HeldBackRawFile {
        entered: Sender<()>,
        released: Receiver<()>,
        written: Sender<String>,
    }

    impl Write for HeldBackRawFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.send(()).map_err(io::Error::other)?;
            self.released.recv().map_err(io::Error::other)?;
            self.written
                .send(String::from_utf8_lossy(bytes).into_owned())
                .map_err(io::Error::other)?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for HeldBackRawFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    /// A trace file that discards every byte written to it.
    struct DiscardedFile;

    impl Write for DiscardedFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for DiscardedFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    /// The test's ends of a held-back raw file: where a write announces itself, where the test releases it, and where its line arrives.
    struct HeldBackRaw {
        entering: Receiver<()>,
        release: Sender<()>,
        written: Receiver<String>,
    }

    /// A trace whose raw file the test holds back and whose events file is discarded, its queues `SMALL_QUEUE_LINES` long.
    fn held_back_trace(directory: &TraceDirectory) -> (Arc<Trace>, HeldBackRaw) {
        let (entered, entering) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (written_sender, written) = mpsc::channel();
        let raw_file = Mutex::new(Some(HeldBackRawFile {
            entered,
            released,
            written: written_sender,
        }));
        let effects = TraceEffects {
            create_file: Arc::new(move |path: &Path| -> io::Result<Box<dyn TraceFile>> {
                let is_raw = path
                    .to_str()
                    .is_some_and(|path| path.ends_with(RAW_FILE_SUFFIX));
                let held_back = is_raw.then(|| raw_file.lock().unwrap().take()).flatten();
                if let Some(file) = held_back {
                    return Ok(Box::new(file));
                }
                Ok(Box::new(DiscardedFile))
            }),
            ..TraceEffects::of_the_operating_system(frozen_clocks())
        };
        let trace = started(
            directory,
            &effects,
            TraceBounds {
                queue_lines: SMALL_QUEUE_LINES,
                drain: TEST_DRAIN_BOUND,
                ..TraceBounds::SPEC
            },
        );
        (
            trace,
            HeldBackRaw {
                entering,
                release,
                written,
            },
        )
    }

    /// What the receiver gets within `WRITER_STEP_BOUND`, failing with the condition's name when it gets nothing.
    fn within_bound<T>(receiver: &Receiver<T>, condition: &str) -> T {
        receiver
            .recv_timeout(WRITER_STEP_BOUND)
            .unwrap_or_else(|failure| panic!("{condition} within {WRITER_STEP_BOUND:?}: {failure}"))
    }

    /// The next line the held-back raw file writes once the test releases it: its number, its event and the count a `dropped` line carries.
    fn released_line(raw: &HeldBackRaw) -> (u64, String, Option<u64>) {
        raw.release.send(()).unwrap();
        let line: Value =
            serde_json::from_str(&within_bound(&raw.written, "a released line is written"))
                .unwrap();
        (
            line["n"].as_u64().unwrap(),
            line["event"].as_str().unwrap().to_owned(),
            line["count"].as_u64(),
        )
    }

    #[test]
    fn observability_decision_12_the_iroh_lines_a_full_raw_queue_loses_are_counted_and_the_next_line_is_a_dropped_line_where_n_skips()
     {
        const LINES_PAST_THE_HELD_FIRST_LINE: usize = SMALL_QUEUE_LINES + 2;
        let (root, directory) = scratch_trace_directory("full-queue");
        let (trace, raw) = held_back_trace(&directory);
        within_bound(&raw.entering, "the writer holds the raw file's first line");

        install(Arc::clone(&trace)).unwrap();
        for _queued_or_lost in 0..LINES_PAST_THE_HELD_FIRST_LINE {
            one_iroh_line();
        }
        let held: Vec<_> = (0..=SMALL_QUEUE_LINES)
            .map(|_line| released_line(&raw))
            .collect();
        one_iroh_line();

        assert_eq!(
            held,
            [
                (0, SPEC_RAW_FILE_OPENED.to_owned(), None),
                (1, SPEC_IROH_LINE.to_owned(), None),
                (2, SPEC_IROH_LINE.to_owned(), None),
            ]
        );
        assert_eq!(released_line(&raw), (5, SPEC_DROPPED.to_owned(), Some(2)));
        assert_eq!(released_line(&raw), (6, SPEC_IROH_LINE.to_owned(), None));
        drop(raw.release);
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));
        remove_scratch(&root).unwrap();
    }
}
