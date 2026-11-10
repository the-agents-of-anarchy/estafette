#[cfg(test)]
#[path = "support/driven_runtime.rs"]
mod driven_runtime;
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
#[path = "support/timed_step.rs"]
mod timed_step;

/// The id of every Message a test builds by hand, which no check of these tests reads.
#[cfg(test)]
const MESSAGE_ID: estafette::wire::MessageId = [9; 16];

/// The body every send of these tests carries.
#[cfg(test)]
const BODY: &str = "the parser review is ready";

/// Sends the body to the handle through the member's send tool and returns the envelope.
#[cfg(test)]
#[track_caller]
fn send(from: &mut member::Member, to: &str) -> serde_json::Value {
    from.call("send", &serde_json::json!({ "to": to, "body": BODY }))
}

#[cfg(test)]
mod spec_3_15_restart_and_departure_on_one_machine {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::{Duration, Instant};

    use estafette::crypto::NetworkKeys;
    use estafette::net::endpoint::{DIRECT_ALPN, GateFailure};
    use estafette::payload::{open_response, seal_payload, seal_request};
    use estafette::wire::{
        Addr, Departure, Introduce, Message, Payload, Request, Response, Version, WIRE_LIMIT_BYTES,
    };
    use iroh::endpoint::Connection;
    use iroh::{EndpointId, SecretKey, TransportAddr};
    use serde_json::{Value, json};

    use crate::local_network::{LocalNetwork, MetPair};
    use crate::mcp_server::{PRESENCE_WAIT, gone, online};
    use crate::member::{Member, loopback_address, loopback_address_of};
    use crate::payload_placeholders::{UNSET_MAC, UNSIGNED, wall_clock_ms};
    use crate::payloads::presence_signed_by;
    use crate::scratch_machine::Card;
    use crate::scripted_node::ScriptedNode;
    use crate::spec_bounds::SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE;
    use crate::timed_step::timed;
    use crate::{MESSAGE_ID, send};

    const SPEC_GONE_KEPT_FOR: Duration = Duration::from_millis(1000);
    const STATUS_POLL: Duration = Duration::from_millis(50);
    const DIRECT_REQUEST_WAIT: Duration = Duration::from_secs(15);
    const SENT_BEFORE_THE_DEPARTURE: &str = "sent before the departure";
    const SENT_AFTER_THE_DEPARTURE: &str = "sent after the departure";
    const SENT_BEFORE_THE_NEW_PRESENCE: &str = "sent before the new presence";
    const SENT_AFTER_THE_NEW_PRESENCE: &str = "sent after the new presence";
    const PRESENCE_APPLIED_SETTLE: Duration = Duration::from_millis(500);
    const SPEC_OBSERVER_WAIT: Duration = Duration::from_millis(3000);
    const DOCUMENTATION_HOST: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    const SENT_BEFORE_THE_PRESENCE: &str = "sent before the presence";
    const SENT_AFTER_THE_PRESENCE: &str = "sent after the presence";

    /// A member, amy, who listed a scripted bob, then took bob's Departure of that seq and lists bob gone.
    struct ScriptedBobDeparted {
        amy: Member,
        amy_card: Card,
        bob: ScriptedNode,
        departure_seq: u64,
    }

    #[track_caller]
    fn amy_after_scripted_bob_departed(network: &LocalNetwork) -> ScriptedBobDeparted {
        let amy_card = network.create_card("amy-card", "amy");
        let bob_key = SecretKey::generate();
        let bob = ScriptedNode::start(
            bob_key.clone(),
            *network.keys().gate_key(),
            network.topic(),
            |_closing: GateFailure| {},
        );
        let mut amy = network.start_member();
        amy.init_through(&amy_card, &bob);
        bob.broadcast(presence_signed_by(
            network.keys(),
            &bob_key,
            "bob",
            &bob.endpoint().endpoint().addr(),
        ));
        amy.peer_within("bob", PRESENCE_WAIT, |_listed| true);
        let departure_seq = wall_clock_ms();
        bob.broadcast(departure_signed_by(network.keys(), &bob_key, departure_seq));
        amy.peer_within("bob", PRESENCE_WAIT, gone);
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

    fn message_from_bob(to: EndpointId, sent_at: u64, body: &str) -> Request {
        Request::Message(Message {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            to: *to.as_bytes(),
            handle: "bob".to_owned(),
            attached: false,
            sent_at,
            body: body.to_owned(),
            mac: UNSET_MAC,
        })
    }

    #[track_caller]
    fn request_directly(
        network: &LocalNetwork,
        node: &ScriptedNode,
        to: EndpointId,
        request: Request,
    ) -> Response {
        let sealed = seal_request(network.keys(), request).unwrap();
        let answer = node.request_directly(to, &sealed).unwrap();
        open_response(network.keys(), &answer).unwrap()
    }

    fn address_on_a_host_no_connection_runs_over(node: &ScriptedNode) -> iroh::EndpointAddr {
        let address = node.endpoint().endpoint().addr();
        let sockets: Vec<TransportAddr> = address
            .ip_addrs()
            .map(|socket| TransportAddr::Ip(SocketAddr::new(DOCUMENTATION_HOST, socket.port())))
            .collect();
        iroh::EndpointAddr::from_parts(address.id, sockets)
    }

    #[track_caller]
    fn exchange_on(
        network: &LocalNetwork,
        node: &ScriptedNode,
        connection: &Connection,
        request: Request,
    ) -> Option<Response> {
        let sealed = seal_request(network.keys(), request).unwrap();
        node.runtime().block_on(async {
            tokio::time::timeout(DIRECT_REQUEST_WAIT, async {
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

    #[track_caller]
    fn announce_until_seen(
        network: &LocalNetwork,
        node: &ScriptedNode,
        key: &SecretKey,
        address: &iroh::EndpointAddr,
        member: &mut Member,
    ) {
        let started = Instant::now();
        let first_sent_ms = wall_clock_ms();
        loop {
            node.broadcast(presence_signed_by(network.keys(), key, "bob", address));
            let seen_ms = member.peer_within("bob", PRESENCE_WAIT, |_listed| true)["last_seen"]
                .as_u64()
                .unwrap();
            if seen_ms > first_sent_ms {
                return;
            }
            assert!(
                started.elapsed() < PRESENCE_WAIT,
                "the new presence never arrived"
            );
            std::thread::sleep(STATUS_POLL);
        }
    }

    fn is_accepted_message_ack(response: &Response) -> bool {
        matches!(response, Response::MessageAck(ack) if ack.accepted)
    }

    #[track_caller]
    fn statuses_throughout(member: &mut Member, handle: &str, span: Duration) -> Vec<Value> {
        let started = Instant::now();
        let mut statuses = Vec::new();
        while started.elapsed() < span {
            statuses
                .push(member.peer_within(handle, PRESENCE_WAIT, |_listed| true)["status"].clone());
            std::thread::sleep(STATUS_POLL);
        }
        statuses.dedup();
        statuses
    }

    #[test]
    fn spec_3_15_test_18_a_killed_member_restarted_with_its_card_is_delivered_to_within_1_s_of_its_new_presence()
     {
        let network = LocalNetwork::create("r1");
        let first_host = network.stand_in_host();
        let MetPair {
            mut amy,
            amy_card,
            mut bob,
            bob_card,
        } = timed("amy and bob list each other online", || {
            network.amy_and_bob_met(&first_host)
        });
        let first_send = timed("amy's send to bob before the kill", || {
            send(&mut amy, "bob")
        });

        bob.kill();
        let killed_ms = wall_clock_ms();
        let second_host = network.stand_in_host();
        timed("a second host links amy", || {
            second_host.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
        });
        let mut restarted = timed("the restarted bob starts", || network.start_member());
        timed("the restarted bob claims its card", || {
            restarted.init_through(&bob_card, &second_host)
        });
        timed(
            "amy lists the restarted bob online, seen after the kill",
            || {
                amy.peer_within("bob", PRESENCE_WAIT, |bob| {
                    online(bob) && bob["last_seen"].as_u64().unwrap() > killed_ms
                })
            },
        );
        let new_presence_reached = Instant::now();
        let sent = timed("amy's send to the restarted bob", || send(&mut amy, "bob"));
        let delivered_after = new_presence_reached.elapsed();

        assert_eq!(
            first_send["result"]["delivered"],
            json!(true),
            "{first_send}"
        );
        assert_eq!(sent["result"]["delivered"], json!(true), "{sent}");
        assert!(
            delivered_after <= SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE,
            "{delivered_after:?}"
        );
    }

    #[test]
    fn spec_3_15_test_4_a_send_to_a_member_gone_by_the_end_of_input_after_an_earlier_send_is_unreachable()
     {
        let network = LocalNetwork::create("r2");
        let host = network.stand_in_host();
        let MetPair {
            mut amy, mut bob, ..
        } = network.amy_and_bob_met(&host);
        let first_send = send(&mut amy, "bob");

        bob.close_standard_input();
        amy.peer_within("bob", PRESENCE_WAIT, gone);
        let exit = bob.wait_for_exit();
        let sent = send(&mut amy, "bob");

        assert_eq!(
            first_send["result"]["delivered"],
            json!(true),
            "{first_send}"
        );
        assert!(exit.success(), "{exit:?}");
        assert_eq!(sent["error"]["code"], json!("unreachable"), "{sent}");
    }

    #[test]
    fn spec_3_15_test_4_a_message_sent_before_the_departure_amy_holds_reaches_her_inbox_and_leaves_its_sender_gone()
     {
        let network = LocalNetwork::create("r3m");
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
            statuses_throughout(&mut amy, "bob", SPEC_GONE_KEPT_FOR);
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
        amy.peer_within("bob", PRESENCE_WAIT, online);

        assert!(is_accepted_message_ack(&sent_before), "{sent_before:?}");
        assert_eq!(statuses_after_the_earlier_message, [json!("gone")]);
        assert_eq!(bodies, [json!(SENT_BEFORE_THE_DEPARTURE)]);
        assert!(is_accepted_message_ack(&sent_after), "{sent_after:?}");
    }

    #[test]
    fn spec_3_15_test_4_an_introduce_from_a_member_amy_holds_departed_leaves_it_gone() {
        let network = LocalNetwork::create("r3i");
        let ScriptedBobDeparted {
            mut amy,
            amy_card,
            bob,
            ..
        } = amy_after_scripted_bob_departed(&network);
        let bob_address = bob.endpoint().endpoint().addr();

        let answer = request_directly(
            &network,
            &bob,
            amy_card.id,
            Request::Introduce(Introduce {
                v: Version::CURRENT,
                handle: Some("bob".to_owned()),
                public: b"{}".to_vec(),
                attached: false,
                addr: Addr::from_endpoint_report(
                    bob_address.relay_urls().map(ToString::to_string),
                    bob_address.ip_addrs().copied(),
                ),
                mac: UNSET_MAC,
            }),
        );
        let statuses_after_the_introduce = statuses_throughout(&mut amy, "bob", SPEC_GONE_KEPT_FOR);

        assert!(matches!(answer, Response::IntroduceAck(_)), "{answer:?}");
        assert_eq!(statuses_after_the_introduce, [json!("gone")]);
    }

    #[test]
    fn spec_3_15_test_18_a_restarted_member_whose_message_came_before_its_presence_keeps_that_connection_and_is_delivered_to()
     {
        let network = LocalNetwork::create("r1k");
        let amy_card = network.create_card("amy-card", "amy");
        let bob_key = SecretKey::generate();
        let first_session = network.scripted_member(&bob_key, "bob");
        let mut amy = timed("amy starts", || network.start_member());
        timed("amy claims its card through the first bob", || {
            amy.init_through(&amy_card, &first_session)
        });
        timed("the first bob's presence", || {
            first_session.broadcast(presence_signed_by(
                network.keys(),
                &bob_key,
                "bob",
                &loopback_address_of(&first_session),
            ));
        });
        timed("amy lists the first bob online", || {
            amy.peer_within("bob", PRESENCE_WAIT, online)
        });
        drop(first_session);

        let restarted = network.scripted_member(&bob_key, "bob");
        timed("the restarted bob links amy", || {
            restarted.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
        });
        let held = timed("the restarted bob connects to amy", || {
            restarted
                .runtime()
                .block_on(
                    restarted
                        .endpoint()
                        .endpoint()
                        .connect(amy_card.id, DIRECT_ALPN),
                )
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
        timed("amy lists the restarted bob's new presence", || {
            announce_until_seen(
                &network,
                &restarted,
                &bob_key,
                &loopback_address_of(&restarted),
                &mut amy,
            );
        });
        timed("amy applies the new presence", || {
            std::thread::sleep(PRESENCE_APPLIED_SETTLE);
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
        let sent = timed("amy's send to the restarted bob", || send(&mut amy, "bob"));

        assert!(
            before_presence
                .as_ref()
                .is_some_and(is_accepted_message_ack),
            "{before_presence:?}"
        );
        assert!(
            after_presence.as_ref().is_some_and(is_accepted_message_ack),
            "{after_presence:?}"
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
        let network = LocalNetwork::create("live-unannounced-path");
        let amy_card = network.create_card("amy-card", "amy");
        let bob_key = SecretKey::generate();
        let bob = network.scripted_member(&bob_key, "bob");
        let mut amy = network.start_member();
        amy.init_through(&amy_card, &bob);
        let held = bob
            .runtime()
            .block_on(bob.endpoint().endpoint().connect(amy_card.id, DIRECT_ALPN))
            .unwrap();
        let before_presence = exchange_on(
            &network,
            &bob,
            &held,
            message_from_bob(amy_card.id, wall_clock_ms(), SENT_BEFORE_THE_PRESENCE),
        );
        std::thread::sleep(SPEC_OBSERVER_WAIT);

        announce_until_seen(
            &network,
            &bob,
            &bob_key,
            &address_on_a_host_no_connection_runs_over(&bob),
            &mut amy,
        );
        std::thread::sleep(PRESENCE_APPLIED_SETTLE);
        let after_presence = exchange_on(
            &network,
            &bob,
            &held,
            message_from_bob(amy_card.id, wall_clock_ms(), SENT_AFTER_THE_PRESENCE),
        );

        assert!(
            before_presence
                .as_ref()
                .is_some_and(is_accepted_message_ack),
            "{before_presence:?}"
        );
        assert!(
            after_presence.as_ref().is_some_and(is_accepted_message_ack),
            "{after_presence:?}"
        );
    }
}

#[cfg(test)]
mod spec_3_15_test_19_on_one_machine {
    use std::collections::BTreeSet;
    use std::time::{Duration, Instant};

    use iroh::{EndpointId, SecretKey};
    use iroh_gossip::api::Event;

    use crate::local_network::{LocalNetwork, MetPair};
    use crate::mcp_server::online;
    use crate::member::{loopback_address, loopback_address_of};
    use crate::payloads::presence_signed_by;
    use crate::scripted_node::ScriptedNode;

    const SPEC_OBSERVER_WAIT: Duration = Duration::from_millis(3000);
    const GOSSIP_NEIGHBOR_WAIT: Duration = Duration::from_secs(10);
    const EVENTS_POLL: Duration = Duration::from_millis(50);

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

    #[track_caller]
    fn wait_for_gossip_neighbor(node: &ScriptedNode, neighbor: EndpointId) {
        let started = Instant::now();
        while !nodes_ever_gossip_neighbors(node).contains(&neighbor) {
            assert!(
                started.elapsed() < GOSSIP_NEIGHBOR_WAIT,
                "{neighbor} was no gossip neighbor within {GOSSIP_NEIGHBOR_WAIT:?}"
            );
            std::thread::sleep(EVENTS_POLL);
        }
    }

    #[test]
    fn spec_3_15_test_19_a_member_lists_a_node_whose_only_gossip_neighbor_is_another_member_within_the_observer_wait_of_its_presence()
     {
        let network = LocalNetwork::create("t19");
        let host = network.stand_in_host();
        let MetPair {
            mut amy,
            bob,
            bob_card,
            ..
        } = network.amy_and_bob_met(&host);
        let carol_key = SecretKey::generate();
        let carol = network.scripted_member(&carol_key, "carol");
        carol.admit(loopback_address(bob_card.id, &bob.bound_udp_ports()));
        wait_for_gossip_neighbor(&carol, bob_card.id);

        carol.broadcast(presence_signed_by(
            network.keys(),
            &carol_key,
            "carol",
            &loopback_address_of(&carol),
        ));
        let presence_sent = Instant::now();
        amy.peer_within("carol", SPEC_OBSERVER_WAIT, online);
        let listed_after = presence_sent.elapsed();
        let carol_gossip_neighbors = nodes_ever_gossip_neighbors(&carol);

        assert!(listed_after <= SPEC_OBSERVER_WAIT, "{listed_after:?}");
        assert_eq!(carol_gossip_neighbors, BTreeSet::from([bob_card.id]));
    }
}

#[cfg(test)]
mod spec_3_15_with_one_member {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use estafette::crypto::{NetworkKeys, NetworkSecret};
    use estafette::net::endpoint::GateFailure;
    use estafette::payload::{open_response, seal_request};
    use estafette::wire::{self, Message, Payload, Request, Response, Version};
    use iroh::{EndpointAddr, EndpointId, SecretKey};
    use iroh_gossip::api::Event;
    use serde_json::{Value, json};

    use crate::MESSAGE_ID;
    use crate::local_network::LocalNetwork;
    use crate::mcp_server::PRESENCE_WAIT;
    use crate::member::{Member, loopback_address};
    use crate::payload_placeholders::{UNSET_MAC, wall_clock_ms};
    use crate::payloads::presence_signed_by;
    use crate::scripted_node::ScriptedNode;

    const SPEC_SIMULTANEOUS_INIT_WINDOW: Duration = Duration::from_millis(100);
    const SPEC_TOOL_NAMES: [&str; 6] =
        ["list_cards", "init", "peers", "send", "broadcast", "inbox"];
    const SPEC_GATE_TIMEOUT_WITH_SLACK: Duration = Duration::from_secs(5);
    const SPEC_DEAF_TIMEOUT_AND_REBIND_WITH_SLACK: Duration = Duration::from_secs(30);
    const SPEC_MEMBER_JOIN_WAIT_WITH_SLACK: Duration = Duration::from_secs(10);
    const LOOSE_SIDECAR_MODE: u32 = 0o644;
    const ANY_BODY: &str = "the parser review is ready";
    const FORGED_BODY: &str = "a message under a wrong mac";
    const HONEST_BODY: &str = "a message under the network mac";
    const ATTACKER_HANDLE: &str = "mallory";
    const OUTSIDER_SECRET_BYTE: u8 = 77;
    const NOTHING_FURTHER_WAIT: Duration = Duration::from_millis(1000);
    const POLL: Duration = Duration::from_millis(100);
    const TRANSIENT_SOCKET_LIFETIME: Duration = Duration::from_millis(1000);

    fn unclaimed() -> Value {
        json!({ "card": null, "handle": null, "status": "unclaimed" })
    }

    fn every_tool_call(card: &str) -> [(&'static str, Value); 6] {
        [
            ("list_cards", json!({})),
            ("init", json!({ "card": card })),
            ("peers", json!({})),
            ("send", json!({ "to": "nobody", "body": ANY_BODY })),
            ("broadcast", json!({ "to": ["nobody"], "body": ANY_BODY })),
            ("inbox", json!({})),
        ]
    }

    #[track_caller]
    fn listed_handles(member: &mut Member) -> Vec<Value> {
        member.call("peers", &json!({}))["result"]["peers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|peer| peer["handle"].clone())
            .collect()
    }

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

    fn count_beyond(count: &AtomicUsize, already: usize, within: Duration) -> usize {
        let started = Instant::now();
        while count.load(Ordering::SeqCst) <= already && started.elapsed() < within {
            std::thread::sleep(POLL);
        }
        count.load(Ordering::SeqCst)
    }

    #[track_caller]
    fn rebound_ports(member: &Member, before: &[u16], within: Duration) -> Vec<u16> {
        let started = Instant::now();
        loop {
            let fresh = member.ipv4_udp_ports();
            std::thread::sleep(TRANSIENT_SOCKET_LIFETIME);
            let held: Vec<u16> = member
                .ipv4_udp_ports()
                .into_iter()
                .filter(|port| fresh.contains(port) && !before.contains(port))
                .collect();
            if !held.is_empty() {
                return held;
            }
            assert!(started.elapsed() < within, "the endpoint was not rebound");
        }
    }

    #[test]
    fn spec_3_15_test_6_two_processes_init_one_card_within_100_ms_and_exactly_one_claims_it() {
        let network = LocalNetwork::create("t6");
        let card = network.create_card("amy-card", "amy");
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
        assert_eq!(claimed[0]["result"]["handle"], json!("amy"));
        assert_eq!(claimed[0]["you"]["status"], json!("claimed"));
        assert_eq!(refused.len(), 1, "{answers:?}");
        assert_eq!(refused[0]["error"]["code"], json!("card_held"));
        assert_eq!(refused[0]["you"], unclaimed());
    }

    #[test]
    fn spec_3_15_test_9_a_message_with_a_wrong_mac_is_dropped_with_no_response() {
        let network = LocalNetwork::create("t9");
        let card = network.create_card("amy-card", "amy");
        let attacker = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &attacker);
        let outsider_keys =
            NetworkKeys::derive(&NetworkSecret::from_bytes([OUTSIDER_SECRET_BYTE; 32]));

        let forged_answer = attacker.request_directly(
            card.id,
            &seal_request(&outsider_keys, message_to(&card.id, FORGED_BODY)).unwrap(),
        );
        let honest_answer = attacker
            .request_directly(
                card.id,
                &seal_request(network.keys(), message_to(&card.id, HONEST_BODY)).unwrap(),
            )
            .unwrap();
        let bodies = amy.inbox_bodies();

        assert_eq!(forged_answer, None);
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
        let network = LocalNetwork::create("t10");
        let card = network.create_card("amy-card", "amy");
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        let forger = SecretKey::generate();
        let honest = SecretKey::generate();
        let nowhere = EndpointAddr::from_parts(forger.public(), []);

        host.broadcast(renamed_after_signing(
            network.keys(),
            &presence_signed_by(network.keys(), &forger, "genuine", &nowhere),
            "forged",
        ));
        host.broadcast(presence_signed_by(
            network.keys(),
            &honest,
            "honest",
            &nowhere,
        ));

        amy.peer_within("honest", PRESENCE_WAIT, |_honest| true);
        std::thread::sleep(NOTHING_FURTHER_WAIT);
        assert_eq!(listed_handles(&mut amy), [json!("honest")]);
    }

    #[test]
    fn spec_3_15_test_12_the_mcp_tool_list_is_identical_before_and_after_init() {
        let network = LocalNetwork::create("t12");
        let card = network.create_card("amy-card", "amy");
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
        let network = LocalNetwork::create("t13");
        let card = network.create_card("amy-card", "amy");
        let mut amy = network.start_member();
        let claimed = json!({ "card": card.name, "handle": "amy", "status": "claimed" });

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
        let network = LocalNetwork::create("t16");
        let card = network.create_card("amy-card", "amy");
        network.set_sidecar_mode(&card, LOOSE_SIDECAR_MODE);
        let mut amy = network.start_member();

        let refused = amy.call("init", &json!({ "card": card.name }));

        assert_eq!(refused["ok"], json!(false), "{refused}");
        assert_eq!(refused["error"]["code"], json!("sidecar_invalid"));
        assert_eq!(refused["you"], unclaimed());
    }

    #[test]
    fn spec_3_15_test_21_a_node_with_the_topic_id_but_not_the_secret_gets_no_neighbor_and_no_payload_dialing_or_dialed()
     {
        let network = LocalNetwork::create("t21");
        let card = network.create_card("amy-card", "amy");
        network.add_host_that_never_answers();
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        let ports_before_rebind = amy.bound_udp_ports();
        let outsider_key = SecretKey::generate();
        let outsider_keys =
            NetworkKeys::derive(&NetworkSecret::from_bytes([OUTSIDER_SECRET_BYTE; 32]));
        let closings = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&closings);
        let outsider = ScriptedNode::start(
            outsider_key.clone(),
            *outsider_keys.gate_key(),
            network.topic(),
            move |_closing: GateFailure| {
                counted.fetch_add(1, Ordering::SeqCst);
            },
        );

        outsider.admit(loopback_address(card.id, &ports_before_rebind));
        let closed_when_dialing = count_beyond(&closings, 0, SPEC_GATE_TIMEOUT_WITH_SLACK);
        host.broadcast(presence_signed_by(
            network.keys(),
            &outsider_key,
            "outsider",
            &outsider.endpoint().endpoint().addr(),
        ));
        amy.peer_within("outsider", PRESENCE_WAIT, |_outsider| true);
        let rebound = rebound_ports(
            &amy,
            &ports_before_rebind,
            SPEC_DEAF_TIMEOUT_AND_REBIND_WITH_SLACK,
        );
        host.admit(loopback_address(card.id, &rebound));
        let closed_when_dialed = count_beyond(
            &closings,
            closed_when_dialing,
            SPEC_MEMBER_JOIN_WAIT_WITH_SLACK,
        );

        assert!(closed_when_dialing >= 1);
        assert!(closed_when_dialed > closed_when_dialing);
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

    use crate::local_network::{LocalNetwork, MetPair};
    use crate::mcp_server::{PRESENCE_WAIT, gone, online};
    use crate::member::{loopback_address, loopback_address_of};
    use crate::payload_placeholders::wall_clock_ms;
    use crate::payloads::presence_signed_by;
    use crate::scripted_node::ScriptedNode;
    use crate::{BODY, send};

    /// Longer than every interval spec 3.14 names below the announcement max age: the 2000 ms re-announce interval, the 15 s deaf timeout and the 20 s rebind interval.
    const PRESENCE_QUIET_WINDOW: Duration = Duration::from_secs(30);
    /// Time for the address set a member announces to settle after its first Presence, beyond the 2000 ms re-announce interval.
    const ADDRESS_SETTLE: Duration = Duration::from_secs(5);
    /// The ack timeout of spec 3.14 (10000 ms), the longest a send waits for an answer, with slack.
    const SPEC_ACK_TIMEOUT_WITH_SLACK: Duration = Duration::from_secs(15);
    /// Time after a failed send in which a retry would reach the receiver.
    const RETRY_WATCH: Duration = Duration::from_secs(5);
    /// Thrice the ten sends per sixty seconds the Claude Code bridge allows itself, so any limit of that order in the binary refuses some.
    const SENDS_IN_A_ROW: usize = 30;
    /// Time for a message held for an absent member to reach it once it returns.
    const REDELIVERY_WATCH: Duration = Duration::from_secs(5);
    const EVENTS_POLL: Duration = Duration::from_millis(100);

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
        node.events()
            .into_iter()
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
        let network = LocalNetwork::create("n1");
        let card = network.create_card("amy-card", "amy");
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
        let network = LocalNetwork::create("n2");
        let host = network.stand_in_host();
        let MetPair {
            mut amy,
            mut bob,
            bob_card,
            ..
        } = network.amy_and_bob_met(&host);
        let delivered = send(&mut amy, "bob");
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
        let network = LocalNetwork::create("n3");
        let host = network.stand_in_host();
        let MetPair {
            mut amy,
            amy_card,
            mut bob,
            bob_card,
        } = network.amy_and_bob_met(&host);
        bob.close_standard_input();
        assert!(bob.wait_for_exit().success());
        amy.peer_within("bob", PRESENCE_WAIT, gone);
        let stopped_ms = wall_clock_ms();

        let refused = send(&mut amy, "bob");
        let second_host = network.stand_in_host();
        second_host.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
        let mut restarted = network.start_member();
        restarted.init_through(&bob_card, &second_host);
        amy.peer_within("bob", PRESENCE_WAIT, |bob| {
            online(bob) && bob["last_seen"].as_u64().unwrap() > stopped_ms
        });
        std::thread::sleep(REDELIVERY_WATCH);

        assert_eq!(refused["error"]["code"], json!("unreachable"), "{refused}");
        assert_eq!(restarted.inbox_bodies(), Vec::<Value>::new());
    }

    #[test]
    fn spec_3_16_a_member_with_nothing_to_announce_sends_no_presence_for_longer_than_every_interval_of_spec_3_14()
     {
        let network = LocalNetwork::create("n4");
        let card = network.create_card("amy-card", "amy");
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        let started = Instant::now();
        while presences_received_from(&host, *card.id.as_bytes()) == 0 {
            assert!(
                started.elapsed() < PRESENCE_WAIT,
                "amy announced no presence"
            );
            std::thread::sleep(EVENTS_POLL);
        }
        std::thread::sleep(ADDRESS_SETTLE);

        let settled = presences_received_from(&host, *card.id.as_bytes());
        std::thread::sleep(PRESENCE_QUIET_WINDOW);
        let after_window = presences_received_from(&host, *card.id.as_bytes());

        assert_eq!(after_window, settled);
    }

    #[test]
    fn spec_3_16_a_send_whose_ack_never_comes_reaches_the_receiver_once() {
        let network = LocalNetwork::create("n5");
        let card = network.create_card("amy-card", "amy");
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        let messages_read = Arc::new(AtomicUsize::new(0));
        let carol_key = SecretKey::generate();
        let carol = ScriptedNode::start_answering_direct(
            carol_key.clone(),
            *network.keys().gate_key(),
            network.topic(),
            AnsweringNothing {
                keys: NetworkKeys::clone(network.keys()),
                messages_read: Arc::clone(&messages_read),
            },
        );
        host.broadcast(presence_signed_by(
            network.keys(),
            &carol_key,
            "carol",
            &loopback_address_of(&carol),
        ));
        amy.peer_within("carol", PRESENCE_WAIT, |_carol| true);

        let sent_at = Instant::now();
        let sent = send(&mut amy, "carol");
        let answered_after = sent_at.elapsed();
        std::thread::sleep(RETRY_WATCH);

        assert_eq!(messages_read.load(Ordering::SeqCst), 1);
        assert_eq!(sent["ok"], json!(false), "{sent}");
        assert!(
            answered_after <= SPEC_ACK_TIMEOUT_WITH_SLACK,
            "{answered_after:?}"
        );
    }

    #[test]
    fn spec_3_16_thirty_sends_in_a_row_are_all_delivered() {
        let network = LocalNetwork::create("n6");
        let card = network.create_card("amy-card", "amy");
        let host = network.stand_in_host();
        let mut amy = network.start_member();
        amy.init_through(&card, &host);
        let carol_key = SecretKey::generate();
        let carol = network.scripted_member(&carol_key, "carol");
        host.broadcast(presence_signed_by(
            network.keys(),
            &carol_key,
            "carol",
            &loopback_address_of(&carol),
        ));
        amy.peer_within("carol", PRESENCE_WAIT, online);

        let outcomes: Vec<Value> = (0..SENDS_IN_A_ROW)
            .map(|_| send(&mut amy, "carol")["result"]["delivered"].clone())
            .collect();

        assert_eq!(outcomes, vec![json!(true); SENDS_IN_A_ROW]);
    }
}

#[cfg(test)]
mod kept_run_records {
    use std::fs;
    use std::io::Write;
    use std::path::Path;

    use crate::scratch_machine::{REDACTED, ScratchMachine};

    const NETWORK: &str = "acme";
    /// A network secret chosen by this test, in the shape the configuration file takes, not a real secret.
    const CHOSEN_SECRET: &str = "5ec2e7c0ffee5ec2e7c0ffee5ec2e7c0ffee5ec2e7c0ffee5ec2e7c0ffee5ec2";
    /// An owner key in the shape the configuration file takes, not a real key.
    const CHOSEN_OWNER: &str = "0aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1e20aa1";
    /// A configuration line chosen by this test with no run of letters and digits as long as a key's.
    const CHOSEN_SHORT_LINE: &str = "# acme lab, set up by hand";

    /// Writes the network configuration of the lines in order, with no `hosts`, which the binary refuses.
    fn write_configuration_without_hosts(machine: &ScratchMachine, lines: &[&str]) {
        let config_file = machine.directories().network_config_file(NETWORK).unwrap();
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        fs::write(&config_file, lines.join("\n")).unwrap();
    }

    /// Starts the binary's MCP server on the machine and requires it to exit refused.
    fn start_refused(machine: &ScratchMachine) {
        assert!(!machine.spawn_mcp_server().wait().unwrap().success());
    }

    /// Appends the lines to every run record of the machine, as a refusal quoting the configuration would print them.
    fn append_to_every_run_record(machine: &ScratchMachine, lines: &[&str]) {
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
        drop(fs::remove_dir_all(directory));
        contents
    }

    /// Kept run records of refused members that hold a line of their network configuration, the secret's line or a short one, hold the redaction marker in place of either.
    #[test]
    fn a_kept_run_record_holds_the_redaction_marker_in_place_of_every_configuration_line_a_member_record_holds()
     {
        let machine = ScratchMachine::fresh("kept-record", NETWORK);
        let secret_line = format!("secret = \"{CHOSEN_SECRET}\"");
        let owner_line = format!("owner = \"{CHOSEN_OWNER}\"");
        write_configuration_without_hosts(&machine, &[&secret_line, &owner_line]);
        start_refused(&machine);
        write_configuration_without_hosts(
            &machine,
            &[CHOSEN_SHORT_LINE, &secret_line, &owner_line],
        );
        start_refused(&machine);
        append_to_every_run_record(&machine, &[CHOSEN_SHORT_LINE, &secret_line, &owner_line]);
        let kept_in =
            std::env::temp_dir().join(format!("estafette-kept-record-test-{}", std::process::id()));

        let records = kept_contents(&machine, &kept_in);

        assert_eq!(records.len(), 2);
        for record in &records {
            assert!(
                !record.contains(CHOSEN_SECRET),
                "a kept record holds the secret"
            );
            assert!(
                !record.contains(CHOSEN_SHORT_LINE),
                "a kept record holds the short configuration line"
            );
            assert!(record.contains(REDACTED), "{record}");
        }
    }
}
