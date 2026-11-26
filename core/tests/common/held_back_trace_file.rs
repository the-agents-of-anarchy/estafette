use std::io::{self, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{TraceBounds, TraceEffects, TraceFile};

pub const SMALL_QUEUE_LINES: usize = 2;
const WRITER_STEP_BOUND: Duration = Duration::from_secs(10);
const TEST_DRAIN_BOUND: Duration = Duration::from_secs(10);

/// A trace file the test holds back: each write announces itself, waits for the test's release, then hands the line over.
struct HeldBackFile {
    entered: Sender<()>,
    released: Receiver<()>,
    written: Sender<String>,
}

impl Write for HeldBackFile {
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

impl TraceFile for HeldBackFile {
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

/// The test's ends of a held-back trace file: where a write announces itself, where the test releases it, and where its line arrives.
pub struct HeldBackEnds {
    pub entering: Receiver<()>,
    pub release: Sender<()>,
    pub written: Receiver<String>,
}

/// The effects of `base` with the first file the predicate names held back and every other file discarded, and the test's ends of the held-back file.
pub fn effects_holding_back(
    base: TraceEffects,
    is_held_back: impl Fn(&Path) -> bool + Send + Sync + 'static,
) -> (TraceEffects, HeldBackEnds) {
    let (entered, entering) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (written_sender, written) = mpsc::channel();
    let held_back_file = Mutex::new(Some(HeldBackFile {
        entered,
        released,
        written: written_sender,
    }));
    let effects = TraceEffects {
        create_file: Arc::new(move |path: &Path| -> io::Result<Box<dyn TraceFile>> {
            let held_back = is_held_back(path)
                .then(|| held_back_file.lock().unwrap().take())
                .flatten();
            if let Some(file) = held_back {
                return Ok(Box::new(file));
            }
            Ok(Box::new(DiscardedFile))
        }),
        ..base
    };
    (
        effects,
        HeldBackEnds {
            entering,
            release,
            written,
        },
    )
}

/// The bounds of a trace whose queues are `SMALL_QUEUE_LINES` long and whose drain is bounded for a test.
pub fn small_queue_bounds() -> TraceBounds {
    TraceBounds {
        queue_lines: SMALL_QUEUE_LINES,
        drain: TEST_DRAIN_BOUND,
        ..TraceBounds::SPEC
    }
}

/// What the receiver gets within `WRITER_STEP_BOUND`, failing with the condition's name when it gets nothing.
pub fn within_bound<T>(receiver: &Receiver<T>, condition: &str) -> T {
    receiver
        .recv_timeout(WRITER_STEP_BOUND)
        .unwrap_or_else(|failure| panic!("{condition} within {WRITER_STEP_BOUND:?}: {failure}"))
}
