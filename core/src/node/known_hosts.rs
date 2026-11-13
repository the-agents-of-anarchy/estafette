//! The known hosts of spec 3.8: the hosts the configuration names and the Infrastructure records that add or move them, kept in the network file.

use std::path::PathBuf;

use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};

use super::addresses::replace_lookup_entry;
use super::presence::NodeError;
use crate::config::{ConfigError, Host, HostOrigin, NetworkConfig, UnixMillis};
use crate::event::{Event, GossipEvent};
use crate::keys::parse_public_key;
use crate::net::endpoint::NodeEndpoint;
use crate::payload::ValidInfrastructure;

/// How far ahead of this node's clock a host record's ts may be and still be held (spec:779, spec:1013).
const INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS: u64 = 300;
const MILLIS_PER_SECOND: u64 = 1000;

/// A known host's node id and relay, as the configuration names them.
pub struct KnownHostAddress {
    pub id: EndpointId,
    pub relay: RelayUrl,
}

/// The node id and relay of every host of the configuration, refusing a host whose id or relay does not parse.
pub fn known_host_addresses(config: &NetworkConfig) -> Result<Vec<KnownHostAddress>, NodeError> {
    config.hosts.iter().map(known_host_address).collect()
}

/// The host's node id and relay, refusing a host whose id or relay does not parse.
fn known_host_address(host: &Host) -> Result<KnownHostAddress, NodeError> {
    let invalid = || NodeError::KnownHostInvalid {
        id: host.id.clone(),
    };
    Ok(KnownHostAddress {
        id: parse_public_key(&host.id).map_err(|_unparsed| invalid())?,
        relay: host.relay.parse().map_err(|_unparsed| invalid())?,
    })
}

/// Gives each known host whose lookup entry holds no address its relay as its only address (spec 3.8).
pub fn seed_lookup_with_known_hosts(endpoint: &NodeEndpoint, hosts: &[KnownHostAddress]) {
    for host in hosts {
        if endpoint.address_of(host.id).is_none() {
            endpoint.replace_address(EndpointAddr::from_parts(
                host.id,
                [TransportAddr::Relay(host.relay.clone())],
            ));
        }
    }
}

/// What a valid Infrastructure record does to the known hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordVerdict {
    Dropped,
    FromKnownHost,
    FromNewHost,
}

/// What a node does with the relay of a host record it takes: a member adds it to its relay map, a bootstrap host keeps its own relay alone (spec 3.14).
#[derive(Debug, Clone, Copy)]
pub enum RelayOfRecord {
    AddToRelayMap,
    LeaveOutOfRelayMap,
}

/// The known hosts of spec 3.8, with the newest record `ts` held for each and the file that keeps them.
pub struct KnownHosts {
    config: NetworkConfig,
    path: PathBuf,
    newer_than_file: bool,
}

impl KnownHosts {
    /// The known hosts as the file holds them.
    pub(super) const fn new(config: NetworkConfig, path: PathBuf) -> Self {
        Self {
            config,
            path,
            newer_than_file: false,
        }
    }

    /// Whether no host is known.
    pub(super) const fn is_empty(&self) -> bool {
        self.config.hosts.is_empty()
    }

    /// The node id and relay of every known host whose id and relay parse.
    pub(super) fn addresses(&self) -> Vec<KnownHostAddress> {
        self.config
            .hosts
            .iter()
            .filter_map(|host| known_host_address(host).ok())
            .collect()
    }

    /// Applies the Infrastructure rows of spec 3.8: holds a record newer than its host's last, points the lookup at its address, adds its relay as `relay` says, and saves a host not known before, recording a save that failed.
    pub(crate) async fn take_infrastructure(
        &mut self,
        valid: &ValidInfrastructure,
        now_ms: u64,
        endpoint: &NodeEndpoint,
        relay: RelayOfRecord,
        report: &(dyn Fn(Event) + Send + Sync),
    ) {
        let verdict = self.take_record(valid, now_ms);
        if verdict == RecordVerdict::Dropped {
            return;
        }
        replace_lookup_entry(endpoint, &valid.record().host, &valid.record().addr);
        match relay {
            RelayOfRecord::AddToRelayMap => endpoint.add_relay(valid.relay().clone()).await,
            RelayOfRecord::LeaveOutOfRelayMap => {}
        }
        if verdict == RecordVerdict::FromNewHost
            && let Err(failure) = self.save(now_ms)
        {
            report(GossipEvent::KnownHostsNotSaved(failure).into());
        }
    }

    /// Holds the relay and `ts` of a record newer than the newest held for its host and within the future bound, adding a host never known before.
    fn take_record(&mut self, valid: &ValidInfrastructure, now_ms: u64) -> RecordVerdict {
        let record = valid.record();
        let latest_accepted_ts =
            seconds_of(now_ms).saturating_add(INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS);
        if record.ts > latest_accepted_ts {
            return RecordVerdict::Dropped;
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
            return RecordVerdict::FromNewHost;
        };
        if host.ts.is_some_and(|held| record.ts <= held) {
            return RecordVerdict::Dropped;
        }
        host.ts = Some(record.ts);
        record.relay.as_str().clone_into(&mut host.relay);
        if let HostOrigin::Learned { heard } = &mut host.origin {
            *heard = UnixMillis(now_ms);
        }
        self.newer_than_file = true;
        RecordVerdict::FromKnownHost
    }

    /// Rewrites the network configuration with the known hosts (spec 3.3).
    pub(crate) fn save(&mut self, now_ms: u64) -> Result<(), ConfigError> {
        self.config.rewrite(&self.path, seconds_of(now_ms))?;
        self.newer_than_file = false;
        Ok(())
    }

    /// Rewrites the network configuration only when a held record is newer than the file's (spec 3.3).
    pub(crate) fn save_if_newer_than_file(&mut self, now_ms: u64) -> Result<(), ConfigError> {
        if self.newer_than_file {
            self.save(now_ms)
        } else {
            Ok(())
        }
    }
}

/// Whole seconds in a span of milliseconds.
const fn seconds_of(milliseconds: u64) -> u64 {
    milliseconds / MILLIS_PER_SECOND
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::node::presence::tests::{
        FIRST_SEED, FROZEN_CLOCK_MS, HOST_RELAY, HOST_SEED, NETWORK_FILE_NAME, OWNER_SEED,
        PROPERTY_CASES, bind, config_with_hosts, infrastructure_of, keys, secret_key,
    };
    use crate::payload::OpenedPayload;
    use crate::seeded_draws::Draws;

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
    ) -> Result<OpenedPayload, crate::payload::PayloadError> {
        crate::payload::open_payload(
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

    /// Spec 3.3 and 3.8: a host learned from its record leaves the file at a later save exactly when more than 30 days have passed since it was heard.
    #[tokio::test]
    async fn spec_3_3_a_save_keeps_a_learned_host_exactly_while_it_was_heard_within_30_days() {
        let root = std::env::temp_dir().join(format!("known-hosts-expiry-{}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        let endpoint = bind(FIRST_SEED).await;
        let record = valid_record(
            opened_record(HOST_SEED, HOST_RELAY, FROZEN_CLOCK_MS / MILLIS_PER_SECOND).unwrap(),
        );

        for case in 0..PROPERTY_CASES {
            let mut draws = Draws::seeded(case);
            let seconds_since_heard = drawn_seconds_since_heard(&mut draws);
            let path = root.join(format!("{case}-{NETWORK_FILE_NAME}"));
            let mut known_hosts = KnownHosts::new(config_with_hosts(Vec::new()), path.clone());
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
        drop(fs::remove_dir_all(&root));
    }
}
