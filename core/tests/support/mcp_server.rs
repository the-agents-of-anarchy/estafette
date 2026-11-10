use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// The JSON-RPC version MCP speaks.
const JSON_RPC_VERSION: &str = "2.0";
/// The MCP protocol version the client asks for.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// The name the client gives in its handshake.
const CLIENT_NAME: &str = "estafette-acceptance";
/// The version the client gives in its handshake.
const CLIENT_VERSION: &str = "0";
/// How long the server may take to answer a request.
const RESPONSE_WAIT: Duration = Duration::from_secs(60);
/// How often the peers tool is called while waiting for a peer.
const PEERS_POLL: Duration = Duration::from_millis(50);

/// A running `estafette mcp` process driven as an MCP client over its standard streams, stopped when dropped.
pub struct McpServer {
    process: Child,
    messages: Receiver<Value>,
    next_request_id: u64,
}

/// A tool call written to the server whose answer has not been read yet.
pub struct PendingCall {
    request_id: u64,
}

impl McpServer {
    /// Takes the spawned server over its piped standard streams and completes the MCP handshake.
    #[track_caller]
    pub fn handshaken(mut process: Child) -> Self {
        let output = BufReader::new(process.stdout.take().unwrap());
        let (forward, messages) = mpsc::channel();
        std::thread::spawn(move || {
            for line in output.lines().map_while(Result::ok) {
                if let Ok(message) = serde_json::from_str(&line)
                    && forward.send(message).is_err()
                {
                    return;
                }
            }
        });
        let mut server = Self {
            process,
            messages,
            next_request_id: 1,
        };
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

    /// The peer of that handle as the peers tool lists it once the peer meets the condition, requiring that within the wait.
    #[track_caller]
    pub fn peer_within(
        &mut self,
        handle: &str,
        within: Duration,
        condition: impl Fn(&Value) -> bool,
    ) -> Value {
        let started = Instant::now();
        loop {
            let listed = self.call("peers", &json!({}));
            let entry = listed["result"]["peers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|peer| peer["handle"] == handle);
            if let Some(peer) = entry.filter(|peer| condition(peer)) {
                return peer.clone();
            }
            assert!(
                started.elapsed() < within,
                "{handle} was not listed as wanted within {within:?}; its entry: {entry:?}"
            );
            std::thread::sleep(PEERS_POLL);
        }
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
            let message = self.messages.recv_timeout(RESPONSE_WAIT).unwrap();
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
        self.close_standard_input();
        self.kill();
        self.wait_for_exit();
    }
}
