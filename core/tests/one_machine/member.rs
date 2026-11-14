use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use iroh::{EndpointAddr, EndpointId, TransportAddr};
use serde_json::{Value, json};

use crate::mcp_server::{INIT_TOOL, McpServer, PendingCall, call_succeeded, init_arguments};
use crate::member_events::MemberEvents;
use crate::scratch_machine::Card;
use crate::scripted_node::ScriptedNode;
use crate::trace_reader::TraceLine;

const ENDPOINT_BIND_WAIT: Duration = Duration::from_secs(10);
const ENDPOINT_BIND_POLL: Duration = Duration::from_millis(20);
const OPEN_FILES_COMMAND: &str = "lsof";
const SIGNAL_COMMAND: &str = "kill";
const TERMINATE_SIGNAL_FLAG: &str = "-TERM";
const IPV4_SOCKET_TYPE: &str = "IPv4";
const LSOF_TYPE_FIELD: char = 't';
const LSOF_NAME_FIELD: char = 'n';
const PORT_SEPARATOR: char = ':';
const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A running `estafette mcp` process driven as an MCP client, with the process id its UDP sockets are listed by and the events it records awaited in its trace directory.
pub struct Member {
    server: McpServer,
    process_id: u32,
    events: MemberEvents,
}

impl Member {
    /// Takes the spawned MCP server over, completes the MCP handshake, and answers for its events through the reader handed in.
    #[track_caller]
    pub fn start(process: Child, events: MemberEvents) -> Self {
        let process_id = process.id();
        Self {
            server: McpServer::handshaken(process),
            process_id,
            events,
        }
    }

    /// Calls the tool and returns the envelope its result carries.
    #[track_caller]
    pub fn call(&mut self, tool: &str, arguments: &Value) -> Value {
        self.server.call(tool, arguments)
    }

    /// Sends the body to the handle through the send tool and returns the envelope.
    #[track_caller]
    pub fn send(&mut self, to: &str, body: &str) -> Value {
        self.server.send(to, body)
    }

    /// Writes the tool call without waiting for its answer.
    pub fn start_call(&mut self, tool: &str, arguments: &Value) -> PendingCall {
        self.server.start_call(tool, arguments)
    }

    /// The envelope the answer to the pending call carries.
    #[track_caller]
    pub fn answer(&self, call: &PendingCall) -> Value {
        self.server.answer(call)
    }

    /// Sends the JSON-RPC request and returns the result its response carries.
    #[track_caller]
    pub fn request(&mut self, method: &str, params: &Value) -> Value {
        self.server.request(method, params)
    }

    /// The bodies of the messages the inbox tool returns, in the order it returns them.
    #[track_caller]
    pub fn inbox_bodies(&mut self) -> Vec<Value> {
        self.call("inbox", &json!({}))["result"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["body"].clone())
            .collect()
    }

    /// Claims the card while the node admits this process's endpoint, requiring the claim to succeed, and returns the init envelope.
    #[track_caller]
    pub fn init_through(&mut self, card: &Card, node: &ScriptedNode) -> Value {
        let init = self.start_call(INIT_TOOL, &init_arguments(&card.name));
        node.admit(loopback_address(card.id, &self.bound_udp_ports()));
        let claim = self.answer(&init);
        assert!(call_succeeded(&claim), "{claim}");
        claim
    }

    /// The peer of that handle as the peers tool lists it once the peer meets the condition, requiring that within the wait.
    #[track_caller]
    pub fn peer_within(
        &mut self,
        handle: &str,
        within: Duration,
        condition: impl Fn(&Value) -> bool,
    ) -> Value {
        self.server.peer_within(handle, within, condition)
    }

    /// The first line of the member's events files that names the event, found already written or awaited until it is written, failing with the member's timeline so far when the bound runs out.
    #[track_caller]
    pub fn wait_for_event(&self, event: &str, within: Duration) -> TraceLine {
        self.events.wait_for(event, within)
    }

    /// The loopback-reachable UDP ports this process's endpoint bound, waiting for the bind.
    #[track_caller]
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
        ipv4_udp_ports_of(self.process_id)
    }

    /// Closes the member's standard input, the end of input that stops its session.
    pub fn close_standard_input(&mut self) {
        self.server.close_standard_input();
    }

    /// Sends the process SIGTERM, the signal that stops its session with the runtime shut down in the background.
    #[track_caller]
    pub fn terminate(&self) {
        let sent = Command::new(SIGNAL_COMMAND)
            .args([TERMINATE_SIGNAL_FLAG, &self.process_id.to_string()])
            .status()
            .unwrap();
        assert!(
            sent.success(),
            "SIGTERM was not sent to {}",
            self.process_id
        );
    }

    /// Kills the process with SIGKILL.
    pub fn kill(&mut self) {
        self.server.kill();
    }

    /// Waits for the process to exit and returns how it exited.
    pub fn wait_for_exit(&mut self) -> ExitStatus {
        self.server.wait_for_exit()
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

/// The address that reaches the scripted node at its UDP ports on this machine's loopback alone.
pub fn loopback_address_of(node: &ScriptedNode) -> EndpointAddr {
    let address = node.endpoint().endpoint().addr();
    let ports: Vec<u16> = address.ip_addrs().map(SocketAddr::port).collect();
    loopback_address(address.id, &ports)
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
