use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use iroh::{EndpointAddr, EndpointId, TransportAddr};
use serde_json::{Value, json};

use crate::mcp_server::McpServer;
use crate::scratch_machine::Card;
use crate::scripted_node::ScriptedNode;

const PEERS_POLL: Duration = Duration::from_millis(50);
const ENDPOINT_BIND_WAIT: Duration = Duration::from_secs(10);
const ENDPOINT_BIND_POLL: Duration = Duration::from_millis(20);
const OPEN_FILES_COMMAND: &str = "lsof";
const IPV4_SOCKET_TYPE: &str = "IPv4";
const LSOF_TYPE_FIELD: char = 't';
const LSOF_NAME_FIELD: char = 'n';
const PORT_SEPARATOR: char = ':';
const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A running `estafette mcp` process driven as an MCP client, with the process id its UDP sockets are listed by.
pub struct Member {
    server: McpServer,
    process_id: u32,
}

impl Member {
    /// Takes the spawned MCP server over and completes the MCP handshake.
    pub fn start(process: Child) -> Self {
        let process_id = process.id();
        Self {
            server: McpServer::handshaken(process),
            process_id,
        }
    }

    /// Calls the tool and returns the envelope its result carries.
    pub fn call(&mut self, tool: &str, arguments: &Value) -> Value {
        self.server.call(tool, arguments)
    }

    /// Claims the card while the node admits this process's endpoint, requiring the claim to succeed, and returns the init envelope.
    pub fn init_through(&mut self, card: &Card, node: &ScriptedNode) -> Value {
        let init = self
            .server
            .start_call("init", &json!({ "card": card.name }));
        node.admit(loopback_address(card.id, &self.bound_udp_ports()));
        let claim = self.server.answer(&init);
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
        ipv4_udp_ports_of(self.process_id)
    }

    /// Closes the member's standard input, the end of input that stops its session.
    pub fn close_standard_input(&mut self) {
        self.server.close_standard_input();
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
