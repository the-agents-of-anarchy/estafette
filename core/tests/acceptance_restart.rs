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

    use serde_json::{Value, json};

    use crate::local_network::LocalNetwork;
    use crate::machine::Card;
    use crate::member::{Member, loopback_address};
    use crate::payloads::wall_clock_ms;
    use crate::scripted_node::ScriptedNode;

    const SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE: Duration = Duration::from_millis(1000);
    const PRESENCE_WAIT: Duration = Duration::from_secs(20);
    const BODY: &str = "the parser review is ready";

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
}
