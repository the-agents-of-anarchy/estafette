use std::sync::Arc;

use estafette_trace::{RUN_ID_BYTES, TraceClocks};

pub const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
pub const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
pub const FROZEN_MONOTONIC_MS: u64 = 7;
pub const NOW_MS: u64 = 2_000_000_000_000;

/// Clocks that always read `FROZEN_MONOTONIC_MS` and `wall_ms`.
pub fn frozen_clocks_at(wall_ms: u64) -> TraceClocks {
    TraceClocks {
        monotonic_ms: Arc::new(|| FROZEN_MONOTONIC_MS),
        wall_ms: Arc::new(move || wall_ms),
    }
}
