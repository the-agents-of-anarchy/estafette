use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use iroh::{EndpointAddr, EndpointId, TransportAddr};
use serde_json::{Value, json};

use crate::machine::Card;
use crate::scripted_node::ScriptedNode;

const JSON_RPC_VERSION: &str = "2.0";
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const CLIENT_NAME: &str = "estafette-acceptance";
const CLIENT_VERSION: &str = "0";
const RESPONSE_WAIT: Duration = Duration::from_secs(30);
const PEERS_POLL: Duration = Duration::from_millis(50);
const ENDPOINT_BIND_WAIT: Duration = Duration::from_secs(10);
const ENDPOINT_BIND_POLL: Duration = Duration::from_millis(20);
const OPEN_FILES_COMMAND: &str = "lsof";
const IPV4_SOCKET_TYPE: &str = "IPv4";
const LSOF_TYPE_FIELD: char = 't';
const LSOF_NAME_FIELD: char = 'n';
const PORT_SEPARATOR: char = ':';
const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A running `estafette mcp` process driven as an MCP client over its standard streams.
pub struct Member {
    process: Child,
    messages: Receiver<Value>,
    next_request_id: u64,
}

/// A tool call written to the member whose answer has not been read yet.
pub struct PendingCall {
    request_id: u64,
}

impl Member {
    /// Starts the command as an MCP server writing its run record to the file, and completes the MCP handshake.
    pub fn start(mut command: Command, run_record: File) -> Self {
        let mut process = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(run_record)
            .spawn()
            .unwrap();
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
        let mut member = Self {
            process,
            messages,
            next_request_id: 1,
        };
        member.request(
            "initialize",
            &json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": CLIENT_NAME, "version": CLIENT_VERSION }
            }),
        );
        member
            .write(&json!({ "jsonrpc": JSON_RPC_VERSION, "method": "notifications/initialized" }));
        member
    }

    /// Calls the tool and returns the envelope its result carries.
    pub fn call(&mut self, tool: &str, arguments: &Value) -> Value {
        let call = self.start_call(tool, arguments);
        self.answer(&call)
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
    pub fn answer(&self, call: &PendingCall) -> Value {
        self.response_to(call.request_id)["result"]["structuredContent"].clone()
    }

    /// Sends the JSON-RPC request and returns the result of its response.
    pub fn request(&mut self, method: &str, params: &Value) -> Value {
        let request_id = self.send_request(method, params);
        self.response_to(request_id)["result"].clone()
    }

    /// Claims the card while the node admits this process's endpoint, requiring the claim to succeed, and returns the init envelope.
    pub fn init_through(&mut self, card: &Card, node: &ScriptedNode) -> Value {
        let init = self.start_call("init", &json!({ "card": card.name }));
        node.admit(loopback_address(card.id, &self.bound_udp_ports()));
        let claim = self.answer(&init);
        assert_eq!(claim["ok"], json!(true), "{claim}");
        claim
    }

    /// The peer of that handle as the peers tool lists it once the peer meets the condition, requiring that within the wait.
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

    /// The loopback-reachable UDP ports this process's endpoint bound, waiting for the bind.
    pub fn bound_udp_ports(&self) -> Vec<u16> {
        let deadline = Instant::now().checked_add(ENDPOINT_BIND_WAIT).unwrap();
        loop {
            let ports = self.ipv4_udp_ports();
            if !ports.is_empty() {
                return ports;
            }
            assert!(Instant::now() < deadline, "no UDP socket bound");
            std::thread::sleep(ENDPOINT_BIND_POLL);
        }
    }

    /// The ports of the IPv4 UDP sockets this process holds now.
    pub fn ipv4_udp_ports(&self) -> Vec<u16> {
        ipv4_udp_ports_of(self.process.id())
    }

    /// Closes the member's standard input, the end of input that stops its session.
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
    fn response_to(&self, request_id: u64) -> Value {
        loop {
            let message = self.messages.recv_timeout(RESPONSE_WAIT).unwrap();
            if message["id"] == json!(request_id) {
                return message;
            }
        }
    }

    /// Writes one JSON-RPC message as a line on the member's standard input.
    fn write(&mut self, message: &Value) {
        let input = self.process.stdin.as_mut().unwrap();
        writeln!(input, "{message}").unwrap();
        input.flush().unwrap();
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        self.close_standard_input();
        self.kill();
        self.wait_for_exit();
    }
}

/// The address that reaches the node at the UDP ports on this machine's loopback alone.
pub fn loopback_address(node: EndpointId, ports: &[u16]) -> EndpointAddr {
    EndpointAddr::from_parts(
        node,
        ports
            .iter()
            .map(|port| TransportAddr::Ip(SocketAddr::new(LOOPBACK, *port))),
    )
}

/// The ports of the IPv4 UDP sockets the process holds, as the open files command lists them.
fn ipv4_udp_ports_of(process_id: u32) -> Vec<u16> {
    let listing = Command::new(OPEN_FILES_COMMAND)
        .args(["-nP", "-a", "-p", &process_id.to_string(), "-iUDP", "-Ftn"])
        .output()
        .unwrap();
    let mut socket_type = String::new();
    let mut ports = Vec::new();
    for field in String::from_utf8(listing.stdout).unwrap().lines() {
        if let Some(listed_type) = field.strip_prefix(LSOF_TYPE_FIELD) {
            listed_type.clone_into(&mut socket_type);
        } else if let Some(name) = field.strip_prefix(LSOF_NAME_FIELD)
            && socket_type == IPV4_SOCKET_TYPE
            && let Some(Ok(port)) = name.rsplit(PORT_SEPARATOR).next().map(str::parse)
        {
            ports.push(port);
        }
    }
    ports
}
