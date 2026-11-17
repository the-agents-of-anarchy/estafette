use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};

use crate::member_events::MemberEvents;
use crate::trace_reader::TraceLine;

/// The JSON-RPC version MCP speaks.
const JSON_RPC_VERSION: &str = "2.0";
/// The MCP protocol version the client asks for.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// The name the client gives in its handshake.
const CLIENT_NAME: &str = "estafette-acceptance";
/// The version the client gives in its handshake.
const CLIENT_VERSION: &str = "0";
/// How long the server may take to answer a request: chosen, not measured, far above its slowest answer, an init that waits out the join timeout and the observer wait (spec 3.14), so only a hung server fails it.
const RESPONSE_WAIT: Duration = Duration::from_secs(60);
/// The tool a member claims its card with (spec 3.10).
pub const INIT_TOOL: &str = "init";
/// The init tool's argument naming the card to claim (spec 3.10).
const CARD_ARGUMENT: &str = "card";
/// The key of a tool's envelope saying whether the call succeeded (spec 3.10).
const OK_KEY: &str = "ok";
/// The tool a member sends a message with (spec 3.10).
pub const SEND_TOOL: &str = "send";
/// The send tool's argument naming the handle the message goes to (spec 3.10).
const TO_ARGUMENT: &str = "to";
/// The send tool's argument holding the message body (spec 3.10).
const BODY_ARGUMENT: &str = "body";
/// The tool a member lists its peers with (spec 3.10).
pub const PEERS_TOOL: &str = "peers";
/// The key of a tool's envelope holding its result (spec 3.10).
pub const RESULT_KEY: &str = "result";
/// The key of the peers tool's result listing the peers (spec 3.10).
pub const PEERS_KEY: &str = "peers";
/// The key of a peers entry holding the peer's handle (spec 3.10).
pub const HANDLE_KEY: &str = "handle";
/// The key of a peers entry holding the peer's status (spec 3.10).
const STATUS_KEY: &str = "status";
/// The field of an events line naming a peer by the handle this member holds for it (spec/observability.md decision 6).
const HANDLE_FIELD: &str = "handle";
/// How long a member may take to list a peer as a test wants it, through gossip or the direct protocol.
pub const PRESENCE_WAIT: Duration = Duration::from_secs(20);
/// The status the peers tool and `show-peers` give a peer whose Presence arrived (spec 3.1).
pub const ONLINE: &str = "online";
/// The status the peers tool gives a peer whose Departure arrived (spec 3.1).
const GONE: &str = "gone";

/// A running `estafette mcp` process driven as an MCP client over its standard streams, with the events it records, stopped when dropped.
pub struct McpServer {
    process: Child,
    messages: Receiver<Value>,
    next_request_id: u64,
    events: MemberEvents,
}

/// A tool call written to the server whose answer has not been read yet.
pub struct PendingCall {
    request_id: u64,
}

impl McpServer {
    /// Takes the spawned server over its piped standard streams, owning it before anything can fail so a failure ends it, and completes the MCP handshake, its events read through the reader handed in.
    #[track_caller]
    pub fn handshaken(process: Child, events: MemberEvents) -> Self {
        let (forward, messages) = mpsc::channel();
        let mut server = Self {
            process,
            messages,
            next_request_id: 1,
            events,
        };
        let output = BufReader::new(server.process.stdout.take().unwrap());
        std::thread::spawn(move || {
            for line in output.lines().map_while(Result::ok) {
                if let Ok(message) = serde_json::from_str(&line)
                    && forward.send(message).is_err()
                {
                    return;
                }
            }
        });
        server.request(
            "initialize",
            &json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": CLIENT_NAME, "version": CLIENT_VERSION }
            }),
        );
        server
            .write(&json!({ "jsonrpc": JSON_RPC_VERSION, "method": "notifications/initialized" }));
        server
    }

    /// Sends the JSON-RPC request and returns the result its response carries.
    #[track_caller]
    pub fn request(&mut self, method: &str, params: &Value) -> Value {
        let request_id = self.send_request(method, params);
        self.response_to(request_id)["result"].clone()
    }

    /// Calls the tool and returns the envelope its result carries.
    #[track_caller]
    pub fn call(&mut self, tool: &str, arguments: &Value) -> Value {
        let call = self.start_call(tool, arguments);
        self.answer(&call)
    }

    /// Sends the body to the handle through the send tool and returns the envelope.
    #[track_caller]
    pub fn send(&mut self, to: &str, body: &str) -> Value {
        self.call(SEND_TOOL, &send_arguments(to, body))
    }

    /// The events the server records, awaited in its trace directory.
    pub const fn events(&self) -> &MemberEvents {
        &self.events
    }

    /// The peer of that handle as the peers tool lists it once the peer meets the condition, asked at once and again each time the server records a line naming the handle, requiring that within the wait.
    #[track_caller]
    pub fn peer_within(
        &mut self,
        handle: &str,
        within: Duration,
        condition: impl Fn(&Value) -> bool,
    ) -> Value {
        let events = self.events.clone();
        let mut lines_naming_the_peer = None;
        events.wait_until(&format!("{handle} listed as wanted"), within, |lines| {
            let naming = lines
                .iter()
                .filter(|line| names_the_peer(line, handle))
                .count();
            if lines_naming_the_peer.replace(naming) == Some(naming) {
                return None;
            }
            self.listed_peer(handle).filter(|peer| condition(peer))
        })
    }

    /// The peer of that handle as the peers tool lists it now, none when it lists no such peer.
    #[track_caller]
    pub fn listed_peer(&mut self, handle: &str) -> Option<Value> {
        let listed = self.call(PEERS_TOOL, &json!({}));
        listed[RESULT_KEY][PEERS_KEY]
            .as_array()
            .into_iter()
            .flatten()
            .find(|peer| peer[HANDLE_KEY] == handle)
            .cloned()
    }

    /// Writes the tool call without waiting for its answer.
    pub fn start_call(&mut self, tool: &str, arguments: &Value) -> PendingCall {
        PendingCall {
            request_id: self.send_request(
                "tools/call",
                &json!({ "name": tool, "arguments": arguments }),
            ),
        }
    }

    /// The envelope the answer to the pending call carries.
    #[track_caller]
    pub fn answer(&self, call: &PendingCall) -> Value {
        self.response_to(call.request_id)["result"]["structuredContent"].clone()
    }

    /// Closes the server's standard input, the end of input that stops its session.
    pub fn close_standard_input(&mut self) {
        drop(self.process.stdin.take());
    }

    /// Kills the process with SIGKILL.
    pub fn kill(&mut self) {
        drop(self.process.kill());
    }

    /// Waits for the process to exit and returns how it exited.
    pub fn wait_for_exit(&mut self) -> ExitStatus {
        self.process.wait().unwrap()
    }

    /// Ends the process at once, its standard input closed, killed and waited for, so nothing of it runs on once this returns.
    pub fn end(&mut self) {
        self.close_standard_input();
        self.kill();
        self.wait_for_exit();
    }

    /// Writes the request and returns its id.
    fn send_request(&mut self, method: &str, params: &Value) -> u64 {
        let request_id = self.next_request_id;
        self.next_request_id = request_id.checked_add(1).unwrap();
        self.write(&json!({
            "jsonrpc": JSON_RPC_VERSION,
            "id": request_id,
            "method": method,
            "params": params
        }));
        request_id
    }

    /// The response to the request of that id, skipping every other message.
    #[track_caller]
    fn response_to(&self, request_id: u64) -> Value {
        loop {
            let message = self
                .messages
                .recv_timeout(RESPONSE_WAIT)
                .unwrap_or_else(|failure| {
                    panic!(
                        "no response to request {request_id} within {RESPONSE_WAIT:?}: {failure}"
                    )
                });
            if message["id"] == json!(request_id) {
                return message;
            }
        }
    }

    /// Writes one JSON-RPC message as a line on the server's standard input.
    fn write(&mut self, message: &Value) {
        let input = self.process.stdin.as_mut().unwrap();
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.end();
    }
}

/// Whether the events line names the peer of that handle.
fn names_the_peer(line: &TraceLine, handle: &str) -> bool {
    line.value(HANDLE_FIELD) == Some(&json!(handle))
}

/// The init tool's arguments claiming the card of that name (spec 3.10).
pub fn init_arguments(card: &str) -> Value {
    json!({ (CARD_ARGUMENT): card })
}

/// Whether the tool's envelope says its call succeeded (spec 3.10).
pub fn call_succeeded(envelope: &Value) -> bool {
    envelope[OK_KEY] == json!(true)
}

/// The send tool's arguments for the body to the handle (spec 3.10).
pub fn send_arguments(to: &str, body: &str) -> Value {
    json!({ (TO_ARGUMENT): to, (BODY_ARGUMENT): body })
}

/// Whether the peers tool lists the peer online.
pub fn online(peer: &Value) -> bool {
    peer[STATUS_KEY] == json!(ONLINE)
}

/// Whether the peers tool lists the peer gone.
pub fn gone(peer: &Value) -> bool {
    peer[STATUS_KEY] == json!(GONE)
}

#[cfg(test)]
mod tests {
    use std::io::{self, PipeReader, Read};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::McpServer;
    use crate::member_events::MemberEvents;

    /// A command that runs until it is ended, reading nothing and printing nothing.
    const RUNS_UNTIL_ENDED: [&str; 3] = ["tail", "-f", "/dev/null"];
    /// How long the end of a stand-in may take to reach the pipe only it holds once its owner ended it: chosen, not measured, far above a kill and a wait.
    const STAND_IN_ENDED_WITHIN: Duration = Duration::from_secs(10);

    /// Whether the end of the pipe that only the stand-in holds open closes within the bound, the sign the stand-in has ended, read on a thread of its own so no signal is sent to any process.
    fn stand_in_ended_within(only_the_stand_in_writes: PipeReader, within: Duration) -> bool {
        let (closed, close_seen) = mpsc::channel();
        std::thread::spawn(move || {
            let mut written = Vec::new();
            drop(closed.send((&only_the_stand_in_writes).read_to_end(&mut written)));
        });
        close_seen.recv_timeout(within).is_ok()
    }

    #[test]
    fn observability_decision_16_a_server_whose_handshake_fails_before_it_starts_is_ended_before_the_failure_reaches_its_machine()
     {
        let (only_the_stand_in_writes, stand_in_end) = io::pipe().unwrap();
        let process = Command::new(RUNS_UNTIL_ENDED[0])
            .args(&RUNS_UNTIL_ENDED[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(stand_in_end)
            .spawn()
            .unwrap();
        let events = MemberEvents::new("member with no output", Path::new("/nonexistent"));

        let handshake = catch_unwind(AssertUnwindSafe(|| McpServer::handshaken(process, events)));

        let ended = stand_in_ended_within(only_the_stand_in_writes, STAND_IN_ENDED_WITHIN);
        let Err(_failure) = handshake else {
            panic!("a server whose output is not piped was handshaken")
        };
        assert!(
            ended,
            "the stand-in still ran {STAND_IN_ENDED_WITHIN:?} after its handshake failed"
        );
    }
}
