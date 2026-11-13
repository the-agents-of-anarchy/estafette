use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use estafette_trace::{
    BridgeRun, FileTrace, RUN_ID_BYTES, RunId, TraceBounds, TraceDirectory, TraceEffects, TraceStop,
};

use crate::CommandError;
use crate::event::BridgeEvent;

/// Whether `run` writes the debug trace of its run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tracing {
    Debug,
    NoDebug,
}

/// What a run's debug trace starts from, built at the composition root and read under `--debug` alone.
pub struct TraceInputs<'inputs> {
    /// Reads an environment variable, for the data directory the trace directory sits under.
    pub environment: &'inputs dyn Fn(&str) -> Option<OsString>,
    /// Draws the run id's bytes from the operating system's secure random source.
    pub draw_run_id: &'inputs dyn Fn() -> io::Result<[u8; RUN_ID_BYTES]>,
    /// Creates a directory readable by its owner alone, with its missing parents.
    pub create_private_directory: &'inputs dyn Fn(&Path) -> io::Result<()>,
    /// The files, threads and clocks the trace writes with.
    pub effects: TraceEffects,
    /// The bounds the trace runs under.
    pub bounds: TraceBounds,
}

/// The debug trace of one bridge run: its events file, the only trace file a bridge writes, and the writer of it.
pub struct DebugTrace {
    events_file: PathBuf,
    events: FileTrace,
    drain: Duration,
}

impl DebugTrace {
    /// The debug trace under `--debug`, its run id drawn from the secure random source; none under `--no-debug`.
    pub fn start(tracing: Tracing, inputs: &TraceInputs<'_>) -> Result<Option<Self>, CommandError> {
        match tracing {
            Tracing::NoDebug => Ok(None),
            Tracing::Debug => {
                let directory = TraceDirectory::resolve(&inputs.environment)
                    .map_err(CommandError::DataDirectoryUnresolvable)?;
                let run_id = (inputs.draw_run_id)().map_err(CommandError::RunIdUnreadable)?;
                let run = BridgeRun::begin(
                    &directory,
                    inputs.create_private_directory,
                    RunId::from_random_bytes(run_id),
                    &inputs.effects,
                    inputs.bounds,
                )
                .map_err(CommandError::TraceRefused)?;
                let events = run.open_events().map_err(CommandError::TraceRefused)?;
                Ok(Some(Self {
                    events_file: run.events_file().to_owned(),
                    events,
                    drain: inputs.bounds.drain,
                }))
            }
        }
    }

    /// The line standard error opens with: the events file and that it is safe to post.
    pub fn announcement(&self) -> String {
        format!(
            "writing the debug trace: {} (safe to post)",
            self.events_file.display()
        )
    }

    /// Queues the event's line on the events file, never waiting for the writer.
    pub fn record(&self, event: &BridgeEvent) {
        self.events.write(event.name().as_str(), &event.fields());
    }

    /// Closes the queue and waits within the drain bound for the writer, giving the event of a stop that left the file unfinished.
    pub fn finish(&self) -> Option<BridgeEvent> {
        match self.events.finish() {
            TraceStop::Drained => None,
            TraceStop::NotDrained { file: _ } => {
                Some(BridgeEvent::TraceNotDrained { drain: self.drain })
            }
            TraceStop::NotCut { file: _, source } => Some(BridgeEvent::TraceNotCut(source)),
        }
    }
}
