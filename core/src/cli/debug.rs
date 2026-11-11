use clap::Args;

use crate::trace::{
    TRACE_DIRECTORY_MAX_BYTES, TRACE_FILE_ROLLOVER_BYTES, TRACE_FRESHNESS, TRACE_RUN_FILE_MAX_BYTES,
};

const BYTES_PER_MEBIBYTE: u64 = 1_048_576;
const BYTES_PER_GIBIBYTE: u64 = 1_073_741_824;
const SECONDS_PER_MINUTE: u64 = 60;

/// Whether a command writes the debug trace of its run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracing {
    Debug,
    NoDebug,
}

/// The `--debug` and `--no-debug` pair of every command that starts an endpoint: the last one given wins, and `--debug` is the default.
#[derive(Debug, Clone, Copy, Default, Args)]
pub struct DebugArgs {
    /// Write the debug trace of this run, the default until v1.0.0
    #[arg(long, overrides_with = "no_debug", long_help = debug_long_help())]
    debug: bool,
    /// Write no trace file and install nothing for one
    #[arg(long = "no-debug", overrides_with = "debug")]
    no_debug: bool,
}

impl DebugArgs {
    /// Whether the command writes the debug trace: it does unless `--no-debug` came last.
    #[must_use]
    pub const fn tracing(&self) -> Tracing {
        if self.no_debug {
            Tracing::NoDebug
        } else {
            Tracing::Debug
        }
    }
}

/// The long help of `--debug`: what the trace writes, where, which file is safe to post, and its four bounds.
fn debug_long_help() -> String {
    format!(
        "Write the debug trace of this run, the default until v1.0.0.\n\nWrites two files per run, readable by you alone, in traces/ under the data directory ($XDG_DATA_HOME/estafette, or ~/.local/share/estafette): <start>-<run>-estafette.events.jsonl, the run's typed events, which is the file safe to post; and <start>-<run>-estafette.raw.jsonl, iroh's own lines with real addresses and full ids, which is not safe to post. The first line on standard error names both files.\n\nBounds: each file rolls over once at {} MiB into a .1 file that replaces any earlier one, so a run keeps at most {} MiB per file; at each start the oldest runs are removed, both files together, until the directory holds at most {} GiB, keeping every run whose events file was written in the last {} minutes.",
        TRACE_FILE_ROLLOVER_BYTES / BYTES_PER_MEBIBYTE,
        TRACE_RUN_FILE_MAX_BYTES / BYTES_PER_MEBIBYTE,
        TRACE_DIRECTORY_MAX_BYTES / BYTES_PER_GIBIBYTE,
        TRACE_FRESHNESS.as_secs() / SECONDS_PER_MINUTE
    )
}
