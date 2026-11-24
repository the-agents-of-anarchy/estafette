use std::time::{Duration, Instant};

use serde_json::json;

use crate::mcp_server::{McpServer, gone};
use crate::message_body::BODY;

/// Spec 3.15 test 4: a member lists a peer that stopped cleanly gone within this long.
const SPEC_GONE_WITHIN: Duration = Duration::from_millis(1000);
/// The code of a send to a member that is no longer there (spec 3.10).
pub const UNREACHABLE_CODE: &str = "unreachable";

/// Requires bob, whose standard input closed at that instant, listed gone by amy within the spec bound, exited cleanly, and unreachable to amy's send.
#[track_caller]
pub fn assert_stopped_member_is_gone_then_unreachable(
    amy: &mut McpServer,
    bob: &mut McpServer,
    bob_handle: &str,
    input_closed: Instant,
) {
    amy.peer_within(bob_handle, SPEC_GONE_WITHIN, gone);
    let gone_after = input_closed.elapsed();
    let exit = bob.wait_for_exit();
    let sent = amy.send(bob_handle, BODY);

    assert!(gone_after <= SPEC_GONE_WITHIN, "{gone_after:?}");
    assert!(exit.success(), "{exit:?}");
    assert_eq!(sent["error"]["code"], json!(UNREACHABLE_CODE), "{sent}");
}
