#[cfg(test)]
#[path = "one_machine/local_network.rs"]
mod local_network;
#[cfg(test)]
#[path = "one_machine/machine.rs"]
mod machine;
#[cfg(test)]
#[path = "one_machine/member.rs"]
mod member;
#[cfg(test)]
#[path = "one_machine/payloads.rs"]
mod payloads;
#[cfg(test)]
#[path = "one_machine/scripted_node.rs"]
mod scripted_node;

#[cfg(test)]
mod spec_3_15_restart_and_departure_on_one_machine {
    use std::time::{Duration, Instant};

    use estafette::net::endpoint::{DIRECT_ALPN, GateFailure};
    use estafette::payload::{open_response, seal_request};
    use estafette::wire::{self, Message, Request, Response, Version, WIRE_LIMIT_BYTES};
    use iroh::{EndpointId, SecretKey};
    use serde_json::{Value, json};

    use crate::local_network::LocalNetwork;
    use crate::machine::Card;
    use crate::member::{Member, loopback_address};
    use crate::payloads::{UNSET_MAC, departure_signed_by, presence_signed_by, wall_clock_ms};
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
        let answer = node.runtime().block_on(async {
            tokio::time::timeout(DIRECT_REQUEST_WAIT, async {
                let connection = node
                    .endpoint()
                    .endpoint()
                    .connect(to, DIRECT_ALPN)
                    .await
                    .unwrap();
                let (mut send, mut receive) = connection.open_bi().await.unwrap();
                send.write_all(&sealed).await.unwrap();
                send.finish().unwrap();
                receive.read_to_end(WIRE_LIMIT_BYTES).await.unwrap()
            })
            .await
            .unwrap()
        });
        open_response(network.keys(), &answer).unwrap()
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
}
