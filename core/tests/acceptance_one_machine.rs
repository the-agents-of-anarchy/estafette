#[cfg(test)]
#[path = "support/driven_runtime.rs"]
mod driven_runtime;
#[cfg(test)]
#[path = "support/events_lines.rs"]
mod events_lines;
#[cfg(test)]
#[path = "support/file_growth.rs"]
mod file_growth;
#[cfg(test)]
#[path = "support/inbox_client.rs"]
mod inbox_client;
#[cfg(test)]
#[cfg(test)]
#[path = "one_machine/local_network.rs"]
mod local_network;
#[cfg(test)]
#[path = "support/mcp_server.rs"]
mod mcp_server;
#[cfg(test)]
#[path = "one_machine/member.rs"]
mod member;
#[cfg(test)]
#[path = "support/member_ended_at_its_peer.rs"]
mod member_ended_at_its_peer;
#[cfg(test)]
#[path = "support/member_events.rs"]
mod member_events;
#[cfg(test)]
#[path = "support/message_body.rs"]
mod message_body;
#[cfg(test)]
#[path = "support/new_presence_window.rs"]
mod new_presence_window;
#[cfg(test)]
#[path = "support/node_start.rs"]
mod node_start;
#[cfg(test)]
#[path = "support/payload_placeholders.rs"]
mod payload_placeholders;
#[cfg(test)]
#[path = "support/payloads.rs"]
mod payloads;
#[cfg(test)]
#[path = "support/scratch_machine.rs"]
mod scratch_machine;
#[cfg(test)]
#[path = "one_machine/scripted_node.rs"]
mod scripted_node;
#[cfg(test)]
#[path = "support/spec_bounds.rs"]
mod spec_bounds;
#[cfg(test)]
#[path = "support/time_passing.rs"]
mod time_passing;
#[cfg(test)]
#[path = "support/timed_step.rs"]
mod timed_step;
#[cfg(test)]
#[path = "one_machine/trace_events.rs"]
mod trace_events;
#[cfg(test)]
use scratch_machine::trace_reader;

/// The id of every Message a test builds by hand, which no check of these tests reads.
#[cfg(test)]
const MESSAGE_ID: estafette::wire::MessageId = [9; 16];

/// Spec 3.14 observer wait: how long a node waits after its topic reports joined before it assigns its handle, and within which a member lists a node after that node's Presence (spec 3.15 test 19).
#[cfg(test)]
const SPEC_OBSERVER_WAIT: std::time::Duration = std::time::Duration::from_millis(3000);

/// Sends the body to the handle through the member's send tool and returns the envelope.
#[cfg(test)]
#[track_caller]
fn send(from: &mut member::Member, to: &str) -> serde_json::Value {
    from.send(to, message_body::BODY)
}

/// The run every line built by `built_trace_line` belongs to.
#[cfg(test)]
const BUILT_RUN: &str = "fixture-run";

/// A trace line of the run, written as number `n`, naming the event and holding the fields.
#[cfg(test)]
fn built_trace_line(
    run: &str,
    n: u64,
    event: &str,
    fields: &serde_json::Value,
) -> trace_reader::TraceLine {
    let mut object = serde_json::json!({
        "v": 2, "run": run, "n": n, "mono_ms": n, "wall_ms": n, "node": "fixture", "event": event,
    });
    if let (Some(members), Some(extra)) = (object.as_object_mut(), fields.as_object()) {
        members.extend(extra.clone());
    }
    let mut text = serde_json::to_string(&object).unwrap();
    text.push('\n');
    let mut lines =
        trace_reader::parse_trace_lines(text.as_bytes(), trace_reader::WritingProcess::Ended)
            .unwrap();
    lines.pop().unwrap()
}

#[cfg(test)]
mod spec_3_15_restart_and_departure_on_one_machine {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::{Duration, Instant};

    use estafette::crypto::NetworkKeys;
    use estafette::net::endpoint::{DIRECT_ALPN, QUIC_KEEP_ALIVE_INTERVAL};
    use estafette::payload::{open_response, seal_payload, seal_request};
    use estafette::wire::{
        Addr, Departure, Introduce, Message, Payload, Request, Response, Version, WIRE_LIMIT_BYTES,
    };
    use iroh::endpoint::Connection;
    use iroh::{EndpointId, SecretKey, TransportAddr};
    use serde_json::{Value, json};

    use crate::events_lines::{
        ACKNOWLEDGED_OUTCOME, HANDLE_FIELD, INTRODUCE_KIND, KIND_FIELD, MESSAGE_KIND,
        OUTCOME_FIELD, PAYLOAD_ACCEPTED_EVENT, PEER_FIELD, PRESENCE_KIND, REQUEST_SENT_EVENT,
        SEQ_FIELD, short_id_of_node,
    };
    use crate::local_network::{
        AMY_CARD, AMY_HANDLE, BOB_CARD, BOB_HANDLE, CAROL_HANDLE, LocalNetwork, MetPair,
    };
    use crate::mcp_server::{PRESENCE_WAIT, SEND_TOOL, gone, online, send_arguments, unreachable};
    use crate::member::{Member, loopback_address, loopback_address_of};
    use crate::member_ended_at_its_peer::{
        UNREACHABLE_CODE, assert_stopped_member_is_gone_then_unreachable,
    };
    use crate::message_body::BODY;
    use crate::new_presence_window::{
        NewPresenceAccepted, assert_acknowledged_promptly_after, new_presence_accepted,
    };
    use crate::payload_placeholders::{UNSET_MAC, UNSIGNED, wall_clock_ms};
    use crate::payloads::presence_signed_by;
    use crate::scratch_machine::{Card, RunRecordKeeping};
    use crate::scripted_node::{SPEC_DIRECT_TIMEOUT_WITH_SLACK, ScriptedNode};
    use crate::timed_step::timed;
    use crate::trace_reader::TraceLine;
    use crate::{MESSAGE_ID, send};

    /// The body of bob's Message stamped just before his Departure's seq (spec 3.8).
    const SENT_BEFORE_THE_DEPARTURE: &str = "sent before the departure";
    /// The body of bob's Message stamped just after his Departure's seq (spec 3.8).
    const SENT_AFTER_THE_DEPARTURE: &str = "sent after the departure";
    /// The body of the restarted bob's Message sent before its new Presence.
    const SENT_BEFORE_THE_NEW_PRESENCE: &str = "sent before the new presence";
    /// The body of the restarted bob's Message sent after its new Presence.
    const SENT_AFTER_THE_NEW_PRESENCE: &str = "sent after the new presence";
    /// An address of the documentation range 192.0.2.0/24, where no host is reached.
    const DOCUMENTATION_HOST: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    /// The body of live bob's Message sent before his Presence.
    const SENT_BEFORE_THE_PRESENCE: &str = "sent before the presence";
    /// The body of live bob's Message sent after his Presence.
    const SENT_AFTER_THE_PRESENCE: &str = "sent after the presence";
    /// The keep-alives a held connection leaves unanswered before the member judges its process gone (architecture log, 2026-11-10).
    const KEEP_ALIVES_A_GONE_PROCESS_LEAVES_UNANSWERED: u32 = 3;
    /// How long a held connection goes unheard before the member closes it as a gone process's.
    const GONE_PROCESS_SILENCE: Duration =
        QUIC_KEEP_ALIVE_INTERVAL.saturating_mul(KEEP_ALIVES_A_GONE_PROCESS_LEAVES_UNANSWERED);
    /// How many times per keep-alive interval the member samples whether a held connection was heard.
    const HEARING_SAMPLES_PER_KEEP_ALIVE: u32 = 2;
    /// How often the member samples whether a held connection was heard, the delay beyond the silence before it judges.
    const HEARING_SAMPLE_INTERVAL: Duration = QUIC_KEEP_ALIVE_INTERVAL
        .checked_div(HEARING_SAMPLES_PER_KEEP_ALIVE)
        .unwrap();
    /// The event a member records when it replaces a peer's lookup entry, the last step of applying a Presence.
    const LOOKUP_REPLACED_EVENT: &str = "peers.lookup_replaced";
    /// How long the wait for a killed peer's entry to change lasts, a wait's bound that decides no run in which the counts of spec 3.15 test 5 hold.
    const KILLED_MEMBER_ENTRY_CHANGE_WAIT: Duration = Duration::from_secs(30);
    /// The event a member records when gossip reports a neighbor down.
    const NEIGHBOR_DOWN_EVENT: &str = "gossip.neighbor_down";
    /// The kind of a direct request that probes a peer's liveness.
    const PROBE_KIND: &str = "probe";
    /// The outcome of a request that failed, as an events line names it.
    const FAILED_OUTCOME: &str = "failed";
    /// The event a member records when gossip reports a neighbor up.
    const NEIGHBOR_UP_EVENT: &str = "gossip.neighbor_up";
    /// The status of an entry whose probe or request failed (spec 3.8).
    const UNREACHABLE_STATUS: &str = "unreachable";
    /// The cause an events line gives an entry a failed probe moved.
    const PROBE_FAILED_CAUSE: &str = "probe_failed";
    /// The event a member records when its table gains an entry.
    const ENTRY_CREATED_EVENT: &str = "peers.entry_created";
    /// The event a member records when an entry's status changes.
    const ENTRY_CHANGED_EVENT: &str = "peers.entry_changed";
    /// The event a member records for each direct request it serves.
    const REQUEST_SERVED_EVENT: &str = "direct.request_served";
    /// The cause an events line gives an entry a Departure moved.
    const DEPARTURE_CAUSE: &str = "departure";
    /// The status of a departed entry (spec 3.8).
    const GONE_STATUS: &str = "gone";
    /// The event a member records when its standard input ends (spec/observability.md decision 9).
    const INPUT_ENDED_EVENT: &str = "process.input_ended";
    /// The event a member records for each tool call it answers, and the field naming the tool (spec/observability.md decision 9).
    const TOOL_ANSWERED_EVENT: &str = "tool.answered";
    const TOOL_FIELD: &str = "tool";
    /// How long a member that has exited may take to be read for lines it wrote before: chosen, not measured, far above a read of a written file.
    const EVENTS_OF_AN_ENDED_MEMBER_WITHIN: Duration = Duration::from_secs(10);
    /// How long the recipient that holds a call's connection unserved may take to receive it: chosen, not measured, far above a loopback dial and below the 10 s a send waits for its ack.
    const CONNECTION_HELD_WITHIN: Duration = Duration::from_secs(5);
    /// The card name of the witness, a member started only to take the observer wait of its own claim.
    const WITNESS_CARD: &str = "witness-card";
    /// The handle the witness's card requests.
    const WITNESS_HANDLE: &str = "wit";

    /// A member, amy, who listed a scripted bob, then took bob's Departure of that seq and lists bob gone.
    struct ScriptedBobDeparted {
        amy: Member,
        amy_card: Card,
        bob: ScriptedNode,
        departure_seq: u64,
    }

    /// A member, amy, claimed through a scripted bob who announced a Presence and then a Departure, once amy lists bob gone.
    #[track_caller]
    fn amy_after_scripted_bob_departed(network: &LocalNetwork) -> ScriptedBobDeparted {
        let amy_card = network.create_card(AMY_CARD, AMY_HANDLE);
        let bob_key = SecretKey::generate();
        let bob = ScriptedNode::start(bob_key.clone(), *network.keys().gate_key(), network.topic());
        let mut amy = network.start_member();
        amy.init_through(&amy_card, &bob);
        bob.broadcast(presence_signed_by(
            network.keys(),
            &bob_key,
            BOB_HANDLE,
            &bob.endpoint().addr(),
        ));
        amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, |_listed| true);
        let departure_seq = wall_clock_ms();
        bob.broadcast(departure_signed_by(network.keys(), &bob_key, departure_seq));
        amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, gone);
        ScriptedBobDeparted {
            amy,
            amy_card,
            bob,
            departure_seq,
        }
    }

    /// A Departure of the signer's card carrying the seq, signed by the card and sealed under the network keys.
    fn departure_signed_by(keys: &NetworkKeys, signer: &SecretKey, seq: u64) -> Vec<u8> {
        let departure = Payload::Departure(Departure {
            v: Version::CURRENT,
            card: *signer.public().as_bytes(),
            seq,
            mac: UNSET_MAC,
            sig: UNSIGNED,
        });
        seal_payload(keys, signer, departure).unwrap()
    }

    /// A Message from the handle bob to the card, stamped with the send time and carrying the body.
    fn message_from_bob(to: EndpointId, sent_at: u64, body: &str) -> Request {
        Request::Message(Message {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            to: *to.as_bytes(),
            handle: BOB_HANDLE.to_owned(),
            attached: false,
            sent_at,
            body: body.to_owned(),
            mac: UNSET_MAC,
        })
    }

    /// Sends the request from the scripted node to the card directly and returns the opened response.
    #[track_caller]
    fn request_directly(
        network: &LocalNetwork,
        node: &ScriptedNode,
        to: EndpointId,
        request: Request,
    ) -> Response {
        let sealed = seal_request(network.keys(), request).unwrap();
        let answer = node.request_directly(to, &sealed).unwrap_or_else(|| {
            panic!("no answer to the direct request within {SPEC_DIRECT_TIMEOUT_WITH_SLACK:?}")
        });
        open_response(network.keys(), &answer).unwrap()
    }

    /// The node's id with each of its socket ports on the documentation host, an address no connection of this machine runs over.
    fn address_on_a_host_no_connection_runs_over(node: &ScriptedNode) -> iroh::EndpointAddr {
        let address = node.endpoint().addr();
        let sockets: Vec<TransportAddr> = address
            .ip_addrs()
            .map(|socket| TransportAddr::Ip(SocketAddr::new(DOCUMENTATION_HOST, socket.port())))
            .collect();
        iroh::EndpointAddr::from_parts(address.id, sockets)
    }

    /// Sends the request on the held connection and returns the opened response, or none when the exchange fails or no answer comes within the direct request wait.
    #[track_caller]
    fn exchange_on(
        network: &LocalNetwork,
        node: &ScriptedNode,
        connection: &Connection,
        request: Request,
    ) -> Option<Response> {
        let sealed = seal_request(network.keys(), request).unwrap();
        node.runtime().block_on(async {
            tokio::time::timeout(SPEC_DIRECT_TIMEOUT_WITH_SLACK, async {
                let (mut send, mut receive) = connection.open_bi().await.ok()?;
                send.write_all(&sealed).await.ok()?;
                send.finish().ok()?;
                let answer = receive.read_to_end(WIRE_LIMIT_BYTES).await.ok()?;
                open_response(network.keys(), &answer).ok()
            })
            .await
            .ok()
            .flatten()
        })
    }

    /// Broadcasts bob's Presence from the scripted node once its topic reports the member up, and requires the member to list bob as last seen in or after the millisecond of the broadcast.
    #[track_caller]
    fn announce_once_neighbor_up_until_seen(
        network: &LocalNetwork,
        node: &ScriptedNode,
        key: &SecretKey,
        address: &iroh::EndpointAddr,
        member: &mut Member,
        member_card: EndpointId,
    ) -> u64 {
        node.wait_for_neighbor_up(member_card);
        let sent_ms = wall_clock_ms();
        node.broadcast(presence_signed_by(network.keys(), key, BOB_HANDLE, address));
        member.peer_within(BOB_HANDLE, PRESENCE_WAIT, |bob| {
            bob["last_seen"].as_u64().unwrap() >= sent_ms
        });
        sent_ms
    }

    /// Waits until the member has applied bob's Presence of a seq at or after `sent_ms`: it took it and then replaced bob's lookup entry, the last step of applying it (spec 3.8).
    #[track_caller]
    fn presence_applied(member: &Member, sent_ms: u64) {
        member.wait_until(
            &format!("{LOOKUP_REPLACED_EVENT} for bob after its {PRESENCE_KIND} of seq {sent_ms} or later"),
            PRESENCE_WAIT,
            |lines| {
                let accepted = lines.iter().position(|line| {
                    line.event == PAYLOAD_ACCEPTED_EVENT
                        && line.value(KIND_FIELD) == Some(&json!(PRESENCE_KIND))
                        && line.value(HANDLE_FIELD) == Some(&json!(BOB_HANDLE))
                        && line.value(SEQ_FIELD).and_then(Value::as_u64) >= Some(sent_ms)
                })?;
                lines
                    .iter()
                    .skip(accepted)
                    .any(|line| {
                        line.event == LOOKUP_REPLACED_EVENT
                            && line.value(HANDLE_FIELD) == Some(&json!(BOB_HANDLE))
                    })
                    .then_some(())
            },
        );
    }

    /// Whether the response is a `MessageAck` that accepted the Message.
    fn is_accepted_message_ack(response: &Response) -> bool {
        matches!(response, Response::MessageAck(ack) if ack.accepted)
    }

    /// Waits until the member has bob as a gossip neighbor and has had an Introduce to bob acknowledged since its table listed bob, so that killing bob leaves no request to it in flight and only a neighbor-down event and a failed probe can set it unreachable.
    #[track_caller]
    fn wait_until_bob_is_a_neighbor_with_no_request_to_it_in_flight(
        member: &Member,
        bob_card: &Card,
    ) {
        let bob = short_id_of_node(bob_card.id);
        member.wait_until(
            &format!(
                "{NEIGHBOR_UP_EVENT} of {bob} and an {INTRODUCE_KIND} to it {ACKNOWLEDGED_OUTCOME} after the {ENTRY_CREATED_EVENT}"
            ),
            PRESENCE_WAIT,
            |lines| {
                let is_bob = |line: &TraceLine| line.value(PEER_FIELD) == Some(&json!(bob));
                let listed = lines
                    .iter()
                    .find(|line| line.event == ENTRY_CREATED_EVENT && is_bob(line))?
                    .n;
                let is_neighbor = lines
                    .iter()
                    .any(|line| line.event == NEIGHBOR_UP_EVENT && is_bob(line));
                let is_acknowledged_since = lines.iter().any(|line| {
                    line.event == REQUEST_SENT_EVENT
                        && line.n > listed
                        && line.value(KIND_FIELD) == Some(&json!(INTRODUCE_KIND))
                        && is_bob(line)
                        && line.value(OUTCOME_FIELD) == Some(&json!(ACKNOWLEDGED_OUTCOME))
                });
                (is_neighbor && is_acknowledged_since).then_some(())
            },
        );
    }

    /// The numbers of the lines of one run that record bob's neighbor-downs and failed probes before the line that sets bob unreachable by a failed probe, and the number of that line.
    #[derive(Debug, PartialEq, Eq)]
    struct DetectionOfBob {
        neighbor_downs: Vec<u64>,
        failed_probes: Vec<u64>,
        change: u64,
    }

    impl DetectionOfBob {
        /// Whether one neighbor-down came first, then one failed probe, then the change, and no other of either stands before the change (spec 3.15 test 5).
        fn is_one_neighbor_down_then_one_failed_probe_then_the_change(&self) -> bool {
            matches!(
                (self.neighbor_downs.as_slice(), self.failed_probes.as_slice()),
                (&[neighbor_down], &[probe]) if neighbor_down < probe && probe < self.change
            )
        }
    }

    /// What the member's lines hold of bob's detection, once a line sets bob unreachable by a failed probe.
    fn detection_of_bob(lines: &[TraceLine], bob: &str) -> Option<DetectionOfBob> {
        let is_bob = |line: &TraceLine| line.value(PEER_FIELD) == Some(&json!(bob));
        let change = lines.iter().find(|line| {
            line.event == ENTRY_CHANGED_EVENT
                && is_bob(line)
                && line.value("to") == Some(&json!(UNREACHABLE_STATUS))
                && line.value("cause") == Some(&json!(PROBE_FAILED_CAUSE))
        })?;
        let before_the_change = || {
            lines
                .iter()
                .filter(|line| line.run == change.run && line.n < change.n && is_bob(line))
        };
        Some(DetectionOfBob {
            neighbor_downs: before_the_change()
                .filter(|line| line.event == NEIGHBOR_DOWN_EVENT)
                .map(|line| line.n)
                .collect(),
            failed_probes: before_the_change()
                .filter(|line| {
                    line.event == REQUEST_SENT_EVENT
                        && line.value(KIND_FIELD) == Some(&json!(PROBE_KIND))
                        && line.value(OUTCOME_FIELD) == Some(&json!(FAILED_OUTCOME))
                })
                .map(|line| line.n)
                .collect(),
            change: change.n,
        })
    }

    /// Requires the member's lines to show one neighbor-down of bob, one failed probe to him, then the change of his entry to unreachable by that probe, none of them twice (spec 3.15 test 5).
    #[track_caller]
    fn require_one_neighbor_down_and_one_failed_probe_before_bob_is_unreachable(
        member: &Member,
        bob_card: &Card,
    ) {
        let bob = short_id_of_node(bob_card.id);
        let detection = member.wait_until(
            &format!(
                "{ENTRY_CHANGED_EVENT} of {bob} to {UNREACHABLE_STATUS} by {PROBE_FAILED_CAUSE}"
            ),
            KILLED_MEMBER_ENTRY_CHANGE_WAIT,
            |lines| detection_of_bob(&lines, &bob),
        );
        assert!(
            detection.is_one_neighbor_down_then_one_failed_probe_then_the_change(),
            "{detection:?}"
        );
    }

    /// Each status the member's table gave the handle from its Departure on, in order, once the member has served a direct request of that kind after the Departure: the Departure's own and every later one (spec 3.8).
    #[track_caller]
    fn statuses_from_the_departure_once_served(
        member: &Member,
        handle: &str,
        kind: &str,
    ) -> Vec<Value> {
        member.wait_until(
            &format!("{REQUEST_SERVED_EVENT} of a {kind} after {handle}'s departure"),
            SPEC_DIRECT_TIMEOUT_WITH_SLACK,
            |lines| {
                let departed = lines.iter().position(|line| {
                    line.event == ENTRY_CHANGED_EVENT
                        && line.value(HANDLE_FIELD) == Some(&json!(handle))
                        && line.value("to") == Some(&json!(GONE_STATUS))
                        && line.value("cause") == Some(&json!(DEPARTURE_CAUSE))
                })?;
                let since = &lines[departed..];
                since
                    .iter()
                    .any(|line| {
                        line.event == REQUEST_SERVED_EVENT
                            && line.value(KIND_FIELD) == Some(&json!(kind))
                    })
                    .then(|| {
                        since
                            .iter()
                            .filter(|line| line.value(HANDLE_FIELD) == Some(&json!(handle)))
                            .filter_map(|line| match line.event.as_str() {
                                ENTRY_CHANGED_EVENT => line.value("to").cloned(),
                                ENTRY_CREATED_EVENT => line.value("status").cloned(),
                                _ => None,
                            })
                            .collect()
                    })
            },
        )
    }

    /// Requires the send before the kill and the send to the restarted bob delivered, and the second acknowledged within 1 s of the new Presence reaching the sender (spec 3.15 test 18).
    #[track_caller]
    fn require_both_sends_delivered_the_second_within_1_s_of_the_new_presence(
        amy: &Member,
        first_send: &Value,
        sent: &Value,
        new_presence: &NewPresenceAccepted,
    ) {
        assert_eq!(
            first_send["result"]["delivered"],
            json!(true),
            "{first_send}"
        );
        assert_eq!(sent["result"]["delivered"], json!(true), "{sent}");
        assert_acknowledged_promptly_after(amy.events(), new_presence, sent);
    }

    /// Restarts bob's card through a second host that links amy, bob's first process gone since `killed_ms`, and requires amy's send before the kill delivered and her send to the restarted bob delivered within 1 s of its new Presence reaching her (spec 3.15 test 18).
    #[track_caller]
    fn restarted_bob_is_delivered_to_within_1_s_of_its_new_presence(
        network: &LocalNetwork,
        amy: &mut Member,
        amy_card: &Card,
        bob_card: &Card,
        first_send: &Value,
        killed_ms: u64,
    ) {
        let second_host = network.stand_in_host();
        timed("a second host links amy", || {
            second_host.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
        });
        let mut restarted = timed("the restarted bob starts", || network.start_member());
        timed("the restarted bob claims its card", || {
            restarted.init_through(bob_card, &second_host)
        });
        let new_presence = timed("amy accepts the restarted bob's new presence", || {
            new_presence_accepted(amy.events(), bob_card.id, killed_ms)
        });
        let sent = timed("amy's send to the restarted bob", || send(amy, BOB_HANDLE));
        require_both_sends_delivered_the_second_within_1_s_of_the_new_presence(
            amy,
            first_send,
            &sent,
            &new_presence,
        );
    }

    #[test]
    fn spec_3_15_test_18_a_killed_member_restarted_with_its_card_is_delivered_to_within_1_s_of_its_new_presence()
     {
        let network =
            LocalNetwork::create("restarted-member", &RunRecordKeeping::from_environment());
        let first_host = network.stand_in_host();
        let MetPair {
            mut amy,
            amy_card,
            mut bob,
            bob_card,
            ..
        } = timed("amy and bob list each other online", || {
            network.amy_and_bob_met(&first_host)
        });
        let first_send = timed("amy's send to bob before the kill", || {
            send(&mut amy, BOB_HANDLE)
        });

        bob.kill();
        let killed_ms = wall_clock_ms();
        restarted_bob_is_delivered_to_within_1_s_of_its_new_presence(
            &network,
            &mut amy,
            &amy_card,
            &bob_card,
            &first_send,
            killed_ms,
        );
    }

    #[test]
    fn spec_3_15_test_18_a_killed_member_restarted_with_its_card_is_delivered_to_within_1_s_of_its_new_presence_while_the_sender_still_holds_a_gossip_connection_to_its_killed_process()
     {
        let network =
            LocalNetwork::create("restart-gossip-held", &RunRecordKeeping::from_environment());
        let amy_card = network.create_card(AMY_CARD, AMY_HANDLE);
        let bob_card = network.create_card(BOB_CARD, BOB_HANDLE);
        let bob_key = network.card_key(&bob_card);
        let first_session = network.scripted_member(&bob_key, BOB_HANDLE);
        let mut amy = timed("amy starts", || network.start_member());
        timed(
            "amy claims its card through the first bob, who links amy over gossip",
            || amy.init_through(&amy_card, &first_session),
        );
        timed("the first bob's presence", || {
            first_session.broadcast(presence_signed_by(
                network.keys(),
                &bob_key,
                BOB_HANDLE,
                &loopback_address_of(&first_session),
            ));
        });
        timed("amy lists the first bob online", || {
            amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, online)
        });
        let first_send = timed("amy's send to bob before the kill", || {
            send(&mut amy, BOB_HANDLE)
        });

        timed("the first bob is killed with its connections open", || {
            first_session.vanish();
        });
        let killed_ms = wall_clock_ms();
        restarted_bob_is_delivered_to_within_1_s_of_its_new_presence(
            &network,
            &mut amy,
            &amy_card,
            &bob_card,
            &first_send,
            killed_ms,
        );
    }

    #[test]
    fn spec_3_15_test_18_a_killed_member_restarted_with_its_card_that_connects_to_the_sender_before_its_new_presence_is_delivered_to_within_1_s_of_that_presence()
     {
        let network =
            LocalNetwork::create("restart-dials-first", &RunRecordKeeping::from_environment());
        let amy_card = network.create_card(AMY_CARD, AMY_HANDLE);
        let bob_card = network.create_card(BOB_CARD, BOB_HANDLE);
        let bob_key = network.card_key(&bob_card);
        let first_session = network.scripted_member(&bob_key, BOB_HANDLE);
        let mut amy = timed("amy starts", || network.start_member());
        timed(
            "amy claims its card through the first bob, who links amy over gossip",
            || amy.init_through(&amy_card, &first_session),
        );
        timed("the first bob's presence", || {
            first_session.broadcast(presence_signed_by(
                network.keys(),
                &bob_key,
                BOB_HANDLE,
                &loopback_address_of(&first_session),
            ));
        });
        timed("amy lists the first bob online", || {
            amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, online)
        });
        let first_send = timed("amy's send to bob before the kill", || {
            send(&mut amy, BOB_HANDLE)
        });

        timed("the first bob is killed with its connections open", || {
            first_session.vanish();
        });
        let killed = Instant::now();
        let killed_ms = wall_clock_ms();
        let restarted = network.scripted_member(&bob_key, BOB_HANDLE);
        timed("the restarted bob connects to amy over gossip", || {
            restarted.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
            restarted.wait_for_neighbor_up(amy_card.id);
        });
        let witness_host = network.stand_in_host();
        let witness_card = network.create_card(WITNESS_CARD, WITNESS_HANDLE);
        let mut witness = network.start_member();
        timed(
            "a witness claims through its own host, which takes the spec 3.14 observer wait, past the silence of a gone process",
            || witness.init_through(&witness_card, &witness_host),
        );
        let silent_for = killed.elapsed();
        assert!(
            silent_for >= GONE_PROCESS_SILENCE.saturating_add(HEARING_SAMPLE_INTERVAL),
            "the first bob was silent only {silent_for:?} before the restarted bob's presence, under the {GONE_PROCESS_SILENCE:?} close bound plus the {HEARING_SAMPLE_INTERVAL:?} sample that judges it"
        );
        timed("the restarted bob's presence", || {
            restarted.broadcast(presence_signed_by(
                network.keys(),
                &bob_key,
                BOB_HANDLE,
                &loopback_address_of(&restarted),
            ));
        });
        let new_presence = timed("amy accepts the restarted bob's new presence", || {
            new_presence_accepted(amy.events(), bob_card.id, killed_ms)
        });
        let sent = timed("amy's send to the restarted bob", || {
            send(&mut amy, BOB_HANDLE)
        });

        require_both_sends_delivered_the_second_within_1_s_of_the_new_presence(
            &amy,
            &first_send,
            &sent,
            &new_presence,
        );
    }

    #[test]
    fn spec_3_15_test_4_a_member_stopped_by_the_end_of_input_is_gone_at_its_peer_within_1000_ms_and_unreachable()
     {
        let network =
            LocalNetwork::create("gone-by-input-end", &RunRecordKeeping::from_environment());
        let host = network.stand_in_host();
        let MetPair {
            mut amy, mut bob, ..
        } = network.amy_and_bob_met(&host);
        let first_send = send(&mut amy, BOB_HANDLE);

        let input_closed = Instant::now();
        bob.close_standard_input();

        assert_stopped_member_is_gone_then_unreachable(
            &mut amy,
            &mut bob,
            BOB_HANDLE,
            input_closed,
        );
        assert_eq!(
            first_send["result"]["delivered"],
            json!(true),
            "{first_send}"
        );
    }

    #[test]
    fn spec_3_15_test_4_a_member_stopped_with_a_tool_call_in_flight_is_gone_at_its_peer_within_1000_ms_and_unreachable()
     {
        let network = LocalNetwork::create(
            "gone-with-call-in-flight",
            &RunRecordKeeping::from_environment(),
        );
        let host = network.stand_in_host();
        let MetPair {
            mut amy,
            mut bob,
            bob_card,
            ..
        } = network.amy_and_bob_met(&host);
        let carol_key = SecretKey::generate();
        let (carol, held_from_bob) =
            network.scripted_member_holding_connections_of(&carol_key, CAROL_HANDLE, bob_card.id);
        host.broadcast(presence_signed_by(
            network.keys(),
            &carol_key,
            CAROL_HANDLE,
            &loopback_address_of(&carol),
        ));
        bob.peer_within(CAROL_HANDLE, PRESENCE_WAIT, online);

        bob.start_call(SEND_TOOL, &send_arguments(CAROL_HANDLE, BODY));
        held_from_bob.wait_for_one(&carol, CONNECTION_HELD_WITHIN);
        let input_closed = Instant::now();
        bob.close_standard_input();
        bob.wait_for_event(INPUT_ENDED_EVENT, EVENTS_OF_AN_ENDED_MEMBER_WITHIN);
        held_from_bob.release();

        assert_stopped_member_is_gone_then_unreachable(
            &mut amy,
            &mut bob,
            BOB_HANDLE,
            input_closed,
        );
        let (input_ended, answered) = bob.wait_until(
            &format!("{TOOL_ANSWERED_EVENT} of {SEND_TOOL} and {INPUT_ENDED_EVENT}"),
            EVENTS_OF_AN_ENDED_MEMBER_WITHIN,
            |lines| {
                let input_ended = lines.iter().find(|line| line.event == INPUT_ENDED_EVENT)?.n;
                let answered = lines
                    .iter()
                    .find(|line| {
                        line.event == TOOL_ANSWERED_EVENT
                            && line.value(TOOL_FIELD) == Some(&json!(SEND_TOOL))
                    })?
                    .n;
                Some((input_ended, answered))
            },
        );
        assert!(
            answered > input_ended,
            "the test's own setup failed, not the member: the call was answered at line {answered}, before the input ended at line {input_ended}, though the recipient held it unanswered"
        );
    }

    #[test]
    fn spec_3_15_test_5_a_killed_member_is_unreachable_at_its_peer_within_one_neighbor_down_and_one_probe_timeout()
     {
        let network =
            LocalNetwork::create("killed-unreachable", &RunRecordKeeping::from_environment());
        let host = network.stand_in_host();
        network.add_stand_in_host_to_the_configuration(&host);
        let MetPair {
            mut amy,
            mut bob,
            bob_card,
            ..
        } = network.amy_and_bob_met(&host);
        wait_until_bob_is_a_neighbor_with_no_request_to_it_in_flight(&amy, &bob_card);

        bob.kill();

        require_one_neighbor_down_and_one_failed_probe_before_bob_is_unreachable(&amy, &bob_card);
        amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, unreachable);
        let sent = amy.send(BOB_HANDLE, BODY);
        assert_eq!(sent["error"]["code"], json!(UNREACHABLE_CODE), "{sent}");
    }

    #[test]
    fn spec_3_15_test_5_one_neighbor_down_then_one_failed_probe_then_the_change_meets_the_count() {
        let lines = [
            neighbor_down_line(1, BOB_FIXTURE),
            failed_probe_line(2, BOB_FIXTURE),
            probe_failed_change_line(3, BOB_FIXTURE),
        ];

        let detection = detection_of_bob(&lines, BOB_FIXTURE).unwrap();

        assert_eq!(
            detection,
            DetectionOfBob {
                neighbor_downs: vec![1],
                failed_probes: vec![2],
                change: 3
            }
        );
        assert!(detection.is_one_neighbor_down_then_one_failed_probe_then_the_change());
    }

    #[test]
    fn spec_3_15_test_5_a_second_neighbor_down_before_the_change_fails_the_count() {
        let lines = [
            neighbor_down_line(1, BOB_FIXTURE),
            neighbor_down_line(2, BOB_FIXTURE),
            failed_probe_line(3, BOB_FIXTURE),
            probe_failed_change_line(4, BOB_FIXTURE),
        ];

        let detection = detection_of_bob(&lines, BOB_FIXTURE).unwrap();

        assert!(!detection.is_one_neighbor_down_then_one_failed_probe_then_the_change());
    }

    #[test]
    fn spec_3_15_test_5_a_second_failed_probe_before_the_change_fails_the_count() {
        let lines = [
            neighbor_down_line(1, BOB_FIXTURE),
            failed_probe_line(2, BOB_FIXTURE),
            failed_probe_line(3, BOB_FIXTURE),
            probe_failed_change_line(4, BOB_FIXTURE),
        ];

        let detection = detection_of_bob(&lines, BOB_FIXTURE).unwrap();

        assert!(!detection.is_one_neighbor_down_then_one_failed_probe_then_the_change());
    }

    #[test]
    fn spec_3_15_test_5_a_change_that_precedes_the_probe_fails_the_count() {
        let lines = [
            neighbor_down_line(1, BOB_FIXTURE),
            probe_failed_change_line(2, BOB_FIXTURE),
            failed_probe_line(3, BOB_FIXTURE),
        ];

        let detection = detection_of_bob(&lines, BOB_FIXTURE).unwrap();

        assert!(!detection.is_one_neighbor_down_then_one_failed_probe_then_the_change());
    }

    #[test]
    fn spec_3_15_test_5_a_probe_that_precedes_the_neighbor_down_fails_the_count() {
        let lines = [
            failed_probe_line(1, BOB_FIXTURE),
            neighbor_down_line(2, BOB_FIXTURE),
            probe_failed_change_line(3, BOB_FIXTURE),
        ];

        let detection = detection_of_bob(&lines, BOB_FIXTURE).unwrap();

        assert!(!detection.is_one_neighbor_down_then_one_failed_probe_then_the_change());
    }

    #[test]
    fn spec_3_15_test_5_lines_of_another_peer_and_no_change_are_not_counted() {
        let lines = [
            neighbor_down_line(1, HOST_FIXTURE),
            neighbor_down_line(2, BOB_FIXTURE),
            failed_probe_line(3, HOST_FIXTURE),
            failed_probe_line(4, BOB_FIXTURE),
        ];

        assert_eq!(detection_of_bob(&lines, BOB_FIXTURE), None);
        let with_the_change = [
            neighbor_down_line(1, HOST_FIXTURE),
            neighbor_down_line(2, BOB_FIXTURE),
            failed_probe_line(3, HOST_FIXTURE),
            failed_probe_line(4, BOB_FIXTURE),
            probe_failed_change_line(5, BOB_FIXTURE),
        ];
        let detection = detection_of_bob(&with_the_change, BOB_FIXTURE).unwrap();
        assert!(detection.is_one_neighbor_down_then_one_failed_probe_then_the_change());
    }

    /// The peer of the fixture lines the count is asked about.
    const BOB_FIXTURE: &str = "b0b0b0b0b0";
    /// A peer of the fixture lines the count is not asked about.
    const HOST_FIXTURE: &str = "4057404057";

    /// A line recording gossip's neighbor-down of the peer.
    fn neighbor_down_line(n: u64, peer: &str) -> TraceLine {
        crate::built_trace_line(
            crate::BUILT_RUN,
            n,
            NEIGHBOR_DOWN_EVENT,
            &json!({ PEER_FIELD: peer }),
        )
    }

    /// A line recording a failed probe request to the peer.
    fn failed_probe_line(n: u64, peer: &str) -> TraceLine {
        crate::built_trace_line(
            crate::BUILT_RUN,
            n,
            REQUEST_SENT_EVENT,
            &json!({ PEER_FIELD: peer, KIND_FIELD: PROBE_KIND, OUTCOME_FIELD: FAILED_OUTCOME }),
        )
    }

    /// A line recording the peer's entry set unreachable by a failed probe.
    fn probe_failed_change_line(n: u64, peer: &str) -> TraceLine {
        crate::built_trace_line(
            crate::BUILT_RUN,
            n,
            ENTRY_CHANGED_EVENT,
            &json!({ PEER_FIELD: peer, "to": UNREACHABLE_STATUS, "cause": PROBE_FAILED_CAUSE }),
        )
    }

    #[test]
    fn spec_3_15_test_4_a_message_sent_before_the_departure_amy_holds_reaches_her_inbox_and_leaves_its_sender_gone()
     {
        let network = LocalNetwork::create("late-message", &RunRecordKeeping::from_environment());
        let ScriptedBobDeparted {
            mut amy,
            amy_card,
            bob,
            departure_seq,
        } = amy_after_scripted_bob_departed(&network);

        let sent_before = request_directly(
            &network,
            &bob,
            amy_card.id,
            message_from_bob(
                amy_card.id,
                departure_seq.checked_sub(1).unwrap(),
                SENT_BEFORE_THE_DEPARTURE,
            ),
        );
        let statuses_after_the_earlier_message =
            statuses_from_the_departure_once_served(&amy, BOB_HANDLE, MESSAGE_KIND);
        let bodies = amy.inbox_bodies();
        let sent_after = request_directly(
            &network,
            &bob,
            amy_card.id,
            message_from_bob(
                amy_card.id,
                departure_seq.checked_add(1).unwrap(),
                SENT_AFTER_THE_DEPARTURE,
            ),
        );
        amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, online);

        assert!(is_accepted_message_ack(&sent_before), "{sent_before:?}");
        assert_eq!(statuses_after_the_earlier_message, [json!("gone")]);
        assert_eq!(bodies, [json!(SENT_BEFORE_THE_DEPARTURE)]);
        assert!(is_accepted_message_ack(&sent_after), "{sent_after:?}");
    }

    #[test]
    fn spec_3_15_test_4_an_introduce_from_a_member_amy_holds_departed_leaves_it_gone() {
        let network = LocalNetwork::create("late-introduce", &RunRecordKeeping::from_environment());
        let ScriptedBobDeparted {
            amy, amy_card, bob, ..
        } = amy_after_scripted_bob_departed(&network);
        let bob_address = bob.endpoint().addr();

        let answer = request_directly(
            &network,
            &bob,
            amy_card.id,
            Request::Introduce(Introduce {
                v: Version::CURRENT,
                handle: Some(BOB_HANDLE.to_owned()),
                public: b"{}".to_vec(),
                attached: false,
                addr: Addr::from_endpoint_report(
                    bob_address.relay_urls().map(ToString::to_string),
                    bob_address.ip_addrs().copied(),
                ),
                mac: UNSET_MAC,
            }),
        );
        let statuses_after_the_introduce =
            statuses_from_the_departure_once_served(&amy, BOB_HANDLE, INTRODUCE_KIND);

        assert!(matches!(answer, Response::IntroduceAck(_)), "{answer:?}");
        assert_eq!(statuses_after_the_introduce, [json!("gone")]);
    }

    #[test]
    fn spec_3_15_test_18_a_restarted_member_whose_message_came_before_its_presence_keeps_that_connection_and_is_delivered_to()
     {
        let network =
            LocalNetwork::create("restart-kept-link", &RunRecordKeeping::from_environment());
        let amy_card = network.create_card(AMY_CARD, AMY_HANDLE);
        let bob_key = SecretKey::generate();
        let first_session = network.scripted_member(&bob_key, BOB_HANDLE);
        let mut amy = timed("amy starts", || network.start_member());
        timed("amy claims its card through the first bob", || {
            amy.init_through(&amy_card, &first_session)
        });
        timed("the first bob's presence", || {
            first_session.broadcast(presence_signed_by(
                network.keys(),
                &bob_key,
                BOB_HANDLE,
                &loopback_address_of(&first_session),
            ));
        });
        timed("amy lists the first bob online", || {
            amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, online)
        });
        drop(first_session);

        let restarted = network.scripted_member(&bob_key, BOB_HANDLE);
        timed("the restarted bob links amy", || {
            restarted.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
        });
        let held = timed("the restarted bob connects to amy", || {
            restarted
                .runtime()
                .block_on(restarted.endpoint().connect(amy_card.id, DIRECT_ALPN))
                .unwrap()
        });
        let before_presence = timed("the restarted bob's message before its presence", || {
            exchange_on(
                &network,
                &restarted,
                &held,
                message_from_bob(amy_card.id, wall_clock_ms(), SENT_BEFORE_THE_NEW_PRESENCE),
            )
        });
        let sent_ms = timed("amy lists the restarted bob's new presence", || {
            announce_once_neighbor_up_until_seen(
                &network,
                &restarted,
                &bob_key,
                &loopback_address_of(&restarted),
                &mut amy,
                amy_card.id,
            )
        });
        timed("amy applies the new presence", || {
            presence_applied(&amy, sent_ms);
        });
        let after_presence = timed("the restarted bob's message after its presence", || {
            exchange_on(
                &network,
                &restarted,
                &held,
                message_from_bob(amy_card.id, wall_clock_ms(), SENT_AFTER_THE_NEW_PRESENCE),
            )
        });
        let bodies = timed("amy's inbox tool answers", || amy.inbox_bodies());
        let sent = timed("amy's send to the restarted bob", || {
            send(&mut amy, BOB_HANDLE)
        });

        assert!(
            before_presence
                .as_ref()
                .is_some_and(is_accepted_message_ack),
            "the restarted bob's message before its presence was not acked accepted within {SPEC_DIRECT_TIMEOUT_WITH_SLACK:?}: {before_presence:?}"
        );
        assert!(
            after_presence.as_ref().is_some_and(is_accepted_message_ack),
            "the restarted bob's message after its presence was not acked accepted within {SPEC_DIRECT_TIMEOUT_WITH_SLACK:?}: {after_presence:?}"
        );
        assert_eq!(
            bodies,
            [
                json!(SENT_BEFORE_THE_NEW_PRESENCE),
                json!(SENT_AFTER_THE_NEW_PRESENCE)
            ]
        );
        assert_eq!(sent["result"]["delivered"], json!(true), "{sent}");
    }

    #[test]
    fn spec_3_8_and_3_15_test_18_a_live_member_whose_presence_omits_the_address_its_connection_runs_over_keeps_that_connection_and_is_answered()
     {
        let network = LocalNetwork::create(
            "live-unannounced-path",
            &RunRecordKeeping::from_environment(),
        );
        let amy_card = network.create_card(AMY_CARD, AMY_HANDLE);
        let bob_key = SecretKey::generate();
        let bob = network.scripted_member(&bob_key, BOB_HANDLE);
        let mut amy = timed("amy starts", || network.start_member());
        timed("amy claims its card through bob", || {
            amy.init_through(&amy_card, &bob)
        });
        let held = timed("bob connects to amy", || {
            bob.runtime()
                .block_on(bob.endpoint().connect(amy_card.id, DIRECT_ALPN))
                .unwrap()
        });
        let before_presence = timed("bob's message before its presence", || {
            exchange_on(
                &network,
                &bob,
                &held,
                message_from_bob(amy_card.id, wall_clock_ms(), SENT_BEFORE_THE_PRESENCE),
            )
        });
        let exchanged = Instant::now();
        let witness_host = network.stand_in_host();
        let witness_card = network.create_card(WITNESS_CARD, WITNESS_HANDLE);
        let mut witness = network.start_member();
        timed(
            "a witness claims through its own host, which takes the spec 3.14 observer wait, past the silence that judges a connection gone",
            || witness.init_through(&witness_card, &witness_host),
        );
        let held_for = exchanged.elapsed();
        assert!(
            held_for >= GONE_PROCESS_SILENCE.saturating_add(HEARING_SAMPLE_INTERVAL),
            "the held connection carried no message only {held_for:?} before bob's presence, under the {GONE_PROCESS_SILENCE:?} close bound plus the {HEARING_SAMPLE_INTERVAL:?} sample that judges it"
        );

        let sent_ms = timed("amy lists bob's presence omitting the held path", || {
            announce_once_neighbor_up_until_seen(
                &network,
                &bob,
                &bob_key,
                &address_on_a_host_no_connection_runs_over(&bob),
                &mut amy,
                amy_card.id,
            )
        });
        timed("amy applies the presence", || {
            presence_applied(&amy, sent_ms);
        });
        let after_presence = timed("bob's message after its presence", || {
            exchange_on(
                &network,
                &bob,
                &held,
                message_from_bob(amy_card.id, wall_clock_ms(), SENT_AFTER_THE_PRESENCE),
            )
        });

        assert!(
            before_presence
                .as_ref()
                .is_some_and(is_accepted_message_ack),
            "live bob's message before its presence was not acked accepted within {SPEC_DIRECT_TIMEOUT_WITH_SLACK:?}: {before_presence:?}"
        );
        assert!(
            after_presence.as_ref().is_some_and(is_accepted_message_ack),
            "live bob's message after its presence was not acked accepted within {SPEC_DIRECT_TIMEOUT_WITH_SLACK:?}: {after_presence:?}"
        );
    }
}

#[cfg(test)]
mod spec_3_15_test_1_on_one_machine {
    use std::time::Duration;

    use serde_json::json;

    use crate::inbox_client::{FRAME_WAIT, InboxClient, NO_FURTHER_FRAME_WINDOW};
    use crate::local_network::{AMY_HANDLE, BOB_HANDLE, LocalNetwork, MetPair};
    use crate::message_body::BODY;
    use crate::payload_placeholders::wall_clock_ms;
    use crate::scratch_machine::RunRecordKeeping;
    use crate::send;

    /// The event a member records when a client connects to its inbox socket (spec/observability.md decision 9).
    const CLIENT_CONNECTED_EVENT: &str = "inbox.client_connected";
    /// How long an inbox socket may take to record a client that connected: chosen, not measured, far above a socket's accept.
    const CLIENT_CONNECTED_WITHIN: Duration = Duration::from_secs(10);

    #[test]
    fn spec_3_15_test_1_a_message_to_a_member_whose_inbox_socket_has_a_client_is_delivered_attached_as_one_frame()
     {
        let network = LocalNetwork::create("t1", &RunRecordKeeping::from_environment());
        let host = network.stand_in_host();
        let MetPair {
            mut amy,
            bob,
            bob_claim,
            ..
        } = network.amy_and_bob_met(&host);
        let mut inbox = InboxClient::connect(&bob_claim["result"]["inbox_path"], 0);
        bob.wait_for_event(CLIENT_CONNECTED_EVENT, CLIENT_CONNECTED_WITHIN);

        let sent = send(&mut amy, BOB_HANDLE);

        let id = sent["result"]["id"].as_str().unwrap();
        assert_eq!(
            sent["result"],
            json!({ "id": id, "delivered": true, "attached": true })
        );
        let frame = inbox.next_frame(FRAME_WAIT);
        bob.wait_for_time_to_pass(wall_clock_ms(), NO_FURTHER_FRAME_WINDOW);
        assert_eq!(frame["seq"], json!(1));
        assert_eq!(frame["id"], json!(id));
        assert_eq!(frame["from"], json!(AMY_HANDLE));
        assert_eq!(frame["body"], json!(BODY));
        assert_eq!(inbox.frame_already_emitted(), None);
    }
}

#[cfg(test)]
mod spec_3_15_test_19_on_one_machine {
    use std::collections::BTreeSet;
    use std::time::Instant;

    use iroh::{EndpointId, SecretKey};
    use iroh_gossip::api::Event;

    use crate::SPEC_OBSERVER_WAIT;
    use crate::local_network::{CAROL_HANDLE, LocalNetwork, MetPair};
    use crate::mcp_server::online;
    use crate::member::{loopback_address, loopback_address_of};
    use crate::payloads::presence_signed_by;
    use crate::scratch_machine::RunRecordKeeping;
    use crate::scripted_node::ScriptedNode;

    /// Every node the node's topic ever reported up as a gossip neighbor.
    fn nodes_ever_gossip_neighbors(node: &ScriptedNode) -> BTreeSet<EndpointId> {
        node.events()
            .into_iter()
            .filter_map(|event| {
                if let Event::NeighborUp(neighbor) = event {
                    Some(neighbor)
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn spec_3_15_test_19_a_member_lists_a_node_whose_only_gossip_neighbor_is_another_member_within_the_observer_wait_of_its_presence()
     {
        let network = LocalNetwork::create("t19", &RunRecordKeeping::from_environment());
        let host = network.stand_in_host();
        let MetPair {
            mut amy,
            bob,
            bob_card,
            ..
        } = network.amy_and_bob_met(&host);
        let carol_key = SecretKey::generate();
        let carol = network.scripted_member(&carol_key, CAROL_HANDLE);
        carol.admit(loopback_address(bob_card.id, &bob.bound_udp_ports()));
        carol.wait_for_neighbor_up(bob_card.id);

        carol.broadcast(presence_signed_by(
            network.keys(),
            &carol_key,
            CAROL_HANDLE,
            &loopback_address_of(&carol),
        ));
        let presence_sent = Instant::now();
        amy.peer_within(CAROL_HANDLE, SPEC_OBSERVER_WAIT, online);
        let listed_after = presence_sent.elapsed();
        let carol_gossip_neighbors = nodes_ever_gossip_neighbors(&carol);

        assert!(listed_after <= SPEC_OBSERVER_WAIT, "{listed_after:?}");
        assert_eq!(carol_gossip_neighbors, BTreeSet::from([bob_card.id]));
    }
}

#[cfg(test)]
mod spec_3_15_joining_through_a_host_without_a_card_on_one_machine {
    use std::time::Instant;

    use serde_json::json;

    use crate::SPEC_OBSERVER_WAIT;
    use crate::local_network::{AMY_CARD, AMY_HANDLE, BOB_CARD, BOB_HANDLE, LocalNetwork, MetPair};
    use crate::mcp_server::{PRESENCE_WAIT, gone, online};
    use crate::member::{Member, loopback_address};
    use crate::payload_placeholders::wall_clock_ms;
    use crate::scratch_machine::{Card, RunRecordKeeping};
    use crate::timed_step::timed;

    /// Spec 3.15 test 7: the handle the first member holds and the second member's card requests too.
    const TAKEN: &str = AMY_HANDLE;
    /// Spec 3.15 test 7: what follows the taken handle in the effective handle `<h>-2` every node gives the second member that requests it (spec 3.1).
    const SECOND_MEMBER_HANDLE_SUFFIX: &str = "-2";

    #[test]
    fn spec_3_15_test_7_a_second_member_joining_through_a_host_without_a_card_after_the_first_spread_is_h_2_at_every_node_and_keeps_it_after_the_first_restarts()
     {
        let network = LocalNetwork::create("t7c", &RunRecordKeeping::from_environment());
        let second_effective = format!("{TAKEN}{SECOND_MEMBER_HANDLE_SUFFIX}");
        let host = network.stand_in_host();
        let amy_card = network.create_card(AMY_CARD, TAKEN);
        let bob_card = network.create_card(BOB_CARD, BOB_HANDLE);
        let ann_card = network.create_card("ann-card", TAKEN);
        let mut amy = timed("amy joins and claims its card", || {
            let mut amy = network.start_member();
            amy.init_through(&amy_card, &host);
            amy
        });
        let mut bob = timed("bob joins and claims its card", || {
            let mut bob = network.start_member();
            bob.init_through(&bob_card, &host);
            bob
        });
        timed("amy and bob list each other online", || {
            amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, online);
            bob.peer_within(TAKEN, PRESENCE_WAIT, online);
        });

        let mut ann = network.start_member();
        let ann_claim = timed(
            "ann joins through the host without a card and claims a card requesting amy's handle",
            || ann.init_through(&ann_card, &host),
        );
        let at_amy = timed("amy lists ann online as <h>-2", || {
            amy.peer_within(&second_effective, PRESENCE_WAIT, online)
        });
        let at_bob = timed("bob lists ann online as <h>-2", || {
            bob.peer_within(&second_effective, PRESENCE_WAIT, online)
        });
        amy.close_standard_input();
        assert!(timed("amy exits after the end of input", || amy.wait_for_exit()).success());
        let second_host = network.stand_in_host();
        timed("a second host links ann and bob", || {
            second_host.admit(loopback_address(ann_card.id, &ann.bound_udp_ports()));
            second_host.admit(loopback_address(bob_card.id, &bob.bound_udp_ports()));
            second_host.wait_for_neighbor_up(ann_card.id);
            second_host.wait_for_neighbor_up(bob_card.id);
        });
        let mut restarted = network.start_member();
        let restarted_claim = timed("the restarted amy claims its card", || {
            restarted.init_through(&amy_card, &second_host)
        });
        let restarted_at_ann = timed("ann lists the restarted amy online as <h>", || {
            ann.peer_within(TAKEN, PRESENCE_WAIT, online)
        });
        let own_after_restart = timed("ann's peers tool answers", || ann.call("peers", &json!({})));
        let restarted_at_bob = timed("bob lists the restarted amy online as <h>", || {
            bob.peer_within(TAKEN, PRESENCE_WAIT, online)
        });
        let ann_at_restarted = timed("the restarted amy lists ann online as <h>-2", || {
            restarted.peer_within(&second_effective, PRESENCE_WAIT, online)
        });
        let ann_at_bob_after_restart = timed(
            "bob still lists ann online as <h>-2 after the restart",
            || bob.peer_within(&second_effective, PRESENCE_WAIT, online),
        );

        assert_eq!(ann_claim["result"]["handle"], json!(second_effective));
        assert_eq!(at_amy["handle"], json!(second_effective));
        assert_eq!(at_bob["handle"], json!(second_effective));
        assert_eq!(restarted_claim["result"]["handle"], json!(TAKEN));
        assert_eq!(restarted_at_ann["handle"], json!(TAKEN));
        assert_eq!(restarted_at_bob["handle"], json!(TAKEN));
        assert_eq!(ann_at_restarted["handle"], json!(second_effective));
        assert_eq!(ann_at_bob_after_restart["handle"], json!(second_effective));
        assert_eq!(own_after_restart["you"]["handle"], json!(second_effective));
    }

    /// Restarts bob's card through a second host that links amy alone, bob's first process gone since `gone_ms`, and requires the restarted bob to list amy online within the observer wait of its init returning (spec 3.15 test 26).
    #[track_caller]
    fn restarted_bob_lists_amy_within_the_observer_wait_of_its_init(
        network: &LocalNetwork,
        amy: &mut Member,
        amy_card: &Card,
        bob_card: &Card,
        gone_ms: u64,
    ) {
        let second_host = network.stand_in_host();
        timed("a second host links amy", || {
            second_host.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
            second_host.wait_for_neighbor_up(amy_card.id);
        });
        let mut restarted = network.start_member();
        timed("the restarted bob claims its card", || {
            restarted.init_through(bob_card, &second_host)
        });
        let init_returned = Instant::now();
        timed(
            "amy lists the restarted bob online, seen after it went",
            || {
                amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, |bob| {
                    online(bob) && bob["last_seen"].as_u64().unwrap() > gone_ms
                })
            },
        );
        timed(
            "the restarted bob lists amy online within the observer wait of its init",
            || {
                restarted.peer_within(
                    AMY_HANDLE,
                    SPEC_OBSERVER_WAIT.saturating_sub(init_returned.elapsed()),
                    online,
                )
            },
        );
        let listed_after = init_returned.elapsed();

        assert!(listed_after <= SPEC_OBSERVER_WAIT, "{listed_after:?}");
    }

    #[test]
    fn spec_3_15_test_26_a_member_killed_and_restarted_through_another_host_lists_every_present_member_within_the_observer_wait_of_its_init()
     {
        let network = LocalNetwork::create("t26k", &RunRecordKeeping::from_environment());
        let first_host = network.stand_in_host();
        let MetPair {
            mut amy,
            amy_card,
            mut bob,
            bob_card,
            ..
        } = timed("amy and bob list each other online", || {
            network.amy_and_bob_met(&first_host)
        });

        bob.kill();
        let killed_ms = wall_clock_ms();
        restarted_bob_lists_amy_within_the_observer_wait_of_its_init(
            &network, &mut amy, &amy_card, &bob_card, killed_ms,
        );
    }

    #[test]
    fn spec_3_15_test_26_a_member_stopped_cleanly_and_restarted_through_another_host_lists_every_present_member_within_the_observer_wait_of_its_init()
     {
        let network = LocalNetwork::create("t26s", &RunRecordKeeping::from_environment());
        let first_host = network.stand_in_host();
        let MetPair {
            mut amy,
            amy_card,
            mut bob,
            bob_card,
            ..
        } = timed("amy and bob list each other online", || {
            network.amy_and_bob_met(&first_host)
        });

        bob.close_standard_input();
        assert!(timed("bob exits after the end of input", || bob.wait_for_exit()).success());
        let stopped_ms = wall_clock_ms();
        timed("amy lists bob gone", || {
            amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, gone)
        });
        restarted_bob_lists_amy_within_the_observer_wait_of_its_init(
            &network, &mut amy, &amy_card, &bob_card, stopped_ms,
        );
    }
}

#[cfg(test)]
mod spec_3_15_with_one_member {
    use std::time::{Duration, Instant};

    use estafette::crypto::{NetworkKeys, NetworkSecret};
    use estafette::payload::{open_response, seal_request};
    use estafette::wire::{self, Message, Payload, Request, Response, Version};
    use iroh::{EndpointAddr, EndpointId, SecretKey};
    use iroh_gossip::api::Event;
    use serde_json::{Value, json};

    use crate::MESSAGE_ID;
    use crate::events_lines::{
        PAYLOAD_DROPPED_EVENT, PEER_FIELD, PRESENCE_KIND, is_dropped_payload_of_seq, short_id_text,
    };
    use crate::local_network::{AMY_CARD, AMY_HANDLE, LocalNetwork};
    use crate::mcp_server::{INIT_TOOL, PRESENCE_WAIT, call_succeeded, init_arguments};
    use crate::member::{Member, loopback_address};
    use crate::member_events::COUNTERS_EVENT;
    use crate::message_body::BODY;
    use crate::node_start::{MOMENT_FIELD, is_counters_snapshot_of_moment};
    use crate::payload_placeholders::{UNSET_MAC, wall_clock_ms};
    use crate::payloads::{presence_signed_by, presence_signed_by_at};
    use crate::scratch_machine::RunRecordKeeping;
    use crate::scripted_node::{SPEC_DIRECT_TIMEOUT_WITH_SLACK, ScriptedNode};
    use crate::trace_reader::TraceLine;

    /// Spec 3.15 test 6: the two inits are written within this long of each other.
    const SPEC_SIMULTANEOUS_INIT_WINDOW: Duration = Duration::from_millis(100);
    /// The six MCP tools of spec 3.10, in the order the tool list gives them.
    const SPEC_TOOL_NAMES: [&str; 6] =
        ["list_cards", "init", "peers", "send", "broadcast", "inbox"];
    /// The gate timeout of spec 3.14 (3000 ms) with slack: how long a gate closing may take to be recorded.
    const SPEC_GATE_TIMEOUT_WITH_SLACK: Duration = Duration::from_secs(5);
    /// The event a member records for each gossip connection its gate closes (spec/observability.md decision 9).
    const GATE_CLOSED_EVENT: &str = "transport.gate_closed";
    /// The field of a gate closing naming the side that opened the connection.
    const DIRECTION_FIELD: &str = "direction";
    /// The side of a connection the peer opened.
    const ACCEPTED_DIRECTION: &str = "accepted";
    /// The side of a connection the member opened.
    const DIALED_DIRECTION: &str = "dialed";
    /// The deaf timeout of spec 3.14 (15 s) and the rebind it starts, with slack.
    const SPEC_DEAF_TIMEOUT_AND_REBIND_WITH_SLACK: Duration = Duration::from_secs(30);
    /// The member join wait of spec 3.14 (1500 ms) and the join it ends, with slack.
    const SPEC_MEMBER_JOIN_WAIT_WITH_SLACK: Duration = Duration::from_secs(10);
    /// The moment of the counters a member records once its rebind's fresh endpoint is bound (spec/observability.md decision 9).
    const REBOUND_MOMENT: &str = "rebound";
    /// The sidecar mode spec 3.15 test 16 gives a card, readable by group and others.
    const LOOSE_SIDECAR_MODE: u32 = 0o644;
    /// The body of the Message sealed under another network's keys (spec 3.15 test 9).
    const FORGED_BODY: &str = "a message under a wrong mac";
    /// The body of the Message sealed under the network's keys, which reaches the inbox (spec 3.15 test 9).
    const HONEST_BODY: &str = "a message under the network mac";
    /// The handle the Messages of the stand-in host carry (spec 3.15 test 9).
    const ATTACKER_HANDLE: &str = "mallory";
    /// The handle the node holding the topic id but not the secret announces through the host (spec 3.15 test 21).
    const OUTSIDER_HANDLE: &str = "outsider";
    /// Every byte of the secret of a network other than the test's, whose keys a member refuses.
    const OUTSIDER_SECRET_BYTE: u8 = 77;

    /// The `you` of spec 3.10 for a session that has claimed no card.
    fn unclaimed() -> Value {
        json!({ "card": null, "handle": null, "status": "unclaimed" })
    }

    /// One call of each of the six tools, with arguments that name the card for init.
    fn every_tool_call(card: &str) -> [(&'static str, Value); 6] {
        [
            ("list_cards", json!({})),
            ("init", json!({ "card": card })),
            ("peers", json!({})),
            ("send", json!({ "to": "nobody", "body": BODY })),
            ("broadcast", json!({ "to": ["nobody"], "body": BODY })),
            ("inbox", json!({})),
        ]
    }

    /// The handles the member's peers tool lists, in its order.
    #[track_caller]
    fn listed_handles(member: &mut Member) -> Vec<Value> {
        member.call("peers", &json!({}))["result"]["peers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|peer| peer["handle"].clone())
            .collect()
    }

    /// A Message from the attacker's handle to the card, stamped now and carrying the body.
    fn message_to(card: &EndpointId, body: &str) -> Request {
        Request::Message(Message {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            to: *card.as_bytes(),
            handle: ATTACKER_HANDLE.to_owned(),
            attached: false,
            sent_at: wall_clock_ms(),
            body: body.to_owned(),
            mac: UNSET_MAC,
        })
    }

    /// The sealed Presence with its handle replaced after it was signed and its mac made anew under the network keys, so only its signature disagrees.
    fn renamed_after_signing(keys: &NetworkKeys, sealed: &[u8], handle: &str) -> Vec<u8> {
        let Payload::Presence(mut presence) = wire::decode::<Payload>(sealed).unwrap() else {
            panic!("a sealed Presence decodes as a Presence");
        };
        handle.clone_into(&mut presence.handle);
        presence.mac = UNSET_MAC;
        presence.mac = *keys
            .mac(&wire::encode(&Payload::Presence(presence.clone())).unwrap())
            .as_bytes();
        wire::encode(&Payload::Presence(presence)).unwrap()
    }

    /// The counters line the member records once the endpoint its rebind bound is in place, awaited within the wait.
    #[track_caller]
    fn rebound_snapshot(member: &Member, within: Duration) -> TraceLine {
        member.wait_until(
            &format!("{COUNTERS_EVENT} of its rebind's {REBOUND_MOMENT} endpoint"),
            within,
            |lines| {
                lines
                    .into_iter()
                    .find(|line| is_counters_snapshot_of_moment(line, REBOUND_MOMENT))
            },
        )
    }

    /// The UDP ports the member holds now that it has recorded its rebound counters, so the fresh endpoint's port is among them even where it takes a port a socket held before the rebind.
    #[track_caller]
    fn ports_held_once_rebound(member: &Member) -> Vec<u16> {
        let held = member.ipv4_udp_ports();
        assert!(
            !held.is_empty(),
            "the member holds no IPv4 UDP port once its rebind's fresh endpoint is bound"
        );
        held
    }

    /// Requires the member to record, within the wait, that its gate closed a connection of the peer that the side opened, in a line written after the one given when one is given.
    #[track_caller]
    fn gate_closed_connection_of(
        member: &Member,
        peer: &str,
        direction: &str,
        written_after: Option<&TraceLine>,
        within: Duration,
    ) {
        let after = written_after.map_or_else(String::new, |earlier| {
            format!(
                " written after its line {} of run {}",
                earlier.n, earlier.run
            )
        });
        member.wait_until(
            &format!("a {GATE_CLOSED_EVENT} of the {direction} connection of {peer}{after}"),
            within,
            |lines| holds_gate_closing_of(&lines, peer, direction, written_after).then_some(()),
        );
    }

    /// Whether the lines hold a gate closing of a connection of the peer that the side opened, written after the earlier line when one is given.
    fn holds_gate_closing_of(
        lines: &[TraceLine],
        peer: &str,
        direction: &str,
        written_after: Option<&TraceLine>,
    ) -> bool {
        lines.iter().any(|line| {
            is_gate_closing_of(line, peer, direction)
                && written_after.is_none_or(|earlier| is_written_after(line, earlier))
        })
    }

    /// Whether the events line was written after the earlier line of the same run.
    fn is_written_after(line: &TraceLine, earlier: &TraceLine) -> bool {
        line.run == earlier.run && line.n > earlier.n
    }

    /// A gate closing line of the peer and direction.
    fn gate_closing_line(run: &str, n: u64, peer: &str, direction: &str) -> TraceLine {
        crate::built_trace_line(
            run,
            n,
            GATE_CLOSED_EVENT,
            &json!({ PEER_FIELD: peer, DIRECTION_FIELD: direction }),
        )
    }

    /// The counters line of the rebound moment, numbered `n` in the run.
    fn rebound_line(run: &str, n: u64) -> TraceLine {
        crate::built_trace_line(
            run,
            n,
            COUNTERS_EVENT,
            &json!({ MOMENT_FIELD: REBOUND_MOMENT }),
        )
    }

    #[test]
    fn spec_3_15_test_21_a_dialed_closing_written_only_before_the_rebound_line_does_not_meet_the_bound()
     {
        let rebound = rebound_line(crate::BUILT_RUN, 10);
        let lines = [
            gate_closing_line(crate::BUILT_RUN, 9, "feedfeedfe", DIALED_DIRECTION),
            rebound_line(crate::BUILT_RUN, 10),
        ];

        assert!(!holds_gate_closing_of(
            &lines,
            "feedfeedfe",
            DIALED_DIRECTION,
            Some(&rebound)
        ));
        assert!(holds_gate_closing_of(
            &lines,
            "feedfeedfe",
            DIALED_DIRECTION,
            None
        ));
    }

    #[test]
    fn spec_3_15_test_21_a_dialed_closing_written_after_the_rebound_line_meets_the_bound() {
        let rebound = rebound_line(crate::BUILT_RUN, 10);
        let lines = [
            gate_closing_line(crate::BUILT_RUN, 9, "feedfeedfe", DIALED_DIRECTION),
            rebound_line(crate::BUILT_RUN, 10),
            gate_closing_line(crate::BUILT_RUN, 11, "feedfeedfe", DIALED_DIRECTION),
        ];

        assert!(holds_gate_closing_of(
            &lines,
            "feedfeedfe",
            DIALED_DIRECTION,
            Some(&rebound)
        ));
    }

    #[test]
    fn spec_3_15_test_21_a_closing_of_another_peer_side_or_run_after_the_bound_does_not_meet_it() {
        let rebound = rebound_line(crate::BUILT_RUN, 10);
        let lines = [
            gate_closing_line(crate::BUILT_RUN, 11, "0000000000", DIALED_DIRECTION),
            gate_closing_line(crate::BUILT_RUN, 12, "feedfeedfe", ACCEPTED_DIRECTION),
            gate_closing_line("another-run", 13, "feedfeedfe", DIALED_DIRECTION),
        ];

        assert!(!holds_gate_closing_of(
            &lines,
            "feedfeedfe",
            DIALED_DIRECTION,
            Some(&rebound)
        ));
    }

    /// Whether the events line records the member's gate closing a connection of the peer that the side opened.
    fn is_gate_closing_of(line: &TraceLine, peer: &str, direction: &str) -> bool {
        line.event == GATE_CLOSED_EVENT
            && line.value(PEER_FIELD) == Some(&json!(peer))
            && line.value(DIRECTION_FIELD) == Some(&json!(direction))
    }

    #[test]
    fn spec_3_15_test_6_two_processes_init_one_card_within_100_ms_and_exactly_one_claims_it() {
        let network = LocalNetwork::create("t6", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let mut first = network.start_member();
        let mut second = network.start_member();

        let first_written = Instant::now();
        let first_init = first.start_call("init", &json!({ "card": card.name }));
        let second_init = second.start_call("init", &json!({ "card": card.name }));
        let between_inits = first_written.elapsed();
        let answers = [first.answer(&first_init), second.answer(&second_init)];

        assert!(between_inits < SPEC_SIMULTANEOUS_INIT_WINDOW);
        let claimed: Vec<&Value> = answers
            .iter()
            .filter(|answer| answer["ok"] == json!(true))
            .collect();
        let refused: Vec<&Value> = answers
            .iter()
            .filter(|answer| answer["ok"] == json!(false))
            .collect();
        assert_eq!(claimed.len(), 1, "{answers:?}");
        assert_eq!(claimed[0]["result"]["handle"], json!(AMY_HANDLE));
        assert_eq!(claimed[0]["you"]["status"], json!("claimed"));
        assert_eq!(refused.len(), 1, "{answers:?}");
        assert_eq!(refused[0]["error"]["code"], json!("card_held"));
        assert_eq!(refused[0]["you"], unclaimed());
    }

    #[test]
    fn spec_3_15_test_9_a_message_with_a_wrong_mac_is_dropped_with_no_response() {
        let network = LocalNetwork::create("t9", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let attacker = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &attacker);
        let outsider_keys =
            NetworkKeys::derive(&NetworkSecret::from_bytes([OUTSIDER_SECRET_BYTE; 32]));

        let forged_sent = Instant::now();
        let forged_answer = attacker.request_directly(
            card.id,
            &seal_request(&outsider_keys, message_to(&card.id, FORGED_BODY)).unwrap(),
        );
        let forged_ended_after = forged_sent.elapsed();
        let honest_answer = attacker
            .request_directly(
                card.id,
                &seal_request(network.keys(), message_to(&card.id, HONEST_BODY)).unwrap(),
            )
            .unwrap_or_else(|| {
                panic!("no answer to the honest message within {SPEC_DIRECT_TIMEOUT_WITH_SLACK:?}")
            });
        let bodies = amy.inbox_bodies();

        assert_eq!(forged_answer, None);
        assert!(
            forged_ended_after < SPEC_DIRECT_TIMEOUT_WITH_SLACK,
            "the forged message's stream ended when its wait of {SPEC_DIRECT_TIMEOUT_WITH_SLACK:?} ran out, not when the member ended it"
        );
        let Response::MessageAck(ack) = open_response(network.keys(), &honest_answer).unwrap()
        else {
            panic!("a Message is answered by a MessageAck");
        };
        assert!(ack.accepted);
        assert_eq!(bodies, [json!(HONEST_BODY)]);
    }

    #[test]
    fn spec_3_15_test_10_a_gossip_payload_whose_signature_does_not_verify_against_its_card_is_dropped()
     {
        let network = LocalNetwork::create("t10", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        let forger = SecretKey::generate();
        let forger_peer = short_id_text(&forger);
        let forged_seq = wall_clock_ms();
        let honest = SecretKey::generate();
        let nowhere = EndpointAddr::from_parts(forger.public(), []);

        host.broadcast(renamed_after_signing(
            network.keys(),
            &presence_signed_by_at(network.keys(), &forger, "genuine", &nowhere, forged_seq),
            "forged",
        ));
        host.broadcast(presence_signed_by(
            network.keys(),
            &honest,
            "honest",
            &nowhere,
        ));

        amy.peer_within("honest", PRESENCE_WAIT, |_honest| true);
        amy.wait_until(
            &format!("{PAYLOAD_DROPPED_EVENT} of the forged {PRESENCE_KIND} signed by {forger_peer} with seq {forged_seq}"),
            PRESENCE_WAIT,
            |lines| {
                lines
                    .iter()
                    .any(|line| is_dropped_payload_of_seq(line, PRESENCE_KIND, &forger_peer, forged_seq))
                    .then_some(())
            },
        );
        assert_eq!(listed_handles(&mut amy), [json!("honest")]);
    }

    #[test]
    fn spec_3_15_test_12_the_mcp_tool_list_is_identical_before_and_after_init() {
        let network = LocalNetwork::create("t12", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let mut amy = network.start_member();

        let before = amy.request("tools/list", &json!({}));
        let claim = amy.call("init", &json!({ "card": card.name }));
        let after = amy.request("tools/list", &json!({}));

        assert_eq!(claim["ok"], json!(true), "{claim}");
        assert_eq!(before, after);
        let names: Vec<&str> = before["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, SPEC_TOOL_NAMES);
    }

    #[test]
    fn spec_3_15_test_13_every_tool_response_contains_you_with_the_current_status_before_and_after_init()
     {
        let network = LocalNetwork::create("t13", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let mut amy = network.start_member();
        let claimed = json!({ "card": card.name, "handle": AMY_HANDLE, "status": "claimed" });

        let before: Vec<Value> = every_tool_call("no-such-card")
            .into_iter()
            .map(|(tool, arguments)| amy.call(tool, &arguments))
            .collect();
        let claim = amy.call("init", &json!({ "card": card.name }));
        let after: Vec<Value> = every_tool_call(&card.name)
            .into_iter()
            .map(|(tool, arguments)| amy.call(tool, &arguments))
            .collect();

        for envelope in &before {
            assert_eq!(envelope["you"], unclaimed(), "{envelope}");
        }
        assert_eq!(claim["you"], claimed);
        for envelope in &after {
            assert_eq!(envelope["you"], claimed, "{envelope}");
        }
    }

    #[test]
    fn spec_3_15_test_16_a_card_whose_sidecar_has_permissions_0644_fails_init_with_sidecar_invalid()
    {
        let network = LocalNetwork::create("t16", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        network.set_sidecar_mode(&card, LOOSE_SIDECAR_MODE);
        let mut amy = network.start_member();

        let refused = amy.call("init", &json!({ "card": card.name }));

        assert_eq!(refused["ok"], json!(false), "{refused}");
        assert_eq!(refused["error"]["code"], json!("sidecar_invalid"));
        assert_eq!(refused["you"], unclaimed());
    }

    #[test]
    fn spec_3_5_a_card_whose_public_key_decodes_from_upper_case_hex_is_claimed_through_init_though_spec_3_1_writes_it_lowercase()
     {
        let network = LocalNetwork::create("upper-key", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        network.uppercase_public_key(&card);
        let mut amy = network.start_member();

        let claim = amy.call(INIT_TOOL, &init_arguments(&card.name));

        assert!(call_succeeded(&claim), "{claim}");
        assert_eq!(
            claim["you"],
            json!({ "card": card.name, "handle": AMY_HANDLE, "status": "claimed" })
        );
    }

    #[test]
    fn spec_3_15_test_21_a_node_with_the_topic_id_but_not_the_secret_gets_no_neighbor_and_no_payload_dialing_or_dialed()
     {
        let network = LocalNetwork::create("t21", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        network.add_host_that_never_answers();
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        let ports_before_rebind = amy.bound_udp_ports();
        let outsider_key = SecretKey::generate();
        let outsider = ScriptedNode::start_without_gate(outsider_key.clone(), network.topic());

        outsider.admit(loopback_address(card.id, &ports_before_rebind));
        let outsider_peer = short_id_text(&outsider_key);
        gate_closed_connection_of(
            &amy,
            &outsider_peer,
            ACCEPTED_DIRECTION,
            None,
            SPEC_GATE_TIMEOUT_WITH_SLACK,
        );
        host.broadcast(presence_signed_by(
            network.keys(),
            &outsider_key,
            OUTSIDER_HANDLE,
            &outsider.endpoint().addr(),
        ));
        amy.peer_within(OUTSIDER_HANDLE, PRESENCE_WAIT, |_outsider| true);
        let rebound_line = rebound_snapshot(&amy, SPEC_DEAF_TIMEOUT_AND_REBIND_WITH_SLACK);
        let rebound = ports_held_once_rebound(&amy);
        host.admit(loopback_address(card.id, &rebound));
        gate_closed_connection_of(
            &amy,
            &outsider_peer,
            DIALED_DIRECTION,
            Some(&rebound_line),
            SPEC_MEMBER_JOIN_WAIT_WITH_SLACK,
        );

        assert_eq!(outsider.events(), Vec::<Event>::new());
    }
}

#[cfg(test)]
mod spec_3_16_absences_on_one_machine {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use estafette::crypto::NetworkKeys;
    use estafette::payload::open_request;
    use estafette::wire::{self, Payload, Request, WIRE_LIMIT_BYTES};
    use iroh::SecretKey;
    use iroh::endpoint::Connection;
    use iroh::protocol::{AcceptError, ProtocolHandler};
    use iroh_gossip::api::Event;
    use serde_json::{Value, json};

    use crate::local_network::{
        AMY_CARD, AMY_HANDLE, BOB_HANDLE, CAROL_HANDLE, LocalNetwork, MetPair,
    };
    use crate::mcp_server::{PRESENCE_WAIT, gone, online};
    use crate::member::{loopback_address, loopback_address_of};
    use crate::message_body::BODY;
    use crate::payload_placeholders::wall_clock_ms;
    use crate::payloads::presence_signed_by;
    use crate::scratch_machine::RunRecordKeeping;
    use crate::scripted_node::{SPEC_DIRECT_TIMEOUT_WITH_SLACK, ScriptedNode};
    use crate::send;

    /// Longer than every interval spec 3.14 names below the announcement max age: the 2000 ms re-announce interval, the 15 s deaf timeout and the 20 s rebind interval.
    const PRESENCE_QUIET_WINDOW: Duration = Duration::from_secs(30);
    /// Time for the address set a member announces to settle after its first Presence, beyond the 2000 ms re-announce interval.
    const ADDRESS_SETTLE: Duration = Duration::from_secs(5);
    /// Time after a failed send in which a retry would reach the receiver.
    const RETRY_WATCH: Duration = Duration::from_secs(5);
    /// Thrice the ten sends per sixty seconds the Claude Code bridge allows itself, so any limit of that order in the binary refuses some.
    const SENDS_IN_A_ROW: usize = 30;
    /// Time for a message held for an absent member to reach it once it returns.
    const REDELIVERY_WATCH: Duration = Duration::from_secs(5);

    /// The files under the base directories whose content holds the text.
    fn base_directory_files_holding(network: &LocalNetwork, text: &str) -> Vec<PathBuf> {
        network
            .base_directory_files()
            .into_iter()
            .filter(|file| {
                fs::read(file).is_ok_and(|content| String::from_utf8_lossy(&content).contains(text))
            })
            .collect()
    }

    /// How many Presences of the card the node's topic has delivered so far.
    fn presences_received_from(node: &ScriptedNode, card: wire::PublicKey) -> usize {
        presences_of(&node.events(), card)
    }

    /// How many of the topic's events are Presences of the card.
    fn presences_of(events: &[Event], card: wire::PublicKey) -> usize {
        events
            .iter()
            .filter(|event| {
                if let Event::Received(message) = event {
                    matches!(wire::decode::<Payload>(&message.content), Ok(Payload::Presence(presence)) if presence.card == card)
                } else {
                    false
                }
            })
            .count()
    }

    /// A scripted member's peer that reads every direct request and answers none, counting the Messages among them.
    #[derive(Debug)]
    struct AnsweringNothing {
        keys: NetworkKeys,
        messages_read: Arc<AtomicUsize>,
    }

    impl ProtocolHandler for AnsweringNothing {
        async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
            while let Ok((send, mut receive)) = connection.accept_bi().await {
                let closed = connection.clone();
                tokio::spawn(async move {
                    let _unanswered_until_the_connection_closes = send;
                    closed.closed().await;
                });
                if let Ok(request) = receive.read_to_end(WIRE_LIMIT_BYTES).await
                    && matches!(open_request(&self.keys, &request), Ok(Request::Message(_)))
                {
                    self.messages_read.fetch_add(1, Ordering::SeqCst);
                }
            }
            Ok(())
        }
    }

    #[test]
    fn spec_3_16_no_file_under_the_base_directories_announces_the_inbox_socket() {
        let network = LocalNetwork::create("n1", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let host = network.stand_in_host();
        let mut amy = network.start_member();

        let claim = amy.init_through(&card, &host);

        let socket = PathBuf::from(claim["result"]["inbox_path"].as_str().unwrap());
        let socket_name = socket.file_name().unwrap().to_string_lossy().into_owned();
        let listed_names: Vec<String> = network
            .base_directory_files()
            .iter()
            .map(|file| file.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(listed_names.contains(&socket_name), "{listed_names:?}");
        assert_eq!(
            base_directory_files_holding(&network, &socket_name),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn spec_3_16_a_received_message_is_not_in_the_inbox_of_the_next_process_on_the_card_nor_on_disk()
     {
        let network = LocalNetwork::create("n2", &RunRecordKeeping::from_environment());
        let host = network.stand_in_host();
        let MetPair {
            mut amy,
            mut bob,
            bob_card,
            ..
        } = network.amy_and_bob_met(&host);
        let delivered = send(&mut amy, BOB_HANDLE);
        let before_exit = bob.inbox_bodies();
        bob.close_standard_input();
        assert!(bob.wait_for_exit().success());

        let mut restarted = network.start_member();
        restarted.init_through(&bob_card, &host);

        assert_eq!(delivered["result"]["delivered"], json!(true), "{delivered}");
        assert_eq!(before_exit, [json!(BODY)]);
        assert_eq!(restarted.inbox_bodies(), Vec::<Value>::new());
        assert_eq!(
            base_directory_files_holding(&network, BODY),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn spec_3_16_a_message_to_an_absent_member_is_not_kept_for_its_return() {
        let network = LocalNetwork::create("n3", &RunRecordKeeping::from_environment());
        let host = network.stand_in_host();
        let MetPair {
            mut amy,
            amy_card,
            mut bob,
            bob_card,
            ..
        } = network.amy_and_bob_met(&host);
        bob.close_standard_input();
        assert!(bob.wait_for_exit().success());
        amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, gone);
        let stopped_ms = wall_clock_ms();

        let refused = send(&mut amy, BOB_HANDLE);
        let second_host = network.stand_in_host();
        second_host.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
        let mut restarted = network.start_member();
        restarted.init_through(&bob_card, &second_host);
        amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, |bob| {
            online(bob) && bob["last_seen"].as_u64().unwrap() > stopped_ms
        });
        restarted.wait_for_time_to_pass(wall_clock_ms(), REDELIVERY_WATCH);

        assert_eq!(refused["error"]["code"], json!("unreachable"), "{refused}");
        assert_eq!(restarted.inbox_bodies(), Vec::<Value>::new());
    }

    #[test]
    fn spec_3_16_a_member_with_nothing_to_announce_sends_no_presence_for_longer_than_every_interval_of_spec_3_14()
     {
        let network = LocalNetwork::create("n4", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        host.wait_for_events("presence from amy", PRESENCE_WAIT, |events| {
            presences_of(events, *card.id.as_bytes()) > 0
        });
        amy.wait_for_time_to_pass(wall_clock_ms(), ADDRESS_SETTLE);

        let settled = presences_received_from(&host, *card.id.as_bytes());
        amy.wait_for_time_to_pass(wall_clock_ms(), PRESENCE_QUIET_WINDOW);
        let after_window = presences_received_from(&host, *card.id.as_bytes());

        assert_eq!(after_window, settled);
    }

    #[test]
    fn spec_3_16_a_send_whose_ack_never_comes_reaches_the_receiver_once() {
        let network = LocalNetwork::create("n5", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        let messages_read = Arc::new(AtomicUsize::new(0));
        let carol_key = SecretKey::generate();
        let carol = ScriptedNode::start_answering_direct(
            carol_key.clone(),
            *network.keys().gate_key(),
            network.topic(),
            |_endpoint| AnsweringNothing {
                keys: NetworkKeys::clone(network.keys()),
                messages_read: Arc::clone(&messages_read),
            },
        );
        host.broadcast(presence_signed_by(
            network.keys(),
            &carol_key,
            CAROL_HANDLE,
            &loopback_address_of(&carol),
        ));
        amy.peer_within(CAROL_HANDLE, PRESENCE_WAIT, |_carol| true);

        let sent_at = Instant::now();
        let sent = send(&mut amy, CAROL_HANDLE);
        let answered_after = sent_at.elapsed();
        amy.wait_for_time_to_pass(wall_clock_ms(), RETRY_WATCH);

        assert_eq!(messages_read.load(Ordering::SeqCst), 1);
        assert_eq!(sent["ok"], json!(false), "{sent}");
        assert!(
            answered_after <= SPEC_DIRECT_TIMEOUT_WITH_SLACK,
            "{answered_after:?}"
        );
    }

    #[test]
    fn spec_3_16_thirty_sends_in_a_row_are_all_delivered() {
        let network = LocalNetwork::create("n6", &RunRecordKeeping::from_environment());
        let card = network.create_card(AMY_CARD, AMY_HANDLE);
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        let carol_key = SecretKey::generate();
        let carol = network.scripted_member(&carol_key, CAROL_HANDLE);
        host.broadcast(presence_signed_by(
            network.keys(),
            &carol_key,
            CAROL_HANDLE,
            &loopback_address_of(&carol),
        ));
        amy.peer_within(CAROL_HANDLE, PRESENCE_WAIT, online);

        let outcomes: Vec<Value> = (0..SENDS_IN_A_ROW)
            .map(|_| send(&mut amy, CAROL_HANDLE)["result"]["delivered"].clone())
            .collect();

        assert_eq!(outcomes, vec![json!(true); SENDS_IN_A_ROW]);
    }
}

#[cfg(test)]
mod kept_run_records {
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::ExitStatus;

    use serde_json::json;

    use crate::local_network::{BOB_HANDLE, LocalNetwork, MetPair, NETWORK};
    use crate::scratch_machine::fresh_directory::fresh_directory_in;
    use crate::scratch_machine::remove_scratch::remove_scratch;
    use crate::scratch_machine::{REDACTED, RunRecordKeeping, ScratchMachine};
    use crate::send;

    /// The members the real-record test starts: amy, bob and the refused one.
    const MEMBERS_STARTED: usize = 3;
    /// The field the refused member's configuration lacks, as the binary's refusal names it.
    const REFUSED_FIELD: &str = "`hosts`";
    /// The configuration lines the binary's own file keeps once its hosts are taken out.
    const KEPT_CONFIGURATION_FIELDS: [&str; 2] = ["secret", "owner"];
    /// A network secret chosen by this test, in the shape the configuration file takes, not a real secret.
    const CHOSEN_SECRET: &str = "5ec2e7c0ffee5ec2e7c0ffee5ec2e7c0ffee5ec2e7c0ffee5ec2e7c0ffee5ec2";
    /// An owner key in the shape the configuration file takes, not a real key.
    const CHOSEN_OWNER: &str = "0aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1";
    /// A configuration line chosen by this test with no run of letters and digits as long as a key's.
    const CHOSEN_SHORT_LINE: &str = "# acme lab, set up by hand";
    /// What the planted text puts before a key it quotes apart from the key's configuration line.
    const KEY_QUOTED_APART_FROM_ITS_LINE: &str = "refused key ";

    /// Writes the network configuration of the lines in order, with no `hosts`, which the binary refuses.
    fn write_configuration_without_hosts(machine: &ScratchMachine, lines: &[&str]) {
        let config_file = machine.directories().network_config_file(NETWORK).unwrap();
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        fs::write(&config_file, lines.join("\n")).unwrap();
    }

    /// Rewrites the configuration file the binary wrote with its secret and owner lines alone, so it lacks the `hosts` field the binary requires.
    fn keep_only_the_secret_and_owner_lines(config_file: &Path) {
        let kept_lines: Vec<String> = fs::read_to_string(config_file)
            .unwrap()
            .lines()
            .filter(|line| {
                KEPT_CONFIGURATION_FIELDS
                    .iter()
                    .any(|field| line.starts_with(field))
            })
            .map(str::to_owned)
            .collect();
        assert_eq!(kept_lines.len(), KEPT_CONFIGURATION_FIELDS.len());
        fs::write(config_file, kept_lines.join("\n")).unwrap();
    }

    /// Starts the binary's MCP server on the machine and requires it to exit refused.
    fn start_refused(machine: &ScratchMachine) {
        assert!(!machine.spawn_mcp_server().wait().unwrap().success());
    }

    /// Appends text the test plants to every run record of the machine, as a refusal quoting the configuration would print it.
    fn plant_in_every_run_record(machine: &ScratchMachine, lines: &[&str]) {
        for record in machine.run_records().unwrap() {
            let mut file = fs::OpenOptions::new().append(true).open(record).unwrap();
            file.write_all(lines.join("\n").as_bytes()).unwrap();
        }
    }

    /// The content of each record the machine keeps in the directory, which is removed once read.
    fn kept_contents(machine: &ScratchMachine, directory: &Path) -> Vec<String> {
        let kept = machine.keep_run_records(directory).unwrap();
        let contents = kept
            .iter()
            .map(|record| fs::read_to_string(record).unwrap())
            .collect();
        remove_scratch(directory).unwrap();
        contents
    }

    /// Records of members that met, exchanged a message and stopped, and of a member the binary refused for its configuration's missing field, are kept as the binary wrote them, the redaction rule finding no line or key of the machine's configuration and key files in any.
    #[test]
    fn run_records_of_members_that_ran_and_of_a_refused_member_are_kept_unredacted_holding_no_secret()
     {
        let network = LocalNetwork::create("kept-real", &RunRecordKeeping::from_environment());
        let host = network.stand_in_host();
        let MetPair {
            mut amy, mut bob, ..
        } = network.amy_and_bob_met(&host);
        let sent = send(&mut amy, BOB_HANDLE);
        let received = bob.inbox_bodies();
        amy.close_standard_input();
        bob.close_standard_input();
        let exits = [amy.wait_for_exit(), bob.wait_for_exit()];
        let machine = network.machine();
        let config_file = machine.directories().network_config_file(NETWORK).unwrap();
        keep_only_the_secret_and_owner_lines(&config_file);
        start_refused(machine);
        let kept_in = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-real-record-test-{}", std::process::id()),
        )
        .unwrap();

        let written: Vec<(PathBuf, String)> = machine
            .run_records()
            .unwrap()
            .into_iter()
            .map(|record| {
                let text = fs::read_to_string(&record).unwrap();
                (record, text)
            })
            .collect();
        let kept = kept_contents(machine, &kept_in);

        assert_eq!(sent["result"]["delivered"], json!(true), "{sent}");
        assert_eq!(received.len(), 1, "{received:?}");
        assert!(exits.iter().all(ExitStatus::success), "{exits:?}");
        assert_eq!(written.len(), MEMBERS_STARTED);
        assert_eq!(kept.len(), MEMBERS_STARTED);
        assert!(
            written.iter().any(
                |(_, text)| text.contains(&config_file.display().to_string())
                    && text.contains(REFUSED_FIELD)
            ),
            "no record names the refused configuration file and its missing field"
        );
        for ((record, as_written), as_kept) in written.iter().zip(&kept) {
            assert!(
                as_kept == as_written,
                "{} as kept differs from it as written: the binary wrote a secret of the machine there",
                record.display()
            );
        }
    }

    /// Text this test plants in a refused member's run record, the binary having written none of it: a short configuration line, the secret's whole line and the owner key quoted apart from its line; the kept record holds the redaction marker in place of each.
    #[test]
    fn a_kept_record_holds_none_of_the_configuration_lines_or_keys_the_test_planted_in_its_run_record()
     {
        let machine = ScratchMachine::fresh(
            "kept-record",
            NETWORK,
            &RunRecordKeeping::from_environment(),
        );
        let secret_line = format!("secret = \"{CHOSEN_SECRET}\"");
        let owner_line = format!("owner = \"{CHOSEN_OWNER}\"");
        let owner_quoted_apart = format!("{KEY_QUOTED_APART_FROM_ITS_LINE}{CHOSEN_OWNER}");
        write_configuration_without_hosts(
            &machine,
            &[CHOSEN_SHORT_LINE, &secret_line, &owner_line],
        );
        start_refused(&machine);
        plant_in_every_run_record(
            &machine,
            &[CHOSEN_SHORT_LINE, &secret_line, &owner_quoted_apart],
        );
        let kept_in = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-record-test-{}", std::process::id()),
        )
        .unwrap();

        let records = kept_contents(&machine, &kept_in);

        assert_eq!(records.len(), 1);
        for record in &records {
            assert!(
                !record.contains(CHOSEN_SHORT_LINE),
                "a kept record holds the short configuration line"
            );
            assert!(
                !record.contains(CHOSEN_SECRET),
                "a kept record holds the secret"
            );
            assert!(
                !record.contains(CHOSEN_OWNER),
                "a kept record holds the owner key quoted apart from its line"
            );
            assert!(record.contains(REDACTED), "{record}");
        }
    }
}

#[cfg(test)]
mod observability_decision_6_on_one_machine {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::io::Write;
    use std::net::SocketAddr;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde_json::{Value, json};

    use crate::local_network::{AMY_CARD, AMY_HANDLE, BOB_CARD, BOB_HANDLE, LocalNetwork, MetPair};
    use crate::mcp_server::{PRESENCE_WAIT, call_succeeded, online};
    use crate::member::LOOPBACK;
    use crate::scratch_machine::RunRecordKeeping;

    /// The body amy sends bob, planted so an events file that holds it is found to write a body (decision 6).
    const BODY_MARKER: &str = "body-marker-q7x2v9";
    /// A body no other text of the run holds, so finding it anywhere means the body was written.
    const MARKER_BODY: &str = "marker body 7c1e quartz lantern 93fd";
    /// The members whose runs write an events file: amy and bob.
    const MEMBER_RUNS: usize = 2;
    /// The start of the name of every path event of the transport area (decision 9).
    const PATH_EVENT_PREFIX: &str = "transport.path_";
    /// The end of the name of a run's events file (decision 4).
    const SPEC_EVENTS_SUFFIX: &str = ".events.jsonl";
    /// The directory under the data base directory that holds the trace files (decision 4).
    const SPEC_TRACES_UNDER_DATA_HOME: &str = "estafette/traces";
    /// The hex characters of a 32-byte key, secret or whole card id.
    const SPEC_KEY_HEX_CHARS: usize = 64;
    /// The shortest run of letters and digits taken for a secret, a key, a mac or an invitation in a file.
    const SHORTEST_SECRET_RUN: usize = 16;
    /// The decimal groups of an IPv4 address written as a dotted quad.
    const DOTTED_QUAD_GROUPS: usize = 4;
    /// The most digits a group of a dotted quad holds.
    const LONGEST_DOTTED_QUAD_GROUP: usize = 3;
    /// The extension of an inbox socket file, `<runtime>/<pid>.sock` (spec 3.3).
    const SOCKET_EXTENSION: &str = "sock";
    /// What every URL holds, a relay URL included, so no events file may hold it (decision 6).
    const URL_SCHEME_SEPARATOR: &str = "://";
    /// The text of the IPv6 loopback address, which no events file may hold (decision 6).
    const IPV6_LOOPBACK: &str = "::1";
    /// How long bob's inbox may take to return amy's message.
    const DELIVERY_WAIT: Duration = Duration::from_secs(20);
    /// The first line an inbox client writes to read every frame from the first (spec 3.11).
    const SPEC_SINCE_EVERY_FRAME: &str = "{\"since\": 0}\n";
    /// How long a member may take to record that the planted inbox client went away.
    const CLIENT_WAIT: Duration = Duration::from_secs(20);
    /// The event names the exchange writes, each quoted as an events line holds it, so the scan is proved to read lines of every area the exchange touches.
    const EVENTS_THIS_EXCHANGE_RECORDS: [&str; 19] = [
        "\"gossip.payload_accepted\"",
        "\"gossip.payload_broadcast\"",
        "\"peers.entry_created\"",
        "\"peers.own_handle_assigned\"",
        "\"peers.lookup_replaced\"",
        "\"process.started\"",
        "\"process.claimed\"",
        "\"process.input_ended\"",
        "\"process.departure_handed_over\"",
        "\"process.lock_released\"",
        "\"process.configuration_rewrite_decided\"",
        "\"process.held_connections_dropped\"",
        "\"process.departure_wait_over\"",
        "\"process.exited\"",
        "\"inbox.frame_delivered\"",
        "\"inbox.client_connected\"",
        "\"inbox.client_gone\"",
        "\"tool.answered\"",
        "\"tool.send_answered\"",
    ];

    /// The runs of letters and digits in the text long enough to be a key, a secret, a mac or an invitation.
    fn long_alphanumeric_runs(text: &str) -> impl Iterator<Item = &str> {
        text.split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|run| run.len() >= SHORTEST_SECRET_RUN)
    }

    /// Whether the text holds a run of 64 hexadecimal characters, the length of a key, a secret or a whole card id written in hex.
    fn holds_a_key_length_hex_run(text: &str) -> bool {
        text.split(|character: char| !character.is_ascii_hexdigit())
            .any(|run| run.len() >= SPEC_KEY_HEX_CHARS)
    }

    /// Whether the text holds four decimal groups joined by dots, the form of an IPv4 address.
    fn holds_a_dotted_quad(text: &str) -> bool {
        text.split(|character: char| !character.is_ascii_digit() && character != '.')
            .any(|token| {
                token.split('.').count() == DOTTED_QUAD_GROUPS
                    && token
                        .split('.')
                        .all(|group| !group.is_empty() && group.len() <= LONGEST_DOTTED_QUAD_GROUP)
            })
    }

    /// Whether the file is an events file of a run.
    fn is_events_file(file: &Path) -> bool {
        file.to_string_lossy().ends_with(SPEC_EVENTS_SUFFIX)
    }

    /// Whether the file is one of the trace files, which share a run's header.
    fn is_trace_file(file: &Path) -> bool {
        file.parent()
            .is_some_and(|directory| directory.ends_with(SPEC_TRACES_UNDER_DATA_HOME))
    }

    /// Whether the file is an inbox socket.
    fn is_inbox_socket(file: &Path) -> bool {
        file.extension()
            .is_some_and(|extension| extension == SOCKET_EXTENSION)
    }

    /// The long runs of letters and digits in every readable file outside the traces, with every such file's path: the secrets, the sidecar paths and the inbox sockets the run holds.
    fn secrets_and_paths_of(files: &[PathBuf]) -> BTreeSet<String> {
        let mut markers = BTreeSet::new();
        for file in files.iter().filter(|file| !is_trace_file(file)) {
            let content = fs::read(file).unwrap_or_default();
            markers.extend(
                long_alphanumeric_runs(&String::from_utf8_lossy(&content)).map(str::to_owned),
            );
            markers.insert(file.to_string_lossy().into_owned());
        }
        markers
    }

    /// What no events file may hold: the body, a URL, the IPv6 loopback, and every path and every long run of letters and digits of the machine's files but the trace files.
    fn planted_markers(files: &[PathBuf]) -> BTreeSet<String> {
        let mut markers: BTreeSet<String> = [BODY_MARKER, URL_SCHEME_SEPARATOR, IPV6_LOOPBACK]
            .map(str::to_owned)
            .into();
        markers.extend(secrets_and_paths_of(files));
        markers
    }

    #[test]
    fn observability_decision_6_the_events_files_of_a_real_exchange_hold_no_body_secret_path_or_address()
     {
        let network =
            LocalNetwork::create("events-no-secret", &RunRecordKeeping::from_environment());
        let host = network.stand_in_host();
        let MetPair {
            mut amy, mut bob, ..
        } = network.amy_and_bob_met(&host);
        assert!(call_succeeded(&amy.send(BOB_HANDLE, BODY_MARKER)));
        bob.wait_for_inbox_body(BODY_MARKER, DELIVERY_WAIT);
        let sockets: Vec<String> = network
            .base_directory_files()
            .iter()
            .filter(|file| is_inbox_socket(file))
            .map(|socket| socket.to_string_lossy().into_owned())
            .collect();
        for socket in &sockets {
            let mut client = UnixStream::connect(socket).unwrap();
            client.write_all(SPEC_SINCE_EVERY_FRAME.as_bytes()).unwrap();
        }
        amy.wait_for_event("inbox.client_gone", CLIENT_WAIT);
        bob.wait_for_event("inbox.client_gone", CLIENT_WAIT);
        amy.close_standard_input();
        bob.close_standard_input();
        amy.wait_for_exit();
        bob.wait_for_exit();

        let files = network.base_directory_files();
        let mut markers = planted_markers(&files);
        markers.extend(sockets.iter().cloned());
        let recorded: String = files
            .iter()
            .filter(|file| is_events_file(file))
            .map(|events| fs::read_to_string(events).unwrap())
            .collect();

        assert!(!sockets.is_empty(), "no inbox socket was found to plant");
        for name in EVENTS_THIS_EXCHANGE_RECORDS {
            assert!(recorded.contains(name), "no {name} event was recorded");
        }
        for marker in &markers {
            assert!(
                !recorded.contains(marker.as_str()),
                "an events file holds {marker}"
            );
        }
        assert!(
            !holds_a_key_length_hex_run(&recorded),
            "an events file holds a key-length hex run"
        );
        assert!(
            !holds_a_dotted_quad(&recorded),
            "an events file holds an IPv4 address"
        );
    }

    /// Every events file the machine's runs wrote, with its parsed lines.
    fn events_files(network: &LocalNetwork) -> BTreeMap<PathBuf, Vec<Value>> {
        network
            .base_directory_files()
            .into_iter()
            .filter(|file| is_trace_file(file) && is_events_file(file))
            .map(|file| {
                let lines = fs::read_to_string(&file)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                (file, lines)
            })
            .collect()
    }

    /// The text of every socket address and its address alone.
    fn address_markers(addresses: impl IntoIterator<Item = SocketAddr>) -> BTreeSet<String> {
        addresses
            .into_iter()
            .flat_map(|address| [address.to_string(), address.ip().to_string()])
            .collect()
    }

    /// The lines of the file whose event is the name.
    fn named<'lines>(lines: &'lines [Value], name: &str) -> Vec<&'lines Value> {
        lines.iter().filter(|line| line["event"] == name).collect()
    }

    /// The salted location hashes the file's path events give the port.
    fn location_hashes_at_port(lines: &[Value], port: u16) -> BTreeSet<String> {
        lines
            .iter()
            .filter(|line| {
                line["event"]
                    .as_str()
                    .is_some_and(|name| name.starts_with(PATH_EVENT_PREFIX))
                    && line["port"] == json!(port)
            })
            .map(|line| line["location"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The salted location hashes the file's path events give each path of each connection.
    fn location_hashes_by_path(lines: &[Value]) -> BTreeMap<(u64, String), BTreeSet<String>> {
        let mut by_path: BTreeMap<(u64, String), BTreeSet<String>> = BTreeMap::new();
        for line in lines.iter().filter(|line| {
            line["event"]
                .as_str()
                .is_some_and(|name| name.starts_with(PATH_EVENT_PREFIX))
        }) {
            by_path
                .entry((
                    line["connection"].as_u64().unwrap(),
                    line["path"].as_str().unwrap().to_owned(),
                ))
                .or_default()
                .insert(line["location"].as_str().unwrap().to_owned());
        }
        by_path
    }

    /// The text the file holds, with its path for the failure message.
    fn file_text(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn observability_decision_6_no_body_secret_socket_path_or_address_reaches_an_events_file_and_an_address_hashes_alike_within_a_run_and_apart_across_runs()
     {
        let network = LocalNetwork::create("o6", &RunRecordKeeping::from_environment());
        let host = network.stand_in_host();
        let amy_card = network.create_card(AMY_CARD, AMY_HANDLE);
        let bob_card = network.create_card(BOB_CARD, BOB_HANDLE);
        let mut amy = network.start_member();
        let amy_claim = amy.init_through(&amy_card, &host);
        let mut bob = network.start_member();
        let bob_claim = bob.init_through(&bob_card, &host);
        amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, online);
        bob.peer_within(AMY_HANDLE, PRESENCE_WAIT, online);
        let host_address = host.endpoint().addr();
        let host_ports: BTreeSet<u16> = host_address.ip_addrs().map(SocketAddr::port).collect();
        let mut markers = secrets_and_paths_of(&network.base_directory_files());
        markers.extend(address_markers(host_address.ip_addrs().copied()));
        markers.extend(address_markers(
            amy.bound_udp_ports()
                .into_iter()
                .chain(bob.bound_udp_ports())
                .chain(host_ports.iter().copied())
                .map(|port| SocketAddr::new(LOOPBACK, port)),
        ));
        for claim in [&amy_claim, &bob_claim] {
            markers.insert(claim["result"]["inbox_path"].as_str().unwrap().to_owned());
        }
        markers.insert(MARKER_BODY.to_owned());

        let sent = amy.send(BOB_HANDLE, MARKER_BODY);
        assert_eq!(sent["result"]["delivered"], json!(true), "{sent}");
        assert!(
            bob.inbox_bodies().contains(&json!(MARKER_BODY)),
            "the body reached bob"
        );
        for member in [&mut amy, &mut bob] {
            member.close_standard_input();
            assert!(member.wait_for_exit().success());
        }

        let files = events_files(&network);
        assert_eq!(files.len(), MEMBER_RUNS, "{:?}", files.keys());
        let all_lines: Vec<Value> = files.values().flatten().cloned().collect();
        for name in [
            "transport.path_opened",
            "direct.request_sent",
            "direct.request_served",
        ] {
            assert!(
                !named(&all_lines, name).is_empty(),
                "the run recorded {name}, so its fields were written"
            );
        }
        for path in files.keys() {
            let text = file_text(path);
            let found: Vec<&String> = markers
                .iter()
                .filter(|marker| text.contains(marker.as_str()))
                .collect();
            assert!(found.is_empty(), "{} holds {found:?}", path.display());
            assert!(
                !holds_a_key_length_hex_run(&text),
                "{} holds a run of {SPEC_KEY_HEX_CHARS} hex characters",
                path.display()
            );
        }
        for (path, lines) in &files {
            for (path_of_a_connection, hashes) in location_hashes_by_path(lines) {
                assert_eq!(
                    hashes.len(),
                    1,
                    "{}: path {path_of_a_connection:?} hashes its one address alike",
                    path.display()
                );
            }
        }
        let host_hashes: Vec<BTreeSet<String>> = files
            .values()
            .map(|lines| {
                host_ports
                    .iter()
                    .flat_map(|port| location_hashes_at_port(lines, *port))
                    .collect()
            })
            .collect();
        let [amy_or_bob, the_other] = host_hashes.as_slice() else {
            panic!("two runs: {host_hashes:?}");
        };
        assert!(
            !amy_or_bob.is_empty() && !the_other.is_empty(),
            "both runs record a path to the host: {host_hashes:?}"
        );
        assert!(
            amy_or_bob.is_disjoint(the_other),
            "the host's address hashes apart in two runs: {host_hashes:?}"
        );
    }
}

#[cfg(test)]
mod observability_decision_9_on_one_machine {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::time::Duration;

    use serde_json::{Value, json};

    use crate::local_network::{AMY_CARD, AMY_HANDLE, BOB_CARD, BOB_HANDLE, LocalNetwork};
    use crate::mcp_server::{INIT_TOOL, PRESENCE_WAIT, call_succeeded, init_arguments, online};
    use crate::member::Member;
    use crate::scratch_machine::RunRecordKeeping;
    use crate::trace_reader::{TraceLine, events_lines_in};

    /// The body amy sends bob before bob's inbox client attaches.
    const UNATTACHED_BODY: &str = "sent while bob has no inbox client";
    /// The body amy sends bob while bob's inbox client is attached.
    const ATTACHED_BODY: &str = "sent while bob has an inbox client";
    /// The body amy broadcasts to bob and to a handle no member holds.
    const BROADCAST_BODY: &str = "broadcast to bob and to nobody";
    /// A handle no member of the exchange holds, so its send is answered `unknown_handle` (spec 3.10).
    const NO_MEMBER: &str = "nobody";
    /// The first line an inbox client writes to read every frame from the first (spec 3.11).
    const SPEC_SINCE_EVERY_FRAME: &str = "{\"since\": 0}\n";
    /// The departure wait at exit of spec 3.14, in milliseconds, as `process.departure_wait_over` writes it.
    const SPEC_DEPARTURE_WAIT_MS: u64 = 5000;
    /// The signal name `process.signal_received` writes for a SIGTERM (spec 3.9).
    const SPEC_TERMINATE_SIGNAL: &str = "SIGTERM";
    /// A `since` below the inbox tool's minimum of 0, which it answers `bad_request` (spec 3.10).
    const SPEC_BELOW_ANY_SEQ: i64 = -1;
    /// How long a member may take to record an event the exchange provokes, and an inbox client to read a frame.
    const EVENT_WAIT: Duration = Duration::from_secs(20);
    /// The frames bob's inbox client reads: the one sent before it attached, replayed, and the one sent while attached.
    const FRAMES_THE_CLIENT_READS: usize = 2;
    /// The process events of a member whose standard input ends, in the order spec:784 and decision 9 give them.
    const STOPPED_BY_END_OF_INPUT: [&str; 9] = [
        "process.started",
        "process.claimed",
        "process.claim_refused",
        "process.input_ended",
        "process.departure_handed_over",
        "process.lock_released",
        "process.held_connections_dropped",
        "process.departure_wait_over",
        "process.exited",
    ];
    /// The process events of a member stopped by SIGTERM, in the order spec:784 and decision 9 give them.
    const STOPPED_BY_SIGTERM: [&str; 8] = [
        "process.started",
        "process.claimed",
        "process.signal_received",
        "process.departure_handed_over",
        "process.lock_released",
        "process.held_connections_dropped",
        "process.departure_wait_over",
        "process.exited",
    ];
    /// The start of the name of every event of the process area (decision 9).
    const PROCESS_AREA: &str = "process.";
    /// The stopping step the node's presence loop records on its own task, in no order the node guarantees against the session's steps.
    const CONFIGURATION_REWRITE_DECIDED: &str = "process.configuration_rewrite_decided";

    /// The event's own fields as written, each a pair of key and value, in order.
    fn fields_of(line: &TraceLine) -> Value {
        line.fields()
            .iter()
            .map(|(key, value)| json!([key, value]))
            .collect()
    }

    /// The fields of each line of the member's events that names the event, in the order written.
    fn fields_named(lines: &[TraceLine], event: &str) -> Vec<Value> {
        lines
            .iter()
            .filter(|line| line.event == event)
            .map(fields_of)
            .collect()
    }

    /// The names of the member's process events but the configuration rewrite its node records, in the order written.
    fn process_events(lines: &[TraceLine]) -> Vec<&str> {
        lines
            .iter()
            .map(|line| line.event.as_str())
            .filter(|event| {
                event.starts_with(PROCESS_AREA) && *event != CONFIGURATION_REWRITE_DECIDED
            })
            .collect()
    }

    /// Every line of the events files the member of that number wrote.
    fn events_of_member(network: &LocalNetwork, number: u32) -> Vec<TraceLine> {
        events_lines_in(&network.machine().traces_directory_of_member(number)).unwrap()
    }

    /// Starts a member, claims the card through the host, and returns it with its number and the inbox socket its claim names.
    fn claimed_member(
        network: &LocalNetwork,
        host: &crate::scripted_node::ScriptedNode,
        card: &crate::scratch_machine::Card,
    ) -> (Member, u32, PathBuf) {
        let number = network.machine().members_started();
        let mut member = network.start_member();
        let claim = member.init_through(card, host);
        let socket = PathBuf::from(claim["result"]["inbox_path"].as_str().unwrap());
        (member, number, socket)
    }

    /// The message id the send envelope names.
    fn sent_id(envelope: &Value) -> String {
        assert!(call_succeeded(envelope), "{envelope}");
        envelope["result"]["id"].as_str().unwrap().to_owned()
    }

    /// What one real exchange between amy and bob left: each member's events, the ids its sends were delivered under, what bob's inbox client read, the two refused calls and each exit code.
    struct RecordedExchange {
        amy_lines: Vec<TraceLine>,
        bob_lines: Vec<TraceLine>,
        unattached_id: String,
        attached_id: String,
        broadcast_id: String,
        frame_ids: Vec<Value>,
        second_claim: Value,
        bad_inbox: Value,
        amy_exit_code: i32,
        bob_exit_code: i32,
    }

    /// The ids of the first frames the inbox client reads, a replay included.
    fn frame_ids_read(client: UnixStream, frames: usize) -> Vec<Value> {
        let mut reader = BufReader::new(client);
        (0..frames)
            .map(|_| {
                let mut frame = String::new();
                reader.read_line(&mut frame).unwrap();
                serde_json::from_str::<Value>(&frame).unwrap()["id"].clone()
            })
            .collect()
    }

    /// Runs amy and bob through a host: a send before bob's inbox client attaches and one after, the client gone, a broadcast to nobody and bob, a second init and an inbox call below any seq, then amy's input ended and bob sent SIGTERM.
    fn recorded_exchange(network: &LocalNetwork) -> RecordedExchange {
        let host = network.stand_in_host();
        let amy_card = network.create_card(AMY_CARD, AMY_HANDLE);
        let bob_card = network.create_card(BOB_CARD, BOB_HANDLE);
        let (mut amy, amy_number, _amy_socket) = claimed_member(network, &host, &amy_card);
        let (mut bob, bob_number, bob_socket) = claimed_member(network, &host, &bob_card);
        amy.peer_within(BOB_HANDLE, PRESENCE_WAIT, online);
        bob.peer_within(AMY_HANDLE, PRESENCE_WAIT, online);

        let unattached_id = sent_id(&amy.send(BOB_HANDLE, UNATTACHED_BODY));
        let mut client = UnixStream::connect(&bob_socket).unwrap();
        client.set_read_timeout(Some(EVENT_WAIT)).unwrap();
        client.write_all(SPEC_SINCE_EVERY_FRAME.as_bytes()).unwrap();
        bob.wait_for_event("inbox.client_connected", EVENT_WAIT);
        let attached_id = sent_id(&amy.send(BOB_HANDLE, ATTACHED_BODY));
        let frame_ids = frame_ids_read(client, FRAMES_THE_CLIENT_READS);
        bob.wait_for_event("inbox.client_gone", EVENT_WAIT);
        let broadcast = amy.call(
            "broadcast",
            &json!({ "to": [NO_MEMBER, BOB_HANDLE], "body": BROADCAST_BODY }),
        );
        let broadcast_id = broadcast["result"]["results"][1]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let second_claim = amy.call(INIT_TOOL, &init_arguments(&amy_card.name));
        let bad_inbox = bob.call("inbox", &json!({ "since": SPEC_BELOW_ANY_SEQ }));
        amy.close_standard_input();
        bob.terminate();
        let amy_exit_code = amy.wait_for_exit().code().unwrap();
        let bob_exit_code = bob.wait_for_exit().code().unwrap();
        RecordedExchange {
            amy_lines: events_of_member(network, amy_number),
            bob_lines: events_of_member(network, bob_number),
            unattached_id,
            attached_id,
            broadcast_id,
            frame_ids,
            second_claim,
            bad_inbox,
            amy_exit_code,
            bob_exit_code,
        }
    }

    /// Requires each process event of the exchange in spec:784's order with its fields.
    fn assert_process_events(exchange: &RecordedExchange) {
        let RecordedExchange {
            amy_lines,
            bob_lines,
            ..
        } = exchange;
        assert_eq!(process_events(amy_lines), STOPPED_BY_END_OF_INPUT);
        assert_eq!(process_events(bob_lines), STOPPED_BY_SIGTERM);
        for lines in [amy_lines, bob_lines] {
            assert_eq!(
                fields_named(lines, CONFIGURATION_REWRITE_DECIDED).len(),
                1,
                "{CONFIGURATION_REWRITE_DECIDED}"
            );
        }
        assert_eq!(
            fields_named(amy_lines, "process.started"),
            [json!([
                ["version", env!("CARGO_PKG_VERSION")],
                ["command", "mcp"]
            ])]
        );
        assert_eq!(
            fields_named(amy_lines, "process.claimed"),
            [json!([["card", AMY_CARD], ["handle", AMY_HANDLE]])]
        );
        assert_eq!(
            fields_named(bob_lines, "process.claimed"),
            [json!([["card", BOB_CARD], ["handle", BOB_HANDLE]])]
        );
        assert_eq!(
            fields_named(amy_lines, "process.claim_refused"),
            [json!([["code", "bad_request"]])]
        );
        assert_eq!(fields_named(amy_lines, "process.input_ended"), [json!([])]);
        assert_eq!(
            fields_named(bob_lines, "process.signal_received"),
            [json!([["signal", SPEC_TERMINATE_SIGNAL]])]
        );
        for lines in [amy_lines, bob_lines] {
            for step in [
                "process.departure_handed_over",
                "process.lock_released",
                "process.held_connections_dropped",
            ] {
                assert_eq!(fields_named(lines, step), [json!([])], "{step}");
            }
            assert_eq!(
                fields_named(lines, "process.departure_wait_over"),
                [json!([["wait_ms", SPEC_DEPARTURE_WAIT_MS]])]
            );
        }
        assert_eq!(
            fields_named(amy_lines, "process.exited"),
            [json!([["status", exchange.amy_exit_code]])]
        );
        assert_eq!(
            fields_named(bob_lines, "process.exited"),
            [json!([["status", exchange.bob_exit_code]])]
        );
    }

    /// Requires each frame bob's inbox delivered with its message id, its frame number and whether a client was attached, and the client's coming and going.
    fn assert_inbox_events(exchange: &RecordedExchange) {
        assert_eq!(
            exchange.frame_ids,
            [json!(exchange.unattached_id), json!(exchange.attached_id)]
        );
        assert_eq!(
            fields_named(&exchange.bob_lines, "inbox.frame_delivered"),
            [
                json!([
                    ["message", exchange.unattached_id],
                    ["frame", 1],
                    ["attached", false]
                ]),
                json!([
                    ["message", exchange.attached_id],
                    ["frame", 2],
                    ["attached", true]
                ]),
                json!([
                    ["message", exchange.broadcast_id],
                    ["frame", 3],
                    ["attached", false]
                ]),
            ]
        );
        assert_eq!(
            fields_named(&exchange.bob_lines, "inbox.client_connected"),
            [json!([["clients", 1]])]
        );
        assert_eq!(
            fields_named(&exchange.bob_lines, "inbox.client_gone"),
            [json!([["clients", 0]])]
        );
    }

    /// Requires each tool call answered by name with its outcome, and each send of a send or broadcast with its handle and the message id the direct request carried.
    fn assert_tool_events(exchange: &RecordedExchange) {
        let RecordedExchange {
            amy_lines,
            bob_lines,
            unattached_id,
            attached_id,
            broadcast_id,
            ..
        } = exchange;
        assert_eq!(
            exchange.second_claim["error"]["code"],
            json!("bad_request"),
            "{}",
            exchange.second_claim
        );
        assert_eq!(
            exchange.bad_inbox["error"]["code"],
            json!("bad_request"),
            "{}",
            exchange.bad_inbox
        );
        assert_eq!(
            fields_named(amy_lines, "tool.send_answered"),
            [
                json!([
                    ["to", BOB_HANDLE],
                    ["outcome", "ok"],
                    ["message", unattached_id]
                ]),
                json!([
                    ["to", BOB_HANDLE],
                    ["outcome", "ok"],
                    ["message", attached_id]
                ]),
                json!([
                    ["to", NO_MEMBER],
                    ["outcome", "error"],
                    ["code", "unknown_handle"]
                ]),
                json!([
                    ["to", BOB_HANDLE],
                    ["outcome", "ok"],
                    ["message", broadcast_id]
                ]),
            ]
        );
        let requests_sent: Vec<Value> = amy_lines
            .iter()
            .filter(|line| line.event == "direct.request_sent")
            .filter_map(|line| line.value("message").cloned())
            .collect();
        for id in [unattached_id, attached_id, broadcast_id] {
            assert!(
                requests_sent.contains(&json!(id)),
                "{id} in {requests_sent:?}"
            );
        }
        let amy_tools = fields_named(amy_lines, "tool.answered");
        assert_eq!(
            amy_tools.first(),
            Some(&json!([["tool", "init"], ["outcome", "ok"]]))
        );
        assert!(amy_tools.contains(&json!([["tool", "peers"], ["outcome", "ok"]])));
        assert_eq!(
            amy_tools.last(),
            Some(&json!([
                ["tool", "init"],
                ["outcome", "error"],
                ["code", "bad_request"]
            ]))
        );
        assert!(fields_named(bob_lines, "tool.answered").contains(&json!([
            ["tool", "inbox"],
            ["outcome", "error"],
            ["code", "bad_request"]
        ])));
    }

    #[test]
    fn observability_decision_9_a_real_exchange_records_each_process_inbox_and_tool_event_with_its_fields()
     {
        let network = LocalNetwork::create("o9", &RunRecordKeeping::from_environment());

        let exchange = recorded_exchange(&network);

        assert_process_events(&exchange);
        assert_inbox_events(&exchange);
        assert_tool_events(&exchange);
    }
}
