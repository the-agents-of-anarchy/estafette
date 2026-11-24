use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

use estafette_trace::{CreateTraceFile, TraceFile};

/// A trace file whose write never returns while the test holds the sender that would unstick it.
struct StuckFile {
    until_unstuck: Receiver<()>,
}

impl Write for StuckFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.until_unstuck.recv().map_err(io::Error::other)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TraceFile for StuckFile {
    fn set_length(&mut self, _length: u64) -> io::Result<()> {
        Ok(())
    }
}

/// Creates a stuck file for every path, handing the sender that would unstick each file to the test, which keeps every writer stuck by holding them.
pub fn stuck_files(to_the_test: Sender<Sender<()>>) -> CreateTraceFile {
    Arc::new(move |_path: &Path| -> io::Result<Box<dyn TraceFile>> {
        let (unstick, until_unstuck) = mpsc::channel();
        to_the_test.send(unstick).map_err(io::Error::other)?;
        Ok(Box::new(StuckFile { until_unstuck }))
    })
}
