use clap::Args;

use crate::dirs::{
    ROLLED_OVER_TRACE_SUFFIX, TraceDirectory, events_trace_file_name, raw_trace_file_name,
};
use crate::trace::{
    TRACE_DIRECTORY_MAX_BYTES, TRACE_FILE_ROLLOVER_BYTES, TRACE_FRESHNESS,
    TRACE_RUN_FILE_MAX_BYTES, run_name_pattern,
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
    /// Write no debug trace for this run: create no trace file and start no trace writer
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
    let run = run_name_pattern();
    format!(
        "Write the debug trace of this run, the default until v1.0.0.\n\nWrites two files per run, readable by you alone, in {directory}: {events}, the run's typed events, which is the file safe to post; and {raw}, iroh's own lines with real addresses and full ids, which is not safe to post. The first line on standard error names both files.\n\nBounds: each file rolls over once at {rollover_mib} MiB into a {ROLLED_OVER_TRACE_SUFFIX} file that replaces any earlier one, so a run keeps at most {run_file_mib} MiB per file; at each start the oldest runs are removed, both files together, until the directory holds at most {directory_gib} GiB, keeping every run whose events file was written in the last {freshness_minutes} minutes.",
        directory = TraceDirectory::described(),
        events = events_trace_file_name(&run),
        raw = raw_trace_file_name(&run),
        rollover_mib = TRACE_FILE_ROLLOVER_BYTES / BYTES_PER_MEBIBYTE,
        run_file_mib = TRACE_RUN_FILE_MAX_BYTES / BYTES_PER_MEBIBYTE,
        directory_gib = TRACE_DIRECTORY_MAX_BYTES / BYTES_PER_GIBIBYTE,
        freshness_minutes = TRACE_FRESHNESS.as_secs() / SECONDS_PER_MINUTE
    )
}
