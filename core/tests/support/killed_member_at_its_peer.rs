use std::time::{Duration, Instant};

use estafette::net::endpoint::QUIC_KEEP_ALIVE_INTERVAL;
use serde_json::json;

use crate::mcp_server::{McpServer, unreachable};
use crate::member_ended_at_its_peer::UNREACHABLE_CODE;
use crate::message_body::BODY;

/// The QUIC idle timeout of spec 3.14, after which a killed peer's gossip neighbor-down event comes (spec 3.15 test 5).
const SPEC_QUIC_IDLE_TIMEOUT: Duration = Duration::from_millis(10_000);
/// The probe timeout of spec 3.14, the second part of the bound of spec 3.15 test 5.
const SPEC_PROBE_TIMEOUT: Duration = Duration::from_millis(3000);
/// Spec 3.15 test 5: a member lists a killed peer unreachable within one gossip neighbor-down event and one probe timeout.
pub const SPEC_NEIGHBOR_DOWN_AND_PROBE_WITHIN: Duration = SPEC_QUIC_IDLE_TIMEOUT
    .saturating_add(QUIC_KEEP_ALIVE_INTERVAL)
    .saturating_add(SPEC_PROBE_TIMEOUT);

/// Requires bob, killed at that instant, listed unreachable by amy within the bound, and unreachable to amy's send.
#[track_caller]
pub fn assert_killed_member_is_unreachable_then_unsendable(
    amy: &mut McpServer,
    bob_handle: &str,
    killed: Instant,
    within: Duration,
) {
    amy.peer_within(bob_handle, within, unreachable);
    let unreachable_after = killed.elapsed();
    let sent = amy.send(bob_handle, BODY);

    assert!(unreachable_after <= within, "{unreachable_after:?}");
    assert_eq!(sent["error"]["code"], json!(UNREACHABLE_CODE), "{sent}");
}
