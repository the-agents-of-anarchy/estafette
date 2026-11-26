//! The known hosts of spec 3.8: the hosts the configuration names and the Infrastructure records that add or move them, kept in the network file.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};

use super::addresses::{lookup_replaced_event, replace_lookup_entry};
use super::presence::{NodeError, admission_record};
use crate::config::{ConfigError, Host, HostOrigin, MILLIS_PER_SECOND, NetworkConfig, UnixMillis};
use crate::event::{ConfigurationRewrite, Event, GossipEvent, ShortId};
use crate::keys::parse_public_key;
use crate::net::endpoint::NodeEndpoint;
use crate::payload::{DescribedPayload, PayloadDropRule, ValidInfrastructure};
use crate::wire;

/// How far ahead of this node's clock a host record's ts may be and still be held (spec:779, spec:1013).
const INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS: u64 = 300;

/// A known host's node id and relay, as the configuration names them.
pub struct KnownHostAddress {
    pub id: EndpointId,
    pub relay: RelayUrl,
}

/// The node id and relay of every host of the configuration, refusing a host whose id or relay does not parse.
pub fn known_host_addresses(config: &NetworkConfig) -> Result<Vec<KnownHostAddress>, NodeError> {
    config
        .hosts
        .iter()
        .map(|host| {
            known_host_address(host).map_err(|_unparsed| NodeError::KnownHostInvalid {
                id: host.id.clone(),
            })
        })
        .collect()
}

/// The part of a configured host that did not parse: its node id, or its relay when the id did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostUnparsed {
    Id,
    Relay,
}

/// A configured host a rebind leaves out because a part of it does not parse: its place in the configuration and that part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedHost {
    pub index: usize,
    pub reason: HostUnparsed,
}

/// The host's node id and relay, or the part that does not parse.
fn known_host_address(host: &Host) -> Result<KnownHostAddress, HostUnparsed> {
    Ok(KnownHostAddress {
        id: parse_public_key(&host.id).map_err(|_unparsed| HostUnparsed::Id)?,
        relay: host
            .relay
            .parse()
            .map_err(|_unparsed| HostUnparsed::Relay)?,
    })
}

/// Gives each known host whose lookup entry holds no address its relay as its only address (spec 3.8), returning the cards it seeded.
pub fn seed_lookup_with_known_hosts(
    endpoint: &NodeEndpoint,
    hosts: &[KnownHostAddress],
) -> Vec<wire::PublicKey> {
    let mut seeded = Vec::new();
    for host in hosts {
        if endpoint.address_of(host.id).is_none() {
            endpoint.replace_address(EndpointAddr::from_parts(
                host.id,
                [TransportAddr::Relay(host.relay.clone())],
            ));
            seeded.push(*host.id.as_bytes());
        }
    }
    seeded
}

/// What a valid Infrastructure record the node holds does to the known hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordVerdict {
    FromKnownHost,
    FromNewHost,
}

/// What a node does with the relay of a host record it takes: a member adds it to its relay map, a bootstrap host keeps its own relay alone (spec 3.14).
#[derive(Debug, Clone, Copy)]
pub enum RelayOfRecord {
    AddToRelayMap,
    LeaveOutOfRelayMap,
}

/// The known hosts of spec 3.8, with the newest record `ts` held for each and the file that keeps them, shared by the presence loop and the stopping session.
pub struct KnownHosts {
    held: Mutex<HeldHosts>,
}

struct HeldHosts {
    config: NetworkConfig,
    path: PathBuf,
    newer_than_file: bool,
}

impl KnownHosts {
    /// The known hosts as the file holds them.
    pub(super) const fn new(config: NetworkConfig, path: PathBuf) -> Self {
        Self {
            held: Mutex::new(HeldHosts {
                config,
                path,
                newer_than_file: false,
            }),
        }
    }

    /// The hosts held, locked for one operation.
    fn held(&self) -> MutexGuard<'_, HeldHosts> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether no host is known.
    pub(super) fn is_empty(&self) -> bool {
        self.held().config.hosts.is_empty()
    }

    /// The node id and relay of every known host whose id and relay parse.
    pub(super) fn addresses(&self) -> Vec<KnownHostAddress> {
        self.held()
            .config
            .hosts
            .iter()
            .filter_map(|host| known_host_address(host).ok())
            .collect()
    }

    /// Every configured host a rebind leaves out because its id or relay does not parse.
    pub(super) fn skipped(&self) -> Vec<SkippedHost> {
        self.held()
            .config
            .hosts
            .iter()
            .enumerate()
            .filter_map(|(index, host)| {
                known_host_address(host)
                    .err()
                    .map(|reason| SkippedHost { index, reason })
            })
            .collect()
    }

    /// The known host whose relay is `relay`, if any parses to it.
    pub(super) fn host_with_relay(&self, relay: &RelayUrl) -> Option<ShortId> {
        self.addresses()
            .into_iter()
            .find(|host| host.relay == *relay)
            .map(|host| ShortId::from(host.id))
    }

    /// Applies the Infrastructure rows of spec 3.8, recording whether the record was taken: holds a record newer than its host's last, points the lookup at its address, adds its relay as `relay` says, and saves a host not known before, recording a save that failed.
    pub(crate) async fn take_infrastructure(
        &self,
        valid: &ValidInfrastructure,
        now_ms: u64,
        endpoint: &NodeEndpoint,
        relay: RelayOfRecord,
        report: &(dyn Fn(Event) + Send + Sync),
    ) {
        let admitted = self.held().take_record(valid, now_ms);
        report(admission_record(valid.record().facts(), None, &admitted));
        let Ok(verdict) = admitted else {
            return;
        };
        replace_lookup_entry(endpoint, &valid.record().host, &valid.record().addr);
        if let Some(event) = lookup_replaced_event(endpoint, &valid.record().host, None) {
            report(event);
        }
        match relay {
            RelayOfRecord::AddToRelayMap => {
                endpoint
                    .add_relay(valid.relay().clone(), ShortId::from(&valid.record().host))
                    .await;
            }
            RelayOfRecord::LeaveOutOfRelayMap => {}
        }
        if verdict == RecordVerdict::FromNewHost
            && let Err(failure) = self.save(now_ms)
        {
            report(GossipEvent::KnownHostsNotSaved(failure).into());
        }
    }

    /// Rewrites the network configuration with the known hosts (spec 3.3).
    fn save(&self, now_ms: u64) -> Result<(), ConfigError> {
        self.held().save(now_ms)
    }

    /// Rewrites the network configuration only when a held record is newer than the file's (spec 3.3), returning `Saved` when it did and `NotNeeded` when it did not.
    pub(crate) fn save_if_newer_than_file(
        &self,
        now_ms: u64,
    ) -> Result<ConfigurationRewrite, ConfigError> {
        let mut held = self.held();
        if held.newer_than_file {
            held.save(now_ms).map(|()| ConfigurationRewrite::Saved)
        } else {
            Ok(ConfigurationRewrite::NotNeeded)
        }
    }
}

impl HeldHosts {
    /// Holds the relay and `ts` of a record newer than the newest held for its host and within the future bound, adding a host never known before, or names the rule that drops the record.
    fn take_record(
        &mut self,
        valid: &ValidInfrastructure,
        now_ms: u64,
    ) -> Result<RecordVerdict, PayloadDropRule> {
        let record = valid.record();
        let latest_accepted_ts =
            seconds_of(now_ms).saturating_add(INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS);
        if record.ts > latest_accepted_ts {
            return Err(PayloadDropRule::RecordPastFutureBound);
        }
        let id = data_encoding::HEXLOWER.encode(&record.host);
        let Some(host) = self.config.hosts.iter_mut().find(|host| host.id == id) else {
            self.config.hosts.push(Host {
                id,
                relay: record.relay.as_str().to_owned(),
                ts: Some(record.ts),
                origin: HostOrigin::Learned {
                    heard: UnixMillis(now_ms),
                },
            });
            self.newer_than_file = true;
            return Ok(RecordVerdict::FromNewHost);
        };
        if host.ts.is_some_and(|held| record.ts <= held) {
            return Err(PayloadDropRule::RecordNotNewerThanHeld);
        }
        host.ts = Some(record.ts);
        record.relay.as_str().clone_into(&mut host.relay);
        if let HostOrigin::Learned { heard } = &mut host.origin {
            *heard = UnixMillis(now_ms);
        }
        self.newer_than_file = true;
        Ok(RecordVerdict::FromKnownHost)
    }

    /// Rewrites the network configuration with the known hosts (spec 3.3).
    fn save(&mut self, now_ms: u64) -> Result<(), ConfigError> {
        self.config.rewrite(&self.path, seconds_of(now_ms))?;
        self.newer_than_file = false;
        Ok(())
    }
}

/// Whole seconds in a span of milliseconds.
const fn seconds_of(milliseconds: u64) -> u64 {
    milliseconds / MILLIS_PER_SECOND
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::presence::tests::{
        FIRST_SEED, FROZEN_CLOCK_MS, HOST_RELAY, HOST_SEED, NETWORK_FILE_NAME, OWNER_SEED,
        PROPERTY_CASES, bind, card, config_with_hosts, infrastructure_of, keys, relay_url,
        secret_key,
    };
    use crate::payload::OpenedPayload;
    use crate::seeded_draws::Draws;

    const UNHELD_RELAY: &str = "https://relay-unheld.estafette.invalid/";
    const NOT_A_NODE_ID: &str = "not a node id";
    const NOT_A_RELAY_URL: &str = "not a relay url";
    const SPEC_REFUSAL_ACCEPTED: &str =
        "64 lowercase hexadecimal characters and a relay of an https URL of at most 128 bytes";

    const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
    const SPEC_INFRASTRUCTURE_RECORD_EXPIRY_DAYS: u64 = 30;
    const SPEC_INFRASTRUCTURE_RECORD_EXPIRY_SECONDS: u64 =
        SPEC_INFRASTRUCTURE_RECORD_EXPIRY_DAYS * SECONDS_PER_DAY;
    const WHOLE_DAYS_DRAWN: u64 = 2 * SPEC_INFRASTRUCTURE_RECORD_EXPIRY_DAYS + 1;
    const SECONDS_DRAWN_EACH_SIDE_OF_A_WHOLE_DAY: u64 = 1;
    const SECOND_SHIFTS_DRAWN: u64 = 2 * SECONDS_DRAWN_EACH_SIDE_OF_A_WHOLE_DAY + 1;

    /// Whole seconds since a host was heard: whole days up to twice the expiry, shifted by up to a second either way so the expiry's own edge is drawn.
    fn drawn_seconds_since_heard(draws: &mut Draws) -> u64 {
        draws
            .below(WHOLE_DAYS_DRAWN)
            .checked_mul(SECONDS_PER_DAY)
            .and_then(|whole_days| whole_days.checked_add(draws.below(SECOND_SHIFTS_DRAWN)))
            .unwrap()
            .saturating_sub(SECONDS_DRAWN_EACH_SIDE_OF_A_WHOLE_DAY)
    }

    fn opened_record(
        host_seed: u8,
        relay: &str,
        ts: u64,
    ) -> Result<OpenedPayload, crate::payload::PayloadRefusal> {
        crate::payload::open_described_payload(
            &keys(),
            &secret_key(OWNER_SEED).public(),
            &sealed_record(host_seed, relay, ts),
        )
    }

    fn sealed_record(host_seed: u8, relay: &str, ts: u64) -> Vec<u8> {
        crate::payload::seal_payload(
            &keys(),
            &secret_key(host_seed),
            infrastructure_of(host_seed, relay, ts),
        )
        .unwrap()
    }

    fn valid_record(opened: OpenedPayload) -> ValidInfrastructure {
        match opened {
            OpenedPayload::Infrastructure(valid) => valid,
            OpenedPayload::Presence(_)
            | OpenedPayload::Departure(_)
            | OpenedPayload::Unreachable(_)
            | OpenedPayload::PresenceRequest(_) => panic!("an infrastructure record was sealed"),
        }
    }

    /// Spec 3.8: the known host whose relay a relay map edit concerns is named by its five-byte id, and a relay no known host has names none.
    #[test]
    fn spec_3_8_the_known_host_with_a_relay_is_found_by_that_relay_alone() {
        let known_hosts = KnownHosts::new(
            config_with_hosts(vec![Host {
                id: crate::keys::render_public_key(&secret_key(HOST_SEED).public()),
                relay: HOST_RELAY.to_owned(),
                ts: None,
                origin: HostOrigin::Configured,
            }]),
            PathBuf::new(),
        );

        assert_eq!(
            known_hosts.host_with_relay(&relay_url(HOST_RELAY)),
            Some(ShortId::from(&card(HOST_SEED)))
        );
        assert_eq!(known_hosts.host_with_relay(&relay_url(UNHELD_RELAY)), None);
    }

    /// Spec 3.5 and 3.8: a configuration with a host whose id or relay does not parse refuses the start, naming the host by its configured id.
    #[test]
    fn spec_3_8_a_configured_host_that_does_not_parse_refuses_the_start_with_one_text_for_either_part()
     {
        let live_id = crate::keys::render_public_key(&secret_key(HOST_SEED).public());
        let unparsed_id = Host {
            id: NOT_A_NODE_ID.to_owned(),
            relay: HOST_RELAY.to_owned(),
            ts: None,
            origin: HostOrigin::Configured,
        };
        let unparsed_relay = Host {
            id: live_id.clone(),
            relay: NOT_A_RELAY_URL.to_owned(),
            ts: None,
            origin: HostOrigin::Configured,
        };

        let refusals: Vec<String> = [unparsed_id, unparsed_relay]
            .into_iter()
            .map(|host| {
                known_host_addresses(&config_with_hosts(vec![host]))
                    .err()
                    .unwrap()
                    .to_string()
            })
            .collect();

        assert_eq!(
            refusals,
            [
                format!("known host {NOT_A_NODE_ID} needs a node id of {SPEC_REFUSAL_ACCEPTED}"),
                format!("known host {live_id} needs a node id of {SPEC_REFUSAL_ACCEPTED}"),
            ]
        );
    }

    /// Spec 3.3 and 3.8: a host learned from its record leaves the file at a later save exactly when more than 30 days have passed since it was heard.
    #[tokio::test]
    async fn spec_3_3_a_save_keeps_a_learned_host_exactly_while_it_was_heard_within_30_days() {
        let root = crate::fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("known-hosts-expiry-{}", std::process::id()),
        )
        .unwrap();
        let endpoint = bind(FIRST_SEED).await;
        let record = valid_record(
            opened_record(HOST_SEED, HOST_RELAY, FROZEN_CLOCK_MS / MILLIS_PER_SECOND).unwrap(),
        );

        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let seconds_since_heard = drawn_seconds_since_heard(&mut draws);
            let path = root.join(format!("{case}-{NETWORK_FILE_NAME}"));
            let known_hosts = KnownHosts::new(config_with_hosts(Vec::new()), path.clone());
            known_hosts
                .take_infrastructure(
                    &record,
                    FROZEN_CLOCK_MS,
                    &endpoint,
                    RelayOfRecord::LeaveOutOfRelayMap,
                    &|_event| {},
                )
                .await;

            known_hosts
                .save(
                    seconds_since_heard
                        .checked_mul(MILLIS_PER_SECOND)
                        .and_then(|span| span.checked_add(FROZEN_CLOCK_MS))
                        .unwrap(),
                )
                .unwrap();

            let saved: Vec<String> = NetworkConfig::load(&path)
                .unwrap()
                .hosts
                .into_iter()
                .map(|host| host.relay)
                .collect();
            let expected: &[&str] =
                if seconds_since_heard <= SPEC_INFRASTRUCTURE_RECORD_EXPIRY_SECONDS {
                    &[HOST_RELAY]
                } else {
                    &[]
                };
            assert_eq!(
                saved, expected,
                "case {case}: saved {seconds_since_heard} s after the host was heard"
            );
        }
        crate::remove_scratch::remove_scratch(&root).unwrap();
    }

    /// Spec 3.14: a member adds the relay of a host record from a new host to its relay map, and a bootstrap host leaves it out.
    #[tokio::test]
    async fn spec_3_14_a_host_record_adds_its_relay_to_a_members_relay_map_and_not_to_a_bootstrap_hosts()
     {
        let record = valid_record(
            opened_record(HOST_SEED, HOST_RELAY, FROZEN_CLOCK_MS / MILLIS_PER_SECOND).unwrap(),
        );
        let cases = [
            (RelayOfRecord::AddToRelayMap, true),
            (RelayOfRecord::LeaveOutOfRelayMap, false),
        ];

        for (relay_of_record, relay_map_holds_the_host_relay) in cases {
            let endpoint = bind(FIRST_SEED).await;
            let known_hosts = KnownHosts::new(config_with_hosts(Vec::new()), PathBuf::new());

            known_hosts
                .take_infrastructure(
                    &record,
                    FROZEN_CLOCK_MS,
                    &endpoint,
                    relay_of_record,
                    &|_event| {},
                )
                .await;

            assert_eq!(
                endpoint.relay_map().contains(&relay_url(HOST_RELAY)),
                relay_map_holds_the_host_relay,
                "{relay_of_record:?}"
            );
        }
    }
}
