use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::process::Command;
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId, TransportAddr};
use serde_json::{Value, json};

use crate::mcp_server::{INIT_TOOL, McpServer, RESULT_KEY, call_succeeded, init_arguments};
use crate::member_events::{COUNTERS_EVENT, MemberEvents};
use crate::node_start::{NODE_START_MOMENT, is_node_start_snapshot};
use crate::scratch_machine::Card;
use crate::scratch_machine::owned_process::OwnedProcess;
use crate::scripted_node::ScriptedNode;
use crate::time_passing::wait_for_time_to_pass;
use crate::trace_reader::TraceLine;
/// The event a member records for each frame it delivers to its inbox (spec/observability.md decision 9).
const FRAME_DELIVERED_EVENT: &str = "inbox.frame_delivered";
/// The tool a member reads its inbox with, the key of its result listing the messages, and the key of a message's body (spec 3.10).
const INBOX_TOOL: &str = "inbox";
const MESSAGES_KEY: &str = "messages";
const BODY_KEY: &str = "body";

const ENDPOINT_BIND_WAIT: Duration = Duration::from_secs(10);
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
}

impl Deref for Member {
    type Target = McpServer;

    fn deref(&self) -> &McpServer {
        &self.server
    }
}

impl DerefMut for Member {
    fn deref_mut(&mut self) -> &mut McpServer {
        &mut self.server
    }
}

impl Member {
    /// Takes the spawned MCP server over, completes the MCP handshake, and answers for its events through the reader handed in.
    #[track_caller]
    pub fn start(process: OwnedProcess, events: MemberEvents) -> Self {
        let process_id = process.id();
        Self {
            server: McpServer::handshaken(process, events),
            process_id,
        }
    }

    /// The bodies of the messages the inbox tool returns, in the order it returns them.
    #[track_caller]
    pub fn inbox_bodies(&mut self) -> Vec<Value> {
        bodies_in(&self.call(INBOX_TOOL, &json!({})))
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

    /// The first line of the member's events files that names the event, found already written or awaited until it is written, failing with the member's timeline so far when the bound runs out.
    #[track_caller]
    pub fn wait_for_event(&self, event: &str, within: Duration) -> TraceLine {
        self.server
            .events()
            .wait_until(event, within, first_line_named(event))
    }

    /// The first value `found_in` makes of every line of the member's events files, awaited as `MemberEvents::wait_until` awaits it.
    #[track_caller]
    pub fn wait_until<Found>(
        &self,
        awaited: &str,
        within: Duration,
        found_in: impl FnMut(Vec<TraceLine>) -> Option<Found>,
    ) -> Found {
        self.server.events().wait_until(awaited, within, found_in)
    }

    /// Waits until the member records a counters snapshot at least `span` after the wall-clock instant, as `time_passing::wait_for_time_to_pass` waits.
    #[track_caller]
    pub fn wait_for_time_to_pass(&self, since_wall_ms: u64, span: Duration) {
        wait_for_time_to_pass(self.server.events(), since_wall_ms, span);
    }

    /// Waits until the inbox tool returns the body, asked at once and again each time the member records a frame delivered.
    #[track_caller]
    pub fn wait_for_inbox_body(&mut self, body: &str, within: Duration) {
        let mut frames_delivered = None;
        let events = self.server.events().clone();
        events.wait_until(&format!("{body} in the inbox"), within, |lines| {
            let delivered = lines
                .iter()
                .filter(|line| line.event == FRAME_DELIVERED_EVENT)
                .count();
            if frames_delivered.replace(delivered) == Some(delivered) {
                return None;
            }
            bodies_in(&self.server.call(INBOX_TOOL, &json!({})))
                .contains(&Value::from(body))
                .then_some(())
        });
    }

    /// The loopback-reachable UDP ports this process's endpoint bound, once it records its node's start on the bound endpoint.
    #[track_caller]
    pub fn bound_udp_ports(&self) -> Vec<u16> {
        self.server.events().wait_until(
            &format!("{COUNTERS_EVENT} of its node's {NODE_START_MOMENT}"),
            ENDPOINT_BIND_WAIT,
            |lines| lines.iter().any(is_node_start_snapshot).then_some(()),
        );
        let ports = self.ipv4_udp_ports();
        assert!(!ports.is_empty(), "no UDP socket bound");
        ports
    }

    /// The ports of the IPv4 UDP sockets this process holds now.
    pub fn ipv4_udp_ports(&self) -> Vec<u16> {
        ipv4_udp_ports_of(self.process_id)
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
}

/// What finds the first events line that names the event.
pub fn first_line_named(event: &str) -> impl FnMut(Vec<TraceLine>) -> Option<TraceLine> + '_ {
    move |lines| lines.into_iter().find(|line| line.event == event)
}

/// The bodies of the messages an inbox tool envelope returns, in its order.
fn bodies_in(envelope: &Value) -> Vec<Value> {
    envelope[RESULT_KEY][MESSAGES_KEY]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message[BODY_KEY].clone())
        .collect()
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
    let address = node.endpoint().addr();
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
