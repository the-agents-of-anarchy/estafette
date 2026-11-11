use std::fs;
use std::process::Command;

use data_encoding::HEXLOWER;
use estafette::config::{Host, HostOrigin, NetworkConfig};
use estafette::crypto::{self, NetworkKeys};
use estafette::dirs::Directories;
use estafette::invite::{parse_invitation, render_host_line};
use estafette::keys::{NetworkSecret, render_network_secret, render_public_key};
use estafette::wire::{HostAddr, Invitation, RelayUrl, SignatureBytes, Version};
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use iroh_gossip::TopicId;

use crate::forgeries::wall_clock_seconds;
use crate::scratch_machine::{BINARY, INVITATION_VARIABLE, RunRecordKeeping};

/// The task runner every project task runs through.
pub const TASK_RUNNER: &str = "just";
/// The recipe that runs the binary in the lab owner's directories.
const OWNER_MACHINE_RECIPE: &str = "remote::estafette";
/// The variable that recipe takes the binary's path from.
pub const LAB_BINARY_VARIABLE: &str = "LAB_BINARY";

/// The network the live lab host serves, as the invitation the acceptance gate exports names it.
pub struct LiveNetwork {
    invitation: Invitation,
    keys: NetworkKeys,
    run_records: RunRecordKeeping,
}

impl LiveNetwork {
    /// Reads the invitation and the running test's run record keeping from the variables the acceptance gate exports, failing with the invitation variable's name when it is unset.
    pub fn from_environment() -> Self {
        let text = std::env::var(INVITATION_VARIABLE).unwrap_or_else(|source| {
            panic!(
                "{INVITATION_VARIABLE} is not usable ({source}): run `just gate::acceptance`, which exports it from the live lab host"
            )
        });
        let invitation = parse_invitation(text.trim())
            .unwrap_or_else(|refusal| panic!("{INVITATION_VARIABLE} is refused: {refusal}"));
        let keys = NetworkKeys::derive(&crypto::NetworkSecret::from_bytes(invitation.secret));
        Self {
            invitation,
            keys,
            run_records: RunRecordKeeping::from_environment(),
        }
    }

    /// How the running test keeps the run records of the members it starts on this network.
    pub const fn run_records(&self) -> &RunRecordKeeping {
        &self.run_records
    }

    /// The network's name.
    pub fn name(&self) -> &str {
        &self.invitation.network
    }

    /// The keys every member derives from the network secret.
    pub const fn keys(&self) -> &NetworkKeys {
        &self.keys
    }

    /// The gossip topic of the network.
    pub fn topic(&self) -> TopicId {
        TopicId::from_bytes(*self.keys.topic_id().as_bytes())
    }

    /// The relay URL of the live host the invitation names first.
    pub fn host_relay(&self) -> &str {
        self.invitation.hosts[0].relay.as_str()
    }

    /// The address of the live host's bootstrap node: its id, reached through its relay.
    pub fn host_address(&self) -> EndpointAddr {
        let host = &self.invitation.hosts[0];
        EndpointAddr::from_parts(
            EndpointId::from_bytes(&host.id).unwrap(),
            [TransportAddr::Relay(host.relay.as_str().parse().unwrap())],
        )
    }

    /// Writes the network configuration the invitation describes on the machine of these directories, as joining the network does.
    pub fn join(&self, directories: &Directories) {
        self.join_through(directories, &self.invitation.hosts);
    }

    /// Writes the network configuration of the invitation on the machine of these directories, naming the hosts given in place of the invitation's.
    pub fn join_through(&self, directories: &Directories, hosts: &[HostAddr]) {
        let config_file = directories.network_config_file(self.name()).unwrap();
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        NetworkConfig {
            secret: render_network_secret(&NetworkSecret::from_bytes(self.invitation.secret)),
            owner: render_public_key(&iroh::PublicKey::from_bytes(&self.invitation.owner).unwrap()),
            hosts: hosts
                .iter()
                .map(|host| Host {
                    id: HEXLOWER.encode(&host.id),
                    relay: host.relay.as_str().to_owned(),
                    ts: None,
                    origin: HostOrigin::Configured,
                })
                .collect(),
        }
        .rewrite(&config_file, wall_clock_seconds())
        .unwrap();
    }

    /// The owner's certificate of the host id, made by the binary's certify command on the lab owner's machine.
    pub fn certify(&self, host: &iroh::PublicKey) -> SignatureBytes {
        let host_line = render_host_line(&HostAddr {
            v: Version::CURRENT,
            id: *host.as_bytes(),
            relay: RelayUrl::try_from(self.host_relay().to_owned()).unwrap(),
        })
        .unwrap();
        let certified = Command::new(TASK_RUNNER)
            .args([
                OWNER_MACHINE_RECIPE,
                "network",
                "certify",
                self.name(),
                &host_line,
            ])
            .env(LAB_BINARY_VARIABLE, BINARY)
            .output()
            .unwrap();
        assert!(
            certified.status.success(),
            "certify on the lab owner's machine failed: {}",
            String::from_utf8_lossy(&certified.stderr)
        );
        let certificate = HEXLOWER
            .decode(
                String::from_utf8(certified.stdout)
                    .unwrap()
                    .trim_end()
                    .as_bytes(),
            )
            .unwrap();
        SignatureBytes(certificate.try_into().unwrap())
    }
}
