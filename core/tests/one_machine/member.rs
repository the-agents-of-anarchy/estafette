use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::Command;
use std::time::Duration;

use iroh::{EndpointAddr, EndpointId, TransportAddr};
use serde_json::{Value, json};

use crate::mcp_server::{INIT_TOOL, McpServer, RESULT_KEY, call_succeeded, init_arguments};
use crate::member_events::{COUNTERS_EVENT, MemberEvents};
use crate::node_start::{NODE_START_MOMENT, is_counters_snapshot_of_moment};
use crate::scratch_machine::Card;
use crate::scratch_machine::owned_process::OwnedProcess;
use crate::scripted_node::ScriptedNode;
use crate::time_passing::wait_for_time_to_pass;
use crate::trace_reader::{TraceLine, WallMs};
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
/// The address of this machine that its members reach one another on.
pub const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A running `estafette mcp` process driven as an MCP client, with the process id its UDP sockets are listed by and the events it records awaited in its trace directory.
pub struct Member {
    server: McpServer,
    process_id: u32,
}

impl Member {
    /// The MCP server the member drives.
    pub const fn server(&self) -> &McpServer {
        &self.server
    }

    /// The MCP server the member drives, for the calls that write to it.
    pub const fn server_mut(&mut self) -> &mut McpServer {
        &mut self.server
    }

    /// Takes the spawned MCP server over, completes the MCP handshake, and answers for its events through the reader handed in.
    #[track_caller]
    pub fn start(process: OwnedProcess, events: MemberEvents) -> Self {
        let process_id = process.child().id();
        Self {
            server: McpServer::handshaken(process, events),
            process_id,
        }
    }

    /// The bodies of the messages the inbox tool returns, in the order it returns them.
    #[track_caller]
    pub fn inbox_bodies(&mut self) -> Vec<Value> {
        bodies_in(&self.server_mut().call(INBOX_TOOL, &json!({})))
    }

    /// Claims the card while the node admits this process's endpoint, requiring the claim to succeed, and returns the init envelope.
    #[track_caller]
    pub fn init_through(&mut self, card: &Card, node: &ScriptedNode) -> Value {
        let init = self
            .server_mut()
            .start_call(INIT_TOOL, &init_arguments(&card.name));
        node.admit(loopback_address(card.id, &self.bound_udp_ports()));
        let claim = self.server().answer(&init);
        assert!(call_succeeded(&claim), "{claim}");
        claim
    }

    /// The first line of the member's events files that names the event, found already written or awaited until it is written, failing with the member's timeline so far when the bound runs out.
    #[track_caller]
    pub fn wait_for_event(&self, event: &str, within: Duration) -> TraceLine {
        wait_for_first_line_named(self.server.events(), event, within)
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
    pub fn wait_for_time_to_pass(&self, since: WallMs, span: Duration) {
        wait_for_time_to_pass(self.server.events(), since, span);
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
        self.counters_snapshot_of_moment(NODE_START_MOMENT, ENDPOINT_BIND_WAIT);
        self.ipv4_udp_ports_held("no UDP socket bound")
    }

    /// The first counters line the member records at the moment, awaited within the wait.
    #[track_caller]
    pub fn counters_snapshot_of_moment(&self, moment: &str, within: Duration) -> TraceLine {
        self.server.events().wait_until(
            &format!("{COUNTERS_EVENT} of its node's {moment}"),
            within,
            |lines| {
                lines
                    .into_iter()
                    .find(|line| is_counters_snapshot_of_moment(line, moment))
            },
        )
    }

    /// The ports of the IPv4 UDP sockets this process holds now, failing with the text given when it holds none.
    #[track_caller]
    pub fn ipv4_udp_ports_held(&self, none_held: &str) -> Vec<u16> {
        let held = self.ipv4_udp_ports();
        assert!(!held.is_empty(), "{none_held}");
        held
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

/// The first line of the events that names the event, found already written or awaited until it is written, failing with the member's timeline so far when the bound runs out.
#[track_caller]
pub fn wait_for_first_line_named(
    events: &MemberEvents,
    event: &str,
    within: Duration,
) -> TraceLine {
    events.wait_until(event, within, first_line_named(event))
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

#[cfg(test)]
mod stopping_a_claimed_member {
    use std::time::Duration;

    use serde_json::json;

    use super::{INBOX_TOOL, Member};
    use crate::local_network::{AMY_CARD, AMY_HANDLE, LocalNetwork};
    use crate::scratch_machine::KeptFilesPolicy;

    /// How long a member may take to record a step of its stopping once it is signalled.
    const STEP_WAIT: Duration = Duration::from_secs(20);
    /// The event a member records when it has handed its Departure over (spec 3.8).
    const DEPARTURE_HANDED_OVER_EVENT: &str = "process.departure_handed_over";
    /// The event a member records when it has released its sidecar lock (spec 3.8).
    const LOCK_RELEASED_EVENT: &str = "process.lock_released";
    /// The event a member records when it has decided whether to save its network configuration (spec 3.8).
    const CONFIGURATION_REWRITE_DECIDED_EVENT: &str = "process.configuration_rewrite_decided";
    /// The event a member records when it has dropped its held connections (spec 3.8).
    const HELD_CONNECTIONS_DROPPED_EVENT: &str = "process.held_connections_dropped";
    /// The event a member records for each signal it receives (spec 3.9).
    const SIGNAL_RECEIVED_EVENT: &str = "process.signal_received";
    /// The event a member records when its departure wait ends (spec 3.8).
    const DEPARTURE_WAIT_OVER_EVENT: &str = "process.departure_wait_over";
    /// The event a member records when its process exits.
    const EXITED_EVENT: &str = "process.exited";
    /// The stopping steps of spec:784 in the order the member does them.
    const STOPPING_STEPS_IN_ORDER: [&str; 4] = [
        DEPARTURE_HANDED_OVER_EVENT,
        LOCK_RELEASED_EVENT,
        CONFIGURATION_REWRITE_DECIDED_EVENT,
        HELD_CONNECTIONS_DROPPED_EVENT,
    ];
    /// The code an inbox call is answered with once the member is stopping (spec 3.8).
    const REFUSED_CODE: &str = "refused";

    /// A member that claimed a card through a stand-in host, with the network it lives in kept alive beside it.
    fn claimed_member(test_tag: &str) -> (Member, LocalNetwork) {
        let network = LocalNetwork::create(test_tag, &KeptFilesPolicy::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let host = network.stand_in_host();
        let mut member = network.start_member();
        member.init_through(&card, &host);
        (member, network)
    }

    #[test]
    fn spec_3_8_a_claimed_member_releases_its_lock_before_it_decides_the_configuration_rewrite() {
        let (member, _network) = claimed_member("stopping-order");

        member.terminate();
        let steps = member.wait_until("every stopping step", STEP_WAIT, |lines| {
            let steps: Vec<String> = lines
                .into_iter()
                .map(|line| line.event)
                .filter(|event| STOPPING_STEPS_IN_ORDER.contains(&event.as_str()))
                .collect();
            steps
                .iter()
                .any(|step| step == HELD_CONNECTIONS_DROPPED_EVENT)
                .then_some(steps)
        });

        assert_eq!(steps, STOPPING_STEPS_IN_ORDER);
    }

    #[test]
    fn spec_3_9_a_second_signal_while_stopping_exits_the_member_before_its_departure_wait_ends() {
        let (mut member, _network) = claimed_member("second-signal");

        member.terminate();
        member.wait_for_event(DEPARTURE_HANDED_OVER_EVENT, STEP_WAIT);
        member.terminate();
        member.server_mut().wait_for_exit();
        let events = member.wait_until("the exit", STEP_WAIT, |lines| {
            lines
                .iter()
                .any(|line| line.event == EXITED_EVENT)
                .then(|| lines.into_iter().map(|line| line.event).collect::<Vec<_>>())
        });

        assert_eq!(
            events
                .iter()
                .filter(|event| *event == SIGNAL_RECEIVED_EVENT)
                .count(),
            2,
            "{events:?}"
        );
        assert!(
            !events.contains(&DEPARTURE_WAIT_OVER_EVENT.to_owned()),
            "{events:?}"
        );
    }

    #[test]
    fn spec_3_8_the_inbox_tool_of_a_stopping_member_is_refused() {
        let (mut member, _network) = claimed_member("inbox-while-stopping");

        member.terminate();
        member.wait_for_event(DEPARTURE_HANDED_OVER_EVENT, STEP_WAIT);
        let answer = member.server_mut().call(INBOX_TOOL, &json!({}));

        assert_eq!(answer["error"]["code"], json!(REFUSED_CODE), "{answer}");
    }
}
