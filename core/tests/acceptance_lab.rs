#[cfg(test)]
#[path = "support/driven_runtime.rs"]
mod driven_runtime;
#[cfg(test)]
#[path = "lab/forger.rs"]
mod forger;
#[cfg(test)]
#[path = "lab/forgeries.rs"]
mod forgeries;
#[cfg(test)]
#[path = "lab/inbox_client.rs"]
mod inbox_client;
#[cfg(test)]
#[path = "lab/killable_host.rs"]
mod killable_host;
#[cfg(test)]
#[path = "lab/lab_machine.rs"]
mod lab_machine;
#[cfg(test)]
#[path = "lab/lab_server.rs"]
mod lab_server;
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
#[path = "support/payloads.rs"]
mod payloads;
#[cfg(test)]
#[path = "support/scratch_machine.rs"]
mod scratch_machine;
#[cfg(test)]
#[path = "lab/server_member.rs"]
mod server_member;
#[cfg(test)]
#[path = "lab/spec_3_15_tests_across_lab_servers.rs"]
mod spec_3_15_tests_across_lab_servers;
#[cfg(test)]
#[path = "support/spec_bounds.rs"]
mod spec_bounds;
#[cfg(test)]
#[path = "lab/tail.rs"]
mod tail;
#[cfg(test)]
#[path = "support/timed_step.rs"]
mod timed_step;

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
        #[track_caller]
        fn joined(card: &str, forger_seed: u8) -> Self {
            let live_host = live_host_to_itself();
            let network = LiveNetwork::from_environment();
            let machine = LabMachine::joined(card, &network);
            let created = machine.create_card(card, card);
            let member = machine.start_member(&created.name);
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
        #[track_caller]
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
    use estafette::keys::render_public_key;
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
    #[track_caller]
    fn start_member_homed_on(network: &LiveNetwork, doomed: &KillableHost) -> Option<HomedMember> {
        let machine = LabMachine::fresh(MEMBER_CARD, network.name());
        network.join_through(
            &machine.directories(),
            std::slice::from_ref(doomed.address()),
        );
        let card = render_public_key(&machine.create_card(MEMBER_CARD, MEMBER_CARD).id);
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

#[cfg(test)]
mod spec_3_15_two_members_through_the_live_host {
    use std::fs;
    use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use estafette::keys::parse_secret_key;
    use estafette::wire::{self, Addr, Payload, Presence, SignatureBytes, Version};
    use iroh::{EndpointAddr, SecretKey, TransportAddr};
    use serde_json::{Value, json};

    use crate::forger::Forger;
    use crate::inbox_client::InboxClient;
    use crate::lab_machine::LabMachine;
    use crate::live_host::live_host_to_itself;
    use crate::live_network::LiveNetwork;
    use crate::mcp_server::{McpServer, PRESENCE_WAIT, gone, online};
    use crate::payload_placeholders::{UNSET_MAC, wall_clock_ms};
    use crate::payloads::presence_signed_by;
    use crate::scratch_machine::Card;
    use crate::spec_bounds::SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE;
    use crate::timed_step::timed;

    const SPEC_GONE_WITHIN: Duration = Duration::from_millis(1000);
    const SPEC_QUIC_IDLE_TIMEOUT: Duration = Duration::from_millis(10_000);
    const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(500);
    const SPEC_PROBE_TIMEOUT: Duration = Duration::from_millis(3000);
    const SPEC_LARGEST_PRESENCE_BYTES: usize = 3951;
    const SPEC_GOSSIP_FRAME_BYTES: usize = 4096;
    const SPEC_GOSSIP_LAYER_OVERHEAD_BYTES: usize = 38;
    const SPEC_PUBLIC_MAX_BYTES: usize = 3072;
    const SPEC_ANNOUNCED_HANDLE_MAX_CHARACTERS: usize = 53;
    const SPEC_REQUESTED_HANDLE_MAX_CHARACTERS: usize = 32;
    const SPEC_RELAY_URLS_PER_ADDR: u16 = 4;
    const SPEC_RELAY_URL_MAX_BYTES: usize = 128;
    const SPEC_DIRECT_ADDRS_PER_ADDR: u16 = 8;
    const LARGEST_THREE_BYTE_VARINT_PORT: u16 = 65_535;
    const HANDLE_TAG_LETTERS: usize = 6;
    const ALPHABET_LENGTH: u64 = 26;
    const FIRST_LETTER: u8 = b'a';
    const RELAY_URL_FRAME_BYTES: usize = "https://".len() + "/".len();
    const DOCUMENTATION_PREFIX_FIRST_SEGMENT: u16 = 0x2001;
    const DOCUMENTATION_PREFIX_SECOND_SEGMENT: u16 = 0x0db8;
    const PUBLIC_NOTES_KEY: &str = "notes";
    const COMPACT_JSON_MAPPING_FRAME_BYTES: usize = r#"{"":""}"#.len();
    const FRAME_WAIT: Duration = Duration::from_secs(10);
    const NO_FURTHER_FRAME_WAIT: Duration = Duration::from_millis(1000);
    const REPLAY_SETTLE_WAIT: Duration = Duration::from_millis(2000);
    const FIRST_BODY: &str = "the parser review is ready";
    const FIRST_SESSION: &str = "first session";
    const SECOND_SESSION: &str = "second session";
    const MESSAGES_BEFORE_RECONNECT: u64 = 3;
    const MESSAGES_IN_ALL: u64 = 5;

    /// A member on its own machine, joined to the live network with its card claimed.
    struct LabMember {
        member: McpServer,
        card: Card,
        handle: String,
        claim: Value,
        machine: LabMachine,
    }

    /// Claims a fresh card requesting the handle, with the description, on a fresh machine joined to the live network.
    #[track_caller]
    fn lab_member(
        network: &LiveNetwork,
        machine_tag: &str,
        handle: &str,
        description: &str,
    ) -> LabMember {
        let machine = LabMachine::joined(machine_tag, network);
        let card = machine.create_card(&format!("{machine_tag}-card"), handle);
        let mut member = machine.start_server();
        let claim = member.call(
            "init",
            &json!({ "card": card.name, "description": description }),
        );
        assert_eq!(claim["ok"], json!(true), "{claim}");
        LabMember {
            member,
            card,
            handle: handle.to_owned(),
            claim,
            machine,
        }
    }

    /// The base followed by letters drawn from the clock, so the handle is free on the live network whatever earlier runs announced.
    fn unique_handle(base: &str) -> String {
        let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let mut drawn = u64::from(since_epoch.subsec_nanos())
            ^ since_epoch.as_secs()
            ^ u64::from(std::process::id());
        let mut handle = base.to_owned();
        for _ in 0..HANDLE_TAG_LETTERS {
            let letter = u8::try_from(drawn % ALPHABET_LENGTH).unwrap();
            handle.push(char::from(FIRST_LETTER.checked_add(letter).unwrap()));
            drawn /= ALPHABET_LENGTH;
        }
        handle
    }

    #[track_caller]
    fn meet(first: &mut LabMember, second: &mut LabMember) {
        first
            .member
            .peer_within(&second.handle, PRESENCE_WAIT, online);
        second
            .member
            .peer_within(&first.handle, PRESENCE_WAIT, online);
    }

    #[track_caller]
    fn send(from: &mut LabMember, to: &str, body: &str) -> Value {
        from.member.call("send", &json!({ "to": to, "body": body }))
    }

    #[track_caller]
    fn listed_peer(member: &mut McpServer, handle: &str) -> Option<Value> {
        member.call("peers", &json!({}))["result"]["peers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|peer| peer["handle"] == handle)
            .cloned()
    }

    /// Adds the public mapping, written as YAML, to the card's file.
    fn add_public_mapping(
        network: &LiveNetwork,
        machine: &LabMachine,
        card: &Card,
        public_yaml: &str,
    ) {
        let card_file = machine
            .directories()
            .card_file(network.name(), &card.name)
            .unwrap();
        let text = fs::read_to_string(&card_file).unwrap();
        fs::write(card_file, format!("{text}{public_yaml}")).unwrap();
    }

    /// The card's secret key, read from the sidecar the binary wrote beside it.
    fn card_key(network: &LiveNetwork, machine: &LabMachine, card: &Card) -> SecretKey {
        let sidecar = machine
            .directories()
            .card_sidecar_file(network.name(), &card.name)
            .unwrap();
        parse_secret_key(&fs::read_to_string(sidecar).unwrap()).unwrap()
    }

    fn largest_presence() -> Payload {
        let relays = (0..SPEC_RELAY_URLS_PER_ADDR).map(|relay| {
            let host_bytes = SPEC_RELAY_URL_MAX_BYTES - RELAY_URL_FRAME_BYTES;
            format!("https://{relay}{}/", "r".repeat(host_bytes - 1))
        });
        let direct = (0..SPEC_DIRECT_ADDRS_PER_ADDR).map(|segment| {
            SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::new(
                    DOCUMENTATION_PREFIX_FIRST_SEGMENT,
                    DOCUMENTATION_PREFIX_SECOND_SEGMENT,
                    0,
                    0,
                    0,
                    0,
                    0,
                    segment.checked_add(1).unwrap(),
                ),
                LARGEST_THREE_BYTE_VARINT_PORT,
                0,
                0,
            ))
        });
        Payload::Presence(Presence {
            v: Version::CURRENT,
            card: [u8::MAX; 32],
            handle: "h".repeat(SPEC_ANNOUNCED_HANDLE_MAX_CHARACTERS),
            public: vec![b'p'; SPEC_PUBLIC_MAX_BYTES],
            attached: true,
            addr: Addr::from_endpoint_report(relays, direct),
            seq: u64::MAX,
            mac: UNSET_MAC,
            sig: SignatureBytes([u8::MAX; 64]),
        })
    }

    #[test]
    fn spec_3_15_test_1_a_message_to_a_member_whose_inbox_socket_has_a_client_is_delivered_attached_as_one_frame()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = lab_member(&network, "t1-amy", &unique_handle("amy"), FIRST_SESSION);
        let mut bob = lab_member(&network, "t1-bob", &unique_handle("bob"), FIRST_SESSION);
        meet(&mut amy, &mut bob);
        let mut inbox = InboxClient::connect(&bob.claim["result"]["inbox_path"], 0);
        amy.member.peer_within(&bob.handle, PRESENCE_WAIT, |bob| {
            bob["attached"] == json!(true)
        });

        let sent = send(&mut amy, &bob.handle.clone(), FIRST_BODY);

        let id = sent["result"]["id"].as_str().unwrap();
        assert_eq!(
            sent["result"],
            json!({ "id": id, "delivered": true, "attached": true })
        );
        let frame = inbox.next_frame(FRAME_WAIT).unwrap();
        assert_eq!(frame["seq"], json!(1));
        assert_eq!(frame["id"], json!(id));
        assert_eq!(frame["from"], json!(amy.handle));
        assert_eq!(frame["body"], json!(FIRST_BODY));
        assert_eq!(inbox.next_frame(NO_FURTHER_FRAME_WAIT), None);
    }

    #[test]
    fn spec_3_15_test_2_a_message_to_a_member_with_no_inbox_client_is_delivered_unattached_and_its_inbox_tool_returns_it()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = lab_member(&network, "t2-amy", &unique_handle("amy"), FIRST_SESSION);
        let mut bob = lab_member(&network, "t2-bob", &unique_handle("bob"), FIRST_SESSION);
        meet(&mut amy, &mut bob);

        let sent = send(&mut amy, &bob.handle.clone(), FIRST_BODY);
        let read = bob.member.call("inbox", &json!({}));

        let id = sent["result"]["id"].as_str().unwrap();
        assert_eq!(
            sent["result"],
            json!({ "id": id, "delivered": true, "attached": false })
        );
        assert_eq!(read["result"]["last_seq"], json!(1));
        let messages = read["result"]["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["id"], json!(id));
        assert_eq!(messages[0]["from"], json!(amy.handle));
        assert_eq!(messages[0]["body"], json!(FIRST_BODY));
    }

    #[track_caller]
    fn assert_stopped_member_is_gone_then_unreachable(
        amy: &mut LabMember,
        bob: &mut LabMember,
        input_closed: Instant,
    ) {
        amy.member.peer_within(&bob.handle, SPEC_GONE_WITHIN, gone);
        let gone_after = input_closed.elapsed();
        let exit = bob.member.wait_for_exit();
        let sent = send(amy, &bob.handle.clone(), FIRST_BODY);

        assert!(gone_after <= SPEC_GONE_WITHIN, "{gone_after:?}");
        assert!(exit.success(), "{exit:?}");
        assert_eq!(sent["error"]["code"], json!("unreachable"), "{sent}");
    }

    #[test]
    fn spec_3_15_test_4_a_member_stopped_by_the_end_of_input_is_gone_at_its_peer_within_1000_ms_and_unreachable()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = lab_member(&network, "t4-amy", &unique_handle("amy"), FIRST_SESSION);
        let mut bob = lab_member(&network, "t4-bob", &unique_handle("bob"), FIRST_SESSION);
        meet(&mut amy, &mut bob);

        let input_closed = Instant::now();
        bob.member.close_standard_input();

        assert_stopped_member_is_gone_then_unreachable(&mut amy, &mut bob, input_closed);
    }

    #[test]
    fn spec_3_15_test_4_a_member_stopped_with_a_tool_call_in_flight_is_gone_at_its_peer_within_1000_ms_and_unreachable()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = lab_member(&network, "t4f-amy", &unique_handle("amy"), FIRST_SESSION);
        let mut bob = lab_member(&network, "t4f-bob", &unique_handle("bob"), FIRST_SESSION);
        meet(&mut amy, &mut bob);

        bob.member
            .start_call("send", &json!({ "to": amy.handle, "body": FIRST_BODY }));
        let input_closed = Instant::now();
        bob.member.close_standard_input();

        assert_stopped_member_is_gone_then_unreachable(&mut amy, &mut bob, input_closed);
    }

    #[test]
    fn spec_3_15_test_5_a_killed_member_is_unreachable_at_its_peer_within_one_neighbor_down_and_one_probe_timeout()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = lab_member(&network, "t5-amy", &unique_handle("amy"), FIRST_SESSION);
        let mut bob = lab_member(&network, "t5-bob", &unique_handle("bob"), FIRST_SESSION);
        meet(&mut amy, &mut bob);
        let neighbor_down_and_probe = SPEC_QUIC_IDLE_TIMEOUT
            .checked_add(QUIC_KEEP_ALIVE_INTERVAL)
            .and_then(|neighbor_down| neighbor_down.checked_add(SPEC_PROBE_TIMEOUT))
            .unwrap();

        let killed = Instant::now();
        bob.member.kill();
        amy.member
            .peer_within(&bob.handle, neighbor_down_and_probe, |bob| {
                bob["status"] == json!("unreachable")
            });
        let unreachable_after = killed.elapsed();
        let sent = send(&mut amy, &bob.handle.clone(), FIRST_BODY);

        assert!(
            unreachable_after <= neighbor_down_and_probe,
            "{unreachable_after:?}"
        );
        assert_eq!(sent["error"]["code"], json!("unreachable"), "{sent}");
    }

    #[test]
    fn spec_3_15_test_7_a_second_member_requesting_a_taken_handle_is_h_2_at_every_node_and_keeps_it_after_the_first_restarts()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let taken = unique_handle("amy");
        let second_effective = format!("{taken}-2");
        let mut amy = timed("amy joins and claims its card", || {
            lab_member(&network, "t7-amy", &taken, FIRST_SESSION)
        });
        let mut bob = timed("bob joins and claims its card", || {
            lab_member(&network, "t7-bob", &unique_handle("bob"), FIRST_SESSION)
        });
        timed("amy and bob list each other online", || {
            meet(&mut amy, &mut bob);
        });

        let mut ann = timed(
            "ann joins and claims a card requesting amy's handle",
            || lab_member(&network, "t7-ann", &taken, FIRST_SESSION),
        );
        let at_amy = timed("amy lists ann online as <h>-2", || {
            amy.member
                .peer_within(&second_effective, PRESENCE_WAIT, online)
        });
        let at_bob = timed("bob lists ann online as <h>-2", || {
            bob.member
                .peer_within(&second_effective, PRESENCE_WAIT, online)
        });
        amy.member.close_standard_input();
        assert!(
            timed("amy exits after the end of input", || amy
                .member
                .wait_for_exit())
            .success()
        );
        let mut restarted = timed("the restarted amy starts", || amy.machine.start_server());
        let restarted_claim = timed("the restarted amy claims its card", || {
            restarted.call("init", &json!({ "card": amy.card.name }))
        });
        let restarted_at_ann = timed("ann lists the restarted amy online as <h>", || {
            ann.member.peer_within(&taken, PRESENCE_WAIT, online)
        });
        let own_after_restart = timed("ann's peers tool answers", || {
            ann.member.call("peers", &json!({}))
        });
        let restarted_at_bob = timed("bob lists the restarted amy online as <h>", || {
            bob.member.peer_within(&taken, PRESENCE_WAIT, online)
        });
        let ann_at_restarted = timed("the restarted amy lists ann online as <h>-2", || {
            restarted.peer_within(&second_effective, PRESENCE_WAIT, online)
        });

        assert_eq!(ann.claim["result"]["handle"], json!(second_effective));
        assert_eq!(at_amy["handle"], json!(second_effective));
        assert_eq!(at_bob["handle"], json!(second_effective));
        assert_eq!(restarted_claim["result"]["handle"], json!(taken));
        assert_eq!(restarted_at_ann["handle"], json!(taken));
        assert_eq!(restarted_at_bob["handle"], json!(taken));
        assert_eq!(ann_at_restarted["handle"], json!(second_effective));
        assert_eq!(own_after_restart["you"]["handle"], json!(second_effective));
        assert_eq!(
            timed("bob still lists ann online as <h>-2", || bob
                .member
                .peer_within(&second_effective, PRESENCE_WAIT, online))["handle"],
            json!(second_effective)
        );
    }

    #[test]
    fn spec_3_15_test_8_a_message_from_a_card_whose_presence_was_not_seen_is_accepted_and_lists_its_sender_online()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = timed("amy joins and claims its card", || {
            lab_member(&network, "t8-amy", &unique_handle("amy"), FIRST_SESSION)
        });
        let mut bob = timed("bob joins and claims its card", || {
            lab_member(&network, "t8-bob", &unique_handle("bob"), FIRST_SESSION)
        });
        timed("amy and bob list each other online", || {
            meet(&mut amy, &mut bob);
        });
        amy.member.close_standard_input();
        assert!(
            timed("amy exits after the end of input", || amy
                .member
                .wait_for_exit())
            .success()
        );
        let mut restarted = timed("the restarted amy starts", || amy.machine.start_server());
        let claim = timed("the restarted amy claims its card", || {
            restarted.call("init", &json!({ "card": amy.card.name }))
        });
        timed("bob lists the restarted amy online", || {
            bob.member.peer_within(&amy.handle, PRESENCE_WAIT, online)
        });

        let unseen_before = timed(
            "the restarted amy's peers tool answers before bob's send",
            || listed_peer(&mut restarted, &bob.handle),
        );
        let amy_handle = amy.handle.clone();
        let sent = timed("bob's send to the restarted amy", || {
            send(&mut bob, &amy_handle, FIRST_BODY)
        });
        let listed_after = timed(
            "the restarted amy's peers tool answers after bob's send",
            || listed_peer(&mut restarted, &bob.handle),
        );

        assert_eq!(claim["ok"], json!(true), "{claim}");
        assert_eq!(unseen_before, None);
        assert_eq!(sent["result"]["delivered"], json!(true), "{sent}");
        assert_eq!(listed_after.unwrap()["status"], json!("online"));
    }

    #[test]
    fn spec_3_15_test_11_an_inbox_client_reconnecting_with_since_3_after_5_frames_receives_frames_4_and_5()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = lab_member(&network, "t11-amy", &unique_handle("amy"), FIRST_SESSION);
        let mut bob = lab_member(&network, "t11-bob", &unique_handle("bob"), FIRST_SESSION);
        meet(&mut amy, &mut bob);
        let bob_handle = bob.handle.clone();
        let body_of = |seq: u64| format!("message {seq}");

        let mut first_client = InboxClient::connect(&bob.claim["result"]["inbox_path"], 0);
        for seq in 1..=MESSAGES_BEFORE_RECONNECT {
            send(&mut amy, &bob_handle, &body_of(seq));
            first_client.next_frame(FRAME_WAIT).unwrap();
        }
        drop(first_client);
        for seq in MESSAGES_BEFORE_RECONNECT.checked_add(1).unwrap()..=MESSAGES_IN_ALL {
            send(&mut amy, &bob_handle, &body_of(seq));
        }
        let mut reconnected = InboxClient::connect(
            &bob.claim["result"]["inbox_path"],
            MESSAGES_BEFORE_RECONNECT,
        );
        let replayed = [
            reconnected.next_frame(FRAME_WAIT).unwrap(),
            reconnected.next_frame(FRAME_WAIT).unwrap(),
        ];

        assert_eq!(replayed[0]["seq"], json!(4));
        assert_eq!(replayed[0]["body"], json!(body_of(4)));
        assert_eq!(replayed[1]["seq"], json!(5));
        assert_eq!(replayed[1]["body"], json!(body_of(5)));
        assert_eq!(reconnected.next_frame(NO_FURTHER_FRAME_WAIT), None);
    }

    #[test]
    fn spec_3_15_test_14_a_broadcast_to_three_handles_one_unknown_returns_two_delivered_and_one_unknown_handle()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = lab_member(&network, "t14-amy", &unique_handle("amy"), FIRST_SESSION);
        let mut bob = lab_member(&network, "t14-bob", &unique_handle("bob"), FIRST_SESSION);
        let mut cid = lab_member(&network, "t14-cid", &unique_handle("cid"), FIRST_SESSION);
        meet(&mut amy, &mut bob);
        meet(&mut amy, &mut cid);
        let unknown = unique_handle("nobody");

        let broadcast = amy.member.call(
            "broadcast",
            &json!({ "to": [bob.handle, unknown, cid.handle], "body": FIRST_BODY }),
        );
        let at_bob = bob.member.call("inbox", &json!({}));
        let at_cid = cid.member.call("inbox", &json!({}));

        let results = broadcast["result"]["results"].as_array().unwrap();
        assert_eq!(results.len(), 3, "{broadcast}");
        assert_eq!(results[0]["to"], json!(bob.handle));
        assert_eq!(results[0]["delivered"], json!(true));
        assert_eq!(results[1]["to"], json!(unknown));
        assert_eq!(results[1]["delivered"], json!(false));
        assert_eq!(results[1]["error"]["code"], json!("unknown_handle"));
        assert_eq!(results[2]["to"], json!(cid.handle));
        assert_eq!(results[2]["delivered"], json!(true));
        assert_eq!(at_bob["result"]["messages"][0]["id"], results[0]["id"]);
        assert_eq!(at_cid["result"]["messages"][0]["id"], results[2]["id"]);
    }

    #[test]
    fn spec_3_15_test_17_the_largest_presence_encodes_within_the_gossip_frame_and_the_largest_a_member_sends_arrives()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let largest = wire::encode(&largest_presence()).unwrap();
        let longest_handle =
            unique_handle(&"a".repeat(SPEC_REQUESTED_HANDLE_MAX_CHARACTERS - HANDLE_TAG_LETTERS));
        let notes = "n".repeat(
            SPEC_PUBLIC_MAX_BYTES - COMPACT_JSON_MAPPING_FRAME_BYTES - PUBLIC_NOTES_KEY.len(),
        );
        let machine = LabMachine::joined("t17-amy", &network);
        let card = machine.create_card("t17-amy-card", &longest_handle);
        add_public_mapping(
            &network,
            &machine,
            &card,
            &format!("public:\n  {PUBLIC_NOTES_KEY}: {notes}\n"),
        );
        let mut amy = machine.start_server();
        let claim = amy.call("init", &json!({ "card": card.name }));
        let mut bob = lab_member(&network, "t17-bob", &unique_handle("bob"), FIRST_SESSION);

        let at_bob = bob
            .member
            .peer_within(&longest_handle, PRESENCE_WAIT, online);

        assert_eq!(largest.len(), SPEC_LARGEST_PRESENCE_BYTES);
        assert!(largest.len() <= SPEC_GOSSIP_FRAME_BYTES - SPEC_GOSSIP_LAYER_OVERHEAD_BYTES);
        assert_eq!(claim["ok"], json!(true), "{claim}");
        assert_eq!(longest_handle.len(), SPEC_REQUESTED_HANDLE_MAX_CHARACTERS);
        assert_eq!(at_bob["public"], json!({ PUBLIC_NOTES_KEY: notes }));
        assert_eq!(
            serde_json::to_string(&at_bob["public"]).unwrap().len(),
            SPEC_PUBLIC_MAX_BYTES
        );
    }

    #[test]
    fn spec_3_15_test_18_a_killed_member_restarted_with_its_card_is_delivered_to_within_1_s_of_its_new_presence()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = timed("amy joins and claims its card", || {
            lab_member(&network, "t18-amy", &unique_handle("amy"), FIRST_SESSION)
        });
        let mut bob = timed("bob joins and claims its card", || {
            lab_member(&network, "t18-bob", &unique_handle("bob"), FIRST_SESSION)
        });
        timed("amy and bob list each other online", || {
            meet(&mut amy, &mut bob);
        });
        let bob_handle = bob.handle.clone();
        assert_eq!(
            timed("amy's send to bob before the kill", || send(
                &mut amy,
                &bob_handle,
                FIRST_BODY
            ))["result"]["delivered"],
            json!(true)
        );

        bob.member.kill();
        let killed_ms = wall_clock_ms();
        let mut restarted = timed("the restarted bob starts", || bob.machine.start_server());
        let claim = timed("the restarted bob claims its card", || {
            restarted.call("init", &json!({ "card": bob.card.name }))
        });
        timed(
            "amy lists the restarted bob online, seen after the kill",
            || {
                amy.member.peer_within(&bob_handle, PRESENCE_WAIT, |bob| {
                    online(bob) && bob["last_seen"].as_u64().unwrap() > killed_ms
                })
            },
        );
        let new_presence_reached = Instant::now();
        let sent = timed("amy's send to the restarted bob", || {
            send(&mut amy, &bob_handle, FIRST_BODY)
        });
        let delivered_after = new_presence_reached.elapsed();

        assert_eq!(claim["ok"], json!(true), "{claim}");
        assert_eq!(sent["result"]["delivered"], json!(true), "{sent}");
        assert!(
            delivered_after <= SPEC_DELIVERED_WITHIN_OF_NEW_PRESENCE,
            "{delivered_after:?}"
        );
    }

    #[test]
    fn spec_3_15_test_20_a_replayed_earlier_presence_changes_nothing_and_after_departure_leaves_the_member_gone()
     {
        let _live_host = live_host_to_itself();
        let network = LiveNetwork::from_environment();
        let mut amy = lab_member(&network, "t20-amy", &unique_handle("amy"), FIRST_SESSION);
        let bob_handle = unique_handle("bob");
        let bob_machine = LabMachine::joined("t20-bob", &network);
        let bob_card = bob_machine.create_card("t20-bob-card", &bob_handle);
        let bob_key = card_key(&network, &bob_machine, &bob_card);
        let earlier = presence_signed_by(
            network.keys(),
            &bob_key,
            &bob_handle,
            &EndpointAddr::from_parts(bob_card.id, []),
        );
        let earlier_through_the_host = presence_signed_by(
            network.keys(),
            &bob_key,
            &bob_handle,
            &EndpointAddr::from_parts(
                bob_card.id,
                [TransportAddr::Relay(network.host_relay().parse().unwrap())],
            ),
        );
        let mut bob = bob_machine.start_server();
        let claim = bob.call(
            "init",
            &json!({ "card": bob_card.name, "description": SECOND_SESSION }),
        );
        let before_replay = amy.member.peer_within(&bob_handle, PRESENCE_WAIT, online);
        let replayer = Forger::join_through(
            SecretKey::generate(),
            *network.keys().gate_key(),
            network.topic(),
            network.host_address(),
        );
        let marker = SecretKey::generate();
        let marker_handle = unique_handle("marker");

        replayer.broadcast(earlier);
        replayer.broadcast(presence_signed_by(
            network.keys(),
            &marker,
            &marker_handle,
            &EndpointAddr::from_parts(marker.public(), []),
        ));
        amy.member
            .peer_within(&marker_handle, PRESENCE_WAIT, |_marker| true);
        std::thread::sleep(REPLAY_SETTLE_WAIT);
        let after_replay = amy
            .member
            .peer_within(&bob_handle, PRESENCE_WAIT, |_bob| true);
        bob.close_standard_input();
        amy.member.peer_within(&bob_handle, PRESENCE_WAIT, gone);
        replayer.broadcast(earlier_through_the_host);
        std::thread::sleep(REPLAY_SETTLE_WAIT);
        let after_departure_replay = amy
            .member
            .peer_within(&bob_handle, PRESENCE_WAIT, |_bob| true);

        assert_eq!(claim["ok"], json!(true), "{claim}");
        assert_eq!(
            before_replay["public"],
            json!({ "description": SECOND_SESSION })
        );
        assert_eq!(after_replay["public"], before_replay["public"]);
        assert_eq!(after_replay["attached"], before_replay["attached"]);
        assert_eq!(after_replay["status"], json!("online"));
        assert_eq!(after_departure_replay["status"], json!("gone"));
    }
}
