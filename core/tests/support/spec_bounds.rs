use std::time::Duration;

/// Spec 3.15 test 18: a send to a restarted member is delivered within this long of its new Presence reaching the sender.
pub const SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE: Duration = Duration::from_millis(1000);
