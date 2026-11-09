#[cfg(test)]
#[path = "lab/forger.rs"]
mod forger;
#[cfg(test)]
#[path = "lab/forgeries.rs"]
mod forgeries;
#[cfg(test)]
#[path = "lab/lab_machine.rs"]
mod lab_machine;
#[cfg(test)]
#[path = "lab/live_network.rs"]
mod live_network;
#[cfg(test)]
#[path = "lab/tail.rs"]
mod tail;

#[cfg(test)]
mod spec_3_15_test_27_forged_payloads_on_the_live_network {
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    use data_encoding::HEXLOWER;
    use estafette::config::Host;
    use estafette::crypto::sign_host_certificate;
    use estafette::wire::{Payload, SignatureBytes};
    use iroh::SecretKey;

    use crate::forger::Forger;
    use crate::forgeries::{
        infrastructure_record, presence_request, remacked, sealed, unsealed, wall_clock_ms,
        wall_clock_seconds,
    };
    use crate::lab_machine::{ClaimedMember, LabMachine};
    use crate::live_network::LiveNetwork;
    use crate::tail::Tail;

    /// How long a member or `tail` may take to show what a broadcast payload did.
    const ACCEPTANCE_WAIT: Duration = Duration::from_secs(60);
    /// How far a forger moves a counter after the payload was signed.
    const TAMPERING_STEP: u64 = 7;
    /// The relay a replayed host record names, which the host's later record replaced.
    const RELAY_THE_HOST_LEFT: &str = "https://earlier-relay.estafette.invalid/";
    /// Key seed of the forger that sends the record signed by a foreign key.
    const FOREIGN_SIGNER_FORGER_SEED: u8 = 101;
    /// Key seed of the certified host that record names.
    const FOREIGN_SIGNER_HOST_SEED: u8 = 102;
    /// Key seed of the foreign key that signs it.
    const FOREIGN_SIGNER_SEED: u8 = 103;
    /// Key seed of the host whose valid record follows it.
    const FOREIGN_SIGNER_MARKER_SEED: u8 = 104;
    /// Key seed of the member, also its forger, that sends a record naming itself as host.
    const UNCERTIFIED_MEMBER_SEED: u8 = 111;
    /// Key seed of an owner the network never had, whose certificate that record carries.
    const ROGUE_OWNER_SEED: u8 = 112;
    /// Key seed of the host whose valid record follows it.
    const UNCERTIFIED_MARKER_SEED: u8 = 113;
    /// Key seed of the forger that sends the tampered record.
    const TAMPERED_FORGER_SEED: u8 = 121;
    /// Key seed of the certified host whose signed record it tampers.
    const TAMPERED_HOST_SEED: u8 = 122;
    /// Key seed of the host whose valid record follows it.
    const TAMPERED_MARKER_SEED: u8 = 123;
    /// Key seed of the forger that sends the tampered and the valid presence request.
    const TAMPERED_REQUESTER_SEED: u8 = 131;
    /// Key seed of the forger that replays an earlier record.
    const REPLAY_FORGER_SEED: u8 = 141;
    /// Key seed of the certified host whose earlier record it replays.
    const REPLAYED_HOST_SEED: u8 = 142;
    /// Key seed of the host whose valid record follows the replay.
    const REPLAY_MARKER_SEED: u8 = 143;
    /// Key seed of the forger that sends the valid record.
    const VALID_FORGER_SEED: u8 = 151;
    /// Key seed of the certified host that record names.
    const VALID_HOST_SEED: u8 = 152;

    /// Lets one test at a time join the live host, whose gossip active view holds five neighbors (the `HyParView` default of iroh-gossip), so concurrent tests never evict each other's member or forger from it.
    static LIVE_HOST_SLOT: Mutex<()> = Mutex::new(());

    /// Waits for the live host to be free of every other test of this target.
    fn live_host_to_itself() -> MutexGuard<'static, ()> {
        LIVE_HOST_SLOT
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The key every byte of whose seed is the seed.
    fn key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; 32])
    }

    /// The key's node id as the network configuration writes it.
    fn id_text(key: &SecretKey) -> String {
        HEXLOWER.encode(key.public().as_bytes())
    }

    /// A member claimed on a machine of its own, and a forger, both joined to the live network through its host.
    struct Scene {
        network: LiveNetwork,
        machine: LabMachine,
        _member: ClaimedMember,
        forger: Forger,
        _live_host: MutexGuard<'static, ()>,
    }

    impl Scene {
        /// Claims a fresh card named for the test on a fresh machine and joins a forger under the seed's key.
        fn joined(card: &str, forger_seed: u8) -> Self {
            let live_host = live_host_to_itself();
            let network = LiveNetwork::from_environment();
            let machine = LabMachine::fresh(card, network.name());
            network.join(&machine.directories());
            machine.run(&["create-card", network.name(), card, card]);
            let member = machine.start_member(card);
            let forger = Forger::join_through(
                key(forger_seed),
                *network.keys().gate_key(),
                network.topic(),
                network.host_address(),
            );
            Self {
                network,
                machine,
                _member: member,
                forger,
                _live_host: live_host,
            }
        }

        /// A record of the host under the live host's relay at the counter, certified by the network owner and not yet sealed.
        fn certified_record(&self, host: &SecretKey, ts: u64) -> Payload {
            infrastructure_record(
                &host.public(),
                self.network.host_relay(),
                ts,
                self.network.certify(&host.public()),
            )
        }

        /// Broadcasts a valid record of the marker host after whatever went before and returns the hosts the member's configuration names once it saved the marker.
        fn hosts_after_marker(&self, marker: &SecretKey) -> Vec<Host> {
            let record = self.certified_record(marker, wall_clock_seconds());
            self.forger
                .broadcast(sealed(self.network.keys(), marker, record));
            self.machine
                .hosts_once_named(&id_text(marker), ACCEPTANCE_WAIT)
        }
    }

    /// How `tail` begins the line of a presence request from the requester carrying the seq (spec 3.13, tail).
    fn presence_request_line_start(requester: &SecretKey, seq: u64) -> String {
        format!("presence-request\t1\t{}\t{seq}\t", id_text(requester))
    }

    /// Whether the hosts include the key's node id.
    fn names(hosts: &[Host], key: &SecretKey) -> bool {
        hosts.iter().any(|host| host.id == id_text(key))
    }

    /// Spec 3.15 test 27: a member saves no host from a record whose signature is by a key other than its `host`.
    #[test]
    fn spec_3_15_test_27_an_infrastructure_record_signed_by_a_key_other_than_its_host_is_dropped() {
        let scene = Scene::joined("u40-foreign-signer", FOREIGN_SIGNER_FORGER_SEED);
        let host = key(FOREIGN_SIGNER_HOST_SEED);
        let signer = key(FOREIGN_SIGNER_SEED);
        let signed_as_its_own_host = infrastructure_record(
            &signer.public(),
            scene.network.host_relay(),
            wall_clock_seconds(),
            scene.network.certify(&host.public()),
        );
        let Payload::Infrastructure(mut record) = unsealed(&sealed(
            scene.network.keys(),
            &signer,
            signed_as_its_own_host,
        )) else {
            panic!("an infrastructure record unseals as one");
        };
        record.host = *host.public().as_bytes();

        scene.forger.broadcast(remacked(
            scene.network.keys(),
            Payload::Infrastructure(record),
        ));
        let hosts = scene.hosts_after_marker(&key(FOREIGN_SIGNER_MARKER_SEED));
        drop(scene);

        assert!(!names(&hosts, &host), "{hosts:?}");
    }

    /// Spec 3.15 test 27: a member saves no host from a record of a member the network owner never certified.
    #[test]
    fn spec_3_15_test_27_an_infrastructure_record_of_a_member_the_owner_never_certified_is_dropped()
    {
        let scene = Scene::joined("u40-uncertified", UNCERTIFIED_MEMBER_SEED);
        let member = key(UNCERTIFIED_MEMBER_SEED);
        let rogue_certificate = SignatureBytes(
            sign_host_certificate(&key(ROGUE_OWNER_SEED), &member.public()).to_bytes(),
        );
        let record = infrastructure_record(
            &member.public(),
            scene.network.host_relay(),
            wall_clock_seconds(),
            rogue_certificate,
        );

        scene
            .forger
            .broadcast(sealed(scene.network.keys(), &member, record));
        let hosts = scene.hosts_after_marker(&key(UNCERTIFIED_MARKER_SEED));
        drop(scene);

        assert!(!names(&hosts, &member), "{hosts:?}");
    }

    /// Spec 3.15 test 27: a member saves no host from a record changed after its host signed it.
    #[test]
    fn spec_3_15_test_27_a_tampered_infrastructure_record_is_dropped() {
        let scene = Scene::joined("u40-tampered", TAMPERED_FORGER_SEED);
        let host = key(TAMPERED_HOST_SEED);
        let record = scene.certified_record(&host, wall_clock_seconds());
        let Payload::Infrastructure(mut tampered) =
            unsealed(&sealed(scene.network.keys(), &host, record))
        else {
            panic!("an infrastructure record unseals as one");
        };
        tampered.ts = tampered.ts.checked_add(TAMPERING_STEP).unwrap();

        scene.forger.broadcast(remacked(
            scene.network.keys(),
            Payload::Infrastructure(tampered),
        ));
        let hosts = scene.hosts_after_marker(&key(TAMPERED_MARKER_SEED));
        drop(scene);

        assert!(!names(&hosts, &host), "{hosts:?}");
    }

    /// Spec 3.15 test 27, for the fifth payload: `tail` prints no presence request changed after its requester signed it.
    #[test]
    fn spec_3_15_test_27_a_tampered_presence_request_is_dropped() {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let observer = LabMachine::fresh("u40-tail", network.name());
        network.join(&observer.directories());
        let mut tail_command = observer.command();
        tail_command.args(["tail", network.name()]);
        let tail = Tail::start(tail_command);
        let requester = key(TAMPERED_REQUESTER_SEED);
        let forger = Forger::join_through(
            key(TAMPERED_REQUESTER_SEED),
            *network.keys().gate_key(),
            network.topic(),
            network.host_address(),
        );
        let signed_seq = wall_clock_ms();
        let tampered_seq = signed_seq.checked_add(TAMPERING_STEP).unwrap();
        let valid_seq = tampered_seq.checked_add(TAMPERING_STEP).unwrap();
        let Payload::PresenceRequest(mut tampered) = unsealed(&sealed(
            network.keys(),
            &requester,
            presence_request(&requester.public(), signed_seq),
        )) else {
            panic!("a presence request unseals as one");
        };
        tampered.seq = tampered_seq;
        let tampered_line_start = presence_request_line_start(&requester, tampered_seq);
        let valid_line_start = presence_request_line_start(&requester, valid_seq);

        forger.broadcast(remacked(network.keys(), Payload::PresenceRequest(tampered)));
        forger.broadcast(sealed(
            network.keys(),
            &requester,
            presence_request(&requester.public(), valid_seq),
        ));
        let printed = tail.lines_until(ACCEPTANCE_WAIT, |line| line.starts_with(&valid_line_start));

        assert!(
            !printed
                .iter()
                .any(|line| line.starts_with(&tampered_line_start)),
            "{printed:?}"
        );
    }

    /// Spec 3.15 test 27: a host's earlier record with a `ts` below the last accepted leaves the saved host as the newer record left it.
    #[test]
    fn spec_3_15_test_27_a_replayed_infrastructure_record_with_a_ts_not_newer_than_the_last_accepted_is_dropped()
     {
        let scene = Scene::joined("u40-replay", REPLAY_FORGER_SEED);
        let host = key(REPLAYED_HOST_SEED);
        let accepted_ts = wall_clock_seconds();
        let certificate = scene.network.certify(&host.public());
        let earlier = sealed(
            scene.network.keys(),
            &host,
            infrastructure_record(
                &host.public(),
                RELAY_THE_HOST_LEFT,
                accepted_ts.checked_sub(1).unwrap(),
                certificate,
            ),
        );
        let accepted = sealed(
            scene.network.keys(),
            &host,
            infrastructure_record(
                &host.public(),
                scene.network.host_relay(),
                accepted_ts,
                certificate,
            ),
        );
        scene.forger.broadcast(accepted);
        scene
            .machine
            .hosts_once_named(&id_text(&host), ACCEPTANCE_WAIT);

        scene.forger.broadcast(earlier);
        let hosts = scene.hosts_after_marker(&key(REPLAY_MARKER_SEED));

        let saved = hosts.iter().find(|saved| saved.id == id_text(&host));
        assert_eq!(
            saved.map(|saved| (saved.relay.as_str(), saved.ts)),
            Some((scene.network.host_relay(), Some(accepted_ts))),
            "{hosts:?}"
        );
    }

    /// Spec 3.15 test 27: a member saves the host of a valid record with its relay and `ts`.
    #[test]
    fn spec_3_15_test_27_a_valid_infrastructure_record_is_accepted() {
        let scene = Scene::joined("u40-valid", VALID_FORGER_SEED);
        let host = key(VALID_HOST_SEED);
        let ts = wall_clock_seconds();
        let record = scene.certified_record(&host, ts);

        scene
            .forger
            .broadcast(sealed(scene.network.keys(), &host, record));
        let hosts = scene
            .machine
            .hosts_once_named(&id_text(&host), ACCEPTANCE_WAIT);

        let saved = hosts.iter().find(|saved| saved.id == id_text(&host));
        assert_eq!(
            saved.map(|saved| (saved.relay.as_str(), saved.ts)),
            Some((scene.network.host_relay(), Some(ts))),
            "{hosts:?}"
        );
    }
}
