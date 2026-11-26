use std::time::{SystemTime, UNIX_EPOCH};

use estafette::crypto::Mac;
use estafette::wire::SignatureBytes;

/// The mac a payload carries before sealing fills it.
pub const UNSET_MAC: Mac = Mac::from_bytes([0; 32]);
/// The signature a payload carries before sealing fills it.
pub const UNSIGNED: SignatureBytes = SignatureBytes([0; 64]);

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
