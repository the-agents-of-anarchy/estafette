use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch by this machine's clock.
pub fn wall_clock_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}
