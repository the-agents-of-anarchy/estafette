use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use estafette_trace::{
    EstafetteRun, RunId, TraceBounds, TraceClocks, TraceDirectory, TraceEffects,
};

use crate::fresh_directory::fresh_directory_in;
use crate::frozen_run::{
    DATA_HOME_VARIABLE, FROZEN_MONOTONIC_MS, NOW_MS, RUN_BYTES, frozen_clocks_at,
};

const RUN_HEX: &str = "a1b2c3d4e5f60718";
/// The one event these tests record.
pub const ONE_EVENT: &str = "process.clock_before_unix_epoch";

/// Clocks that always read the same two values.
pub fn frozen_clocks() -> TraceClocks {
    frozen_clocks_at(NOW_MS)
}

/// The line a run on the frozen clocks writes for `ONE_EVENT` as its line `n`.
pub fn one_event_line(n: u64) -> String {
    format!(
        "{{\"v\":2,\"run\":\"{RUN_HEX}\",\"n\":{n},\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{NOW_MS},\"event\":\"{ONE_EVENT}\"}}\n"
    )
}

/// A run begun under a fresh scratch root, with the root the test removes when it ends.
pub struct ScratchRun {
    pub root: PathBuf,
    pub run: EstafetteRun,
}

/// A run begun on the effects within the bounds, its trace directory created under a fresh scratch root named for the test.
pub fn scratch_run(test_name: &str, effects: &TraceEffects, bounds: TraceBounds) -> ScratchRun {
    let root = fresh_directory_in(
        &std::env::temp_dir(),
        &format!("estafette-trace-{test_name}-{}", std::process::id()),
    )
    .unwrap();
    let data_home = root.join("data");
    let directory = TraceDirectory::resolve(&move |name: &str| {
        (name == DATA_HOME_VARIABLE).then(|| OsString::from(&data_home))
    })
    .unwrap();
    let run = EstafetteRun::begin(
        &directory,
        &|path: &Path| fs::create_dir_all(path),
        RunId::from_random_bytes(RUN_BYTES),
        effects,
        bounds,
    )
    .unwrap();
    ScratchRun { root, run }
}
