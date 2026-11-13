use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use estafette::dirs::{OperatingSystemFilesystem, TraceDirectory};
use estafette::trace::{RUN_ID_BYTES, RunId, Trace, TraceBounds, TraceClocks, TraceEffects};

/// The run id every trace of these tests starts with.
pub const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
/// The monotonic reading the frozen clocks give every line.
pub const FROZEN_MONOTONIC_MS: u64 = 7;
/// The wall-clock reading the frozen clocks give every line.
pub const NOW_MS: u64 = 2_000_000_000_000;
const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
const DATA_HOME: &str = "data";

/// A fresh scratch root for the test and the trace directory under its data home.
pub fn scratch_trace_directory(test_name: &str) -> (PathBuf, TraceDirectory) {
    let root = std::env::temp_dir().join(format!(
        "estafette-raw-iroh-lines-{test_name}-{}",
        std::process::id()
    ));
    drop(fs::remove_dir_all(&root));
    fs::create_dir_all(&root).unwrap();
    let data_home = root.join(DATA_HOME);
    let directory = TraceDirectory::resolve(&move |name: &str| {
        (name == DATA_HOME_VARIABLE).then(|| OsString::from(&data_home))
    })
    .unwrap();
    (root, directory)
}

/// Clocks frozen at `FROZEN_MONOTONIC_MS` and `NOW_MS`.
pub fn frozen_clocks() -> TraceClocks {
    TraceClocks {
        monotonic_ms: Arc::new(|| FROZEN_MONOTONIC_MS),
        wall_ms: Arc::new(|| NOW_MS),
    }
}

/// A trace started on the given effects within the given bounds.
pub fn started(
    directory: &TraceDirectory,
    effects: &TraceEffects,
    bounds: TraceBounds,
) -> Arc<Trace> {
    Arc::new(
        Trace::start(
            directory,
            &OperatingSystemFilesystem,
            RunId::from_random_bytes(RUN_BYTES),
            effects,
            bounds,
        )
        .unwrap(),
    )
}
