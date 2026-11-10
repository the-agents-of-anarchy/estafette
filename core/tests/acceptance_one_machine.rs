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
#[path = "one_machine/payloads.rs"]
mod payloads;
#[cfg(test)]
#[path = "support/scratch_machine.rs"]
mod scratch_machine;
#[cfg(test)]
#[path = "one_machine/scripted_node.rs"]
mod scripted_node;

#[cfg(test)]
mod spec_3_15_restart_and_departure_on_one_machine {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::{Duration, Instant};

    use estafette::crypto::NetworkKeys;
    use estafette::net::direct::{DirectHost, DirectServer};
    use estafette::net::endpoint::{DIRECT_ALPN, GateFailure};
    use estafette::payload::{open_response, seal_request};
    use estafette::wire::{
        self, Addr, Introduce, Message, Request, Response, Version, WIRE_LIMIT_BYTES,
    };
    use iroh::endpoint::Connection;
    use iroh::{EndpointId, SecretKey, TransportAddr};
    use serde_json::{Value, json};

    use crate::local_network::LocalNetwork;
    use crate::member::{Member, loopback_address};
    use crate::payload_placeholders::{UNSET_MAC, wall_clock_ms};
    use crate::payloads::{departure_signed_by, presence_signed_by};
    use crate::scratch_machine::Card;
    use crate::scripted_node::ScriptedNode;

    const SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE: Duration = Duration::from_millis(1000);
    const PRESENCE_WAIT: Duration = Duration::from_secs(20);
    const BODY: &str = "the parser review is ready";
    const SPEC_GONE_KEPT_FOR: Duration = Duration::from_millis(1000);
    const STATUS_POLL: Duration = Duration::from_millis(50);
    const DIRECT_REQUEST_WAIT: Duration = Duration::from_secs(15);
    const MESSAGE_ID: wire::MessageId = [9; 16];
    const SENT_BEFORE_THE_DEPARTURE: &str = "sent before the departure";
    const SENT_AFTER_THE_DEPARTURE: &str = "sent after the departure";
    const SENT_BEFORE_THE_NEW_PRESENCE: &str = "sent before the new presence";
    const SENT_AFTER_THE_NEW_PRESENCE: &str = "sent after the new presence";
    const PRESENCE_APPLIED_SETTLE: Duration = Duration::from_millis(500);
    const SPEC_OBSERVER_WAIT: Duration = Duration::from_millis(3000);
    const DOCUMENTATION_HOST: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    const SENT_BEFORE_THE_PRESENCE: &str = "sent before the presence";
    const SENT_AFTER_THE_PRESENCE: &str = "sent after the presence";

    fn online(peer: &Value) -> bool {
        peer["status"] == json!("online")
    }

    fn gone(peer: &Value) -> bool {
        peer["status"] == json!("gone")
    }

    /// Two members joined through the host, each listing the other online.
    struct MetPair {
        amy: Member,
        amy_card: Card,
        bob: Member,
        bob_card: Card,
    }

    fn amy_and_bob_met(network: &LocalNetwork, host: &ScriptedNode) -> MetPair {
        let amy_card = network.create_card("amy-card", "amy");
        let bob_card = network.create_card("bob-card", "bob");
        let mut amy = network.start_member();
        amy.init_through(&amy_card, host);
        let mut bob = network.start_member();
        bob.init_through(&bob_card, host);
        amy.peer_within("bob", PRESENCE_WAIT, online);
        bob.peer_within("amy", PRESENCE_WAIT, online);
        MetPair {
            amy,
            amy_card,
            bob,
            bob_card,
        }
    }

    fn send(from: &mut Member, to: &str) -> Value {
        from.call("send", &json!({ "to": to, "body": BODY }))
    }

    /// A member, amy, who listed a scripted bob, then took bob's Departure of that seq and lists bob gone.
    struct ScriptedBobDeparted {
        amy: Member,
        amy_card: Card,
        bob: ScriptedNode,
        departure_seq: u64,
    }

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

    /// The direct side of a scripted member holding a card: it accepts every Message and answers every Introduce with its handle.
    #[derive(Clone)]
    struct ScriptedMemberHost {
        card: wire::PublicKey,
    }

    impl DirectHost for ScriptedMemberHost {
        fn accepted_connections(&self) -> estafette::net::direct::AcceptedConnections {
            estafette::net::direct::AcceptedConnections::new(std::sync::Arc::new(
                tokio::time::Instant::now,
            ))
        }

        fn is_stopping(&self) -> bool {
            false
        }

        fn own_card(&self) -> Option<wire::PublicKey> {
            Some(self.card)
        }

        fn is_attached(&self) -> bool {
            false
        }

        fn receive_message(&self, _sender: wire::PublicKey, _message: Message) {}

        fn receive_introduce(
            &self,
            _sender: wire::PublicKey,
            _introduce: Introduce,
        ) -> wire::IntroduceAck {
            wire::IntroduceAck {
                v: Version::CURRENT,
                handle: Some("bob".to_owned()),
                public: b"{}".to_vec(),
                attached: false,
                addr: Addr::from_endpoint_report(Vec::<String>::new(), []),
                mac: UNSET_MAC,
            }
        }
    }

    fn scripted_member(network: &LocalNetwork, key: &SecretKey) -> ScriptedNode {
        ScriptedNode::start_answering_direct(
            key.clone(),
            *network.keys().gate_key(),
            network.topic(),
            DirectServer::new(
                NetworkKeys::clone(network.keys()),
                ScriptedMemberHost {
                    card: *key.public().as_bytes(),
                },
            ),
        )
    }

    fn loopback_address_of(node: &ScriptedNode) -> iroh::EndpointAddr {
        let address = node.endpoint().endpoint().addr();
        let ports: Vec<u16> = address.ip_addrs().map(std::net::SocketAddr::port).collect();
        loopback_address(address.id, &ports)
    }

    fn address_on_a_host_no_connection_runs_over(node: &ScriptedNode) -> iroh::EndpointAddr {
        let address = node.endpoint().endpoint().addr();
        let sockets: Vec<TransportAddr> = address
            .ip_addrs()
            .map(|socket| TransportAddr::Ip(SocketAddr::new(DOCUMENTATION_HOST, socket.port())))
            .collect();
        iroh::EndpointAddr::from_parts(address.id, sockets)
    }

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

    fn inbox_bodies(member: &mut Member) -> Vec<Value> {
        member.call("inbox", &json!({}))["result"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["body"].clone())
            .collect()
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
        } = amy_and_bob_met(&network, &first_host);
        let first_send = send(&mut amy, "bob");

        bob.kill();
        let killed_ms = wall_clock_ms();
        let second_host = network.stand_in_host();
        second_host.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
        let mut restarted = network.start_member();
        restarted.init_through(&bob_card, &second_host);
        amy.peer_within("bob", PRESENCE_WAIT, |bob| {
            online(bob) && bob["last_seen"].as_u64().unwrap() > killed_ms
        });
        let new_presence_reached = Instant::now();
        let sent = send(&mut amy, "bob");
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
        } = amy_and_bob_met(&network, &host);
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
        let bodies = inbox_bodies(&mut amy);
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
        let first_session = scripted_member(&network, &bob_key);
        let mut amy = network.start_member();
        amy.init_through(&amy_card, &first_session);
        first_session.broadcast(presence_signed_by(
            network.keys(),
            &bob_key,
            "bob",
            &loopback_address_of(&first_session),
        ));
        amy.peer_within("bob", PRESENCE_WAIT, online);
        drop(first_session);

        let restarted = scripted_member(&network, &bob_key);
        restarted.admit(loopback_address(amy_card.id, &amy.bound_udp_ports()));
        let held = restarted
            .runtime()
            .block_on(
                restarted
                    .endpoint()
                    .endpoint()
                    .connect(amy_card.id, DIRECT_ALPN),
            )
            .unwrap();
        let before_presence = exchange_on(
            &network,
            &restarted,
            &held,
            message_from_bob(amy_card.id, wall_clock_ms(), SENT_BEFORE_THE_NEW_PRESENCE),
        );
        announce_until_seen(
            &network,
            &restarted,
            &bob_key,
            &loopback_address_of(&restarted),
            &mut amy,
        );
        std::thread::sleep(PRESENCE_APPLIED_SETTLE);
        let after_presence = exchange_on(
            &network,
            &restarted,
            &held,
            message_from_bob(amy_card.id, wall_clock_ms(), SENT_AFTER_THE_NEW_PRESENCE),
        );
        let bodies = inbox_bodies(&mut amy);
        let sent = send(&mut amy, "bob");

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
        let bob = scripted_member(&network, &bob_key);
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
