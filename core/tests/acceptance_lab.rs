#[cfg(test)]
#[path = "lab/forger.rs"]
mod forger;
#[cfg(test)]
#[path = "lab/forgeries.rs"]
mod forgeries;
#[cfg(test)]
#[path = "lab/killable_host.rs"]
mod killable_host;
#[cfg(test)]
#[path = "lab/lab_machine.rs"]
mod lab_machine;
#[cfg(test)]
#[path = "lab/live_network.rs"]
mod live_network;
#[cfg(test)]
#[path = "support/mcp_server.rs"]
mod mcp_server;
#[cfg(test)]
#[path = "support/payload_placeholders.rs"]
mod payload_placeholders;
#[cfg(test)]
#[path = "support/scratch_machine.rs"]
mod scratch_machine;
#[cfg(test)]
#[path = "lab/tail.rs"]
mod tail;

#[cfg(test)]
mod live_host {
    use std::sync::{Mutex, MutexGuard, PoisonError};

    /// Lets one test at a time join the live host, whose gossip active view holds five neighbors (the `HyParView` default of iroh-gossip), so concurrent tests never evict each other's member or forger from it.
    static LIVE_HOST_SLOT: Mutex<()> = Mutex::new(());

    /// Waits for the live host to be free of every other test of this target.
    pub fn live_host_to_itself() -> MutexGuard<'static, ()> {
        LIVE_HOST_SLOT
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod spec_3_15_test_27_forged_payloads_on_the_live_network {
    use std::sync::MutexGuard;
    use std::time::Duration;

    use data_encoding::HEXLOWER;
    use estafette::config::Host;
    use estafette::crypto::sign_host_certificate;
    use estafette::wire::{Payload, SignatureBytes};
    use iroh::SecretKey;

    use crate::forger::Forger;
    use crate::forgeries::{
        infrastructure_record, presence_request, remacked, sealed, unsealed, wall_clock_seconds,
    };
    use crate::lab_machine::{ClaimedMember, LabMachine};
    use crate::live_host::live_host_to_itself;
    use crate::live_network::LiveNetwork;
    use crate::payload_placeholders::wall_clock_ms;
    use crate::tail::Tail;

    /// How long a member or `tail` may take to show what a broadcast payload did.
    const ACCEPTANCE_WAIT: Duration = Duration::from_secs(60);
    /// How far a forger moves a counter after the payload was signed.
    const TAMPERING_STEP: u64 = 7;
    /// How long a forgery broadcast before its marker may still be on its way after the marker arrived, since gossip orders no delivery across neighbors; chosen, not measured.
    const FORGERY_SETTLE: Duration = Duration::from_secs(5);
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
    /// Key seed of the forger that sends the unedited record whose mac `remacked` computed.
    const REMACKED_FORGER_SEED: u8 = 161;
    /// Key seed of the certified host that record names.
    const REMACKED_HOST_SEED: u8 = 162;

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

        /// Broadcasts a valid record of the marker host after whatever went before, waits until the member saved the marker and then the forgery settle, and returns the hosts its configuration names then, so a forgery delivered after the marker by another gossip path is read too.
        fn hosts_after_marker(&self, marker: &SecretKey) -> Vec<Host> {
            let record = self.certified_record(marker, wall_clock_seconds());
            self.forger
                .broadcast(sealed(self.network.keys(), marker, record));
            self.machine
                .hosts_once_named(&id_text(marker), ACCEPTANCE_WAIT);
            std::thread::sleep(FORGERY_SETTLE);
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
        let mut printed =
            tail.lines_until(ACCEPTANCE_WAIT, |line| line.starts_with(&valid_line_start));
        printed.extend(tail.lines_during(FORGERY_SETTLE));

        assert!(
            !printed
                .iter()
                .any(|line| line.starts_with(&tampered_line_start)),
            "{printed:?}"
        );
    }

    /// Spec 3.15 test 27: a host's records with a `ts` below or equal to the last accepted leave the saved host as the accepted record left it.
    #[test]
    fn spec_3_15_test_27_a_replayed_infrastructure_record_with_a_ts_not_newer_than_the_last_accepted_is_dropped()
     {
        let scene = Scene::joined("u40-replay", REPLAY_FORGER_SEED);
        let host = key(REPLAYED_HOST_SEED);
        let accepted_ts = wall_clock_seconds();
        let certificate = scene.network.certify(&host.public());
        let replayed_below = sealed(
            scene.network.keys(),
            &host,
            infrastructure_record(
                &host.public(),
                RELAY_THE_HOST_LEFT,
                accepted_ts.checked_sub(1).unwrap(),
                certificate,
            ),
        );
        let replayed_at_the_accepted_ts = sealed(
            scene.network.keys(),
            &host,
            infrastructure_record(
                &host.public(),
                RELAY_THE_HOST_LEFT,
                accepted_ts,
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

        scene.forger.broadcast(replayed_below);
        scene.forger.broadcast(replayed_at_the_accepted_ts);
        let hosts = scene.hosts_after_marker(&key(REPLAY_MARKER_SEED));

        let saved = hosts.iter().find(|saved| saved.id == id_text(&host));
        assert_eq!(
            saved.map(|saved| (saved.relay.as_str(), saved.ts)),
            Some((scene.network.host_relay(), Some(accepted_ts))),
            "{hosts:?}"
        );
    }

    /// Spec 3.15 test 27, control for the forged cases: a sealed record left unedited, its mac computed afresh by `remacked`, is saved, so a forgery built with `remacked` passes the mac and reaches the check its case names.
    #[test]
    fn spec_3_15_test_27_a_remacked_unedited_record_is_accepted() {
        let scene = Scene::joined("u40-remacked", REMACKED_FORGER_SEED);
        let host = key(REMACKED_HOST_SEED);
        let ts = wall_clock_seconds();
        let record = scene.certified_record(&host, ts);
        let unedited = unsealed(&sealed(scene.network.keys(), &host, record));

        scene
            .forger
            .broadcast(remacked(scene.network.keys(), unedited));
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

#[cfg(test)]
mod spec_3_15_test_28_addresses_on_the_live_network {
    use std::time::{Duration, Instant};

    use data_encoding::HEXLOWER;
    use estafette::wire::{HostAddr, Version};
    use serde_json::Value;

    use crate::killable_host::KillableHost;
    use crate::lab_machine::{ClaimedMember, LabMachine};
    use crate::live_host::live_host_to_itself;
    use crate::live_network::LiveNetwork;
    use crate::tail::Tail;

    /// How long the member may take to announce an address naming the doomed host's relay, and to announce after the kill at all.
    const ANNOUNCEMENT_WAIT: Duration = Duration::from_secs(60);
    /// Spec 3.15 test 28: the bound within which a member's address lists only live relays after its home relay is killed.
    const SPEC_LIVE_RELAYS_WITHIN: Duration = Duration::from_millis(2000);
    /// The re-announce interval of spec:777, which holds any address change for this long after the member's last Presence.
    const SPEC_REANNOUNCE_INTERVAL: Duration = Duration::from_millis(2000);
    /// How long the member may take to fall quiet for a re-announce interval before the kill.
    const PRESENCE_QUIET_WAIT: Duration = Duration::from_secs(60);
    /// The card and handle of the member homed on the doomed host.
    const MEMBER_CARD: &str = "u40-relay-loss";
    /// The scratch tag of the machine whose `tail` observes the member.
    const OBSERVER_TAG: &str = "u40-relay-tail";
    /// How many times a member is started before the test gives up on iroh homing it on the doomed host's relay, which iroh picks among the relays it knows.
    const HOMING_ATTEMPTS: usize = 3;
    /// The kind word that opens a presence line of `tail`.
    const PRESENCE_KIND: &str = "presence";
    /// The column of a presence line holding its card, after the kind and `v` (spec 3.7 order).
    const CARD_COLUMN: usize = 2;
    /// The column of a presence line holding its `addr` as JSON (spec 3.7 order).
    const ADDR_COLUMN: usize = 6;
    /// The kind word that opens an infrastructure line of `tail`.
    const INFRASTRUCTURE_KIND: &str = "infrastructure";
    /// The column of an infrastructure line holding its `relay`, after the kind, `v` and `host` (spec 3.7 order).
    const HOST_RELAY_COLUMN: usize = 3;
    /// The key of the relay list in a printed `addr`.
    const RELAYS_KEY: &str = "relays";
    /// The column separator of a `tail` line.
    const COLUMN_SEPARATOR: char = '\t';

    /// The relay URLs a `tail` line announces for the card, when it is a presence of that card.
    fn relays_announced_by(line: &str, card: &str) -> Option<Vec<String>> {
        let columns: Vec<&str> = line.split(COLUMN_SEPARATOR).collect();
        if columns.first() != Some(&PRESENCE_KIND) || columns.get(CARD_COLUMN) != Some(&card) {
            return None;
        }
        let addr: Value = serde_json::from_str(columns.get(ADDR_COLUMN)?).ok()?;
        addr.get(RELAYS_KEY)?
            .as_array()?
            .iter()
            .map(|relay| relay.as_str().map(str::to_owned))
            .collect()
    }

    /// Whether a `tail` line is an infrastructure record of the host whose relay is the one given.
    fn is_a_record_of_the_host_relaying_through(line: &str, relay: &str) -> bool {
        let columns: Vec<&str> = line.split(COLUMN_SEPARATOR).collect();
        columns.first() == Some(&INFRASTRUCTURE_KIND)
            && columns.get(HOST_RELAY_COLUMN) == Some(&relay)
    }

    /// The member as a node to join the topic through, so the observer is its gossip neighbor whichever other neighbors the topic gives it.
    fn member_reached_through_its_home_relay(card: &str, doomed: &KillableHost) -> HostAddr {
        HostAddr {
            v: Version::CURRENT,
            id: HEXLOWER
                .decode(card.as_bytes())
                .unwrap()
                .try_into()
                .unwrap(),
            relay: doomed.address().relay.clone(),
        }
    }

    /// A member homed on the doomed host's relay, with a `tail` joined through it alone.
    struct HomedMember {
        card: String,
        tail: Tail,
        _member: ClaimedMember,
        _observer: LabMachine,
        _machine: LabMachine,
    }

    /// Starts a member whose only host is the doomed one and a `tail` joined through it, returning them once its first announced relays are the doomed host's alone and it has relayed the live host's record, or nothing when iroh homed it elsewhere.
    fn start_member_homed_on(network: &LiveNetwork, doomed: &KillableHost) -> Option<HomedMember> {
        let machine = LabMachine::fresh(MEMBER_CARD, network.name());
        network.join_through(
            &machine.directories(),
            std::slice::from_ref(doomed.address()),
        );
        let card = machine
            .run(&["create-card", network.name(), MEMBER_CARD, MEMBER_CARD])
            .trim_end()
            .to_owned();
        let started = Instant::now();
        let member = machine.start_member(MEMBER_CARD);
        let observer = LabMachine::fresh(OBSERVER_TAG, network.name());
        network.join_through(
            &observer.directories(),
            &[member_reached_through_its_home_relay(&card, doomed)],
        );
        let mut tail_command = observer.command();
        tail_command.args(["tail", network.name()]);
        let tail = Tail::start(tail_command);
        let (first_homed, _) = tail.first_line_after(started, ANNOUNCEMENT_WAIT, |line| {
            relays_announced_by(line, &card).is_some_and(|relays| !relays.is_empty())
        });
        if relays_announced_by(&first_homed, &card) != Some(vec![doomed.relay().to_owned()]) {
            return None;
        }
        tail.first_line_after(Instant::now(), ANNOUNCEMENT_WAIT, |line| {
            is_a_record_of_the_host_relaying_through(line, network.host_relay())
        });
        Some(HomedMember {
            card,
            tail,
            _member: member,
            _observer: observer,
            _machine: machine,
        })
    }

    /// Spec 3.15 test 28: within 2000 ms of the kill of a member's home relay, the member announces an address whose relays are the live host's alone, the kill waiting until the member's last Presence is a re-announce interval old so spec:777 cannot hold that announcement past the bound.
    #[test]
    fn spec_3_15_test_28_within_2000_ms_of_its_home_relays_death_a_member_lists_only_live_relays() {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let doomed = KillableHost::from_environment();
        let homed = (0..HOMING_ATTEMPTS)
            .find_map(|_| start_member_homed_on(&network, &doomed))
            .unwrap_or_else(|| {
                panic!("iroh homed the member on another relay than {} in each of {HOMING_ATTEMPTS} starts", doomed.relay())
            });
        homed
            .tail
            .quiet_for(SPEC_REANNOUNCE_INTERVAL, PRESENCE_QUIET_WAIT, |line| {
                relays_announced_by(line, &homed.card).is_some()
            });

        let kill = doomed.kill();
        let mut announced_after_the_kill = Vec::new();
        let after_the_kill = loop {
            let (line, after) = homed
                .tail
                .first_line_after(kill.sent, ANNOUNCEMENT_WAIT, |line| {
                    relays_announced_by(line, &homed.card).is_some()
                });
            let lists_only_the_live_relay =
                relays_announced_by(&line, &homed.card).is_some_and(|relays| {
                    !relays.is_empty() && relays.iter().all(|relay| relay == network.host_relay())
                });
            announced_after_the_kill.push(format!("{after:?} {line}"));
            if lists_only_the_live_relay {
                break after;
            }
        };

        assert!(
            after_the_kill <= SPEC_LIVE_RELAYS_WITHIN,
            "no presence listing only live relays within {SPEC_LIVE_RELAYS_WITHIN:?} of the kill sent at {:?}, whose command returned at {:?}, {:?} later; presences after the send: {announced_after_the_kill:#?}",
            kill.sent,
            kill.returned,
            kill.returned.duration_since(kill.sent)
        );
    }
}
