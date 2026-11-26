use std::fs;
use std::io;
use std::process::Command;

use data_encoding::HEXLOWER;
use estafette::config::{Host, HostOrigin, NetworkConfig};
use estafette::crypto::NetworkKeys;
use estafette::dirs::Directories;
use estafette::invite::{parse_invitation, render_host_line};
use estafette::keys::{render_network_secret, render_public_key};
use estafette::wire::{self, HostAddr, Invitation, RelayUrl, SignatureBytes, Version};
use iroh::{EndpointAddr, EndpointId, TransportAddr};
use iroh_gossip::TopicId;

use crate::forgeries::wall_clock_seconds;
use crate::scratch_machine::recipe_run::recipe_failure;
use crate::scratch_machine::{BINARY, INVITATION_VARIABLE, KeptFilesPolicy, KeptSecrets};

/// The task runner every project task runs through.
pub const TASK_RUNNER: &str = "just";
/// The recipe that runs the binary in the lab owner's directories.
const OWNER_MACHINE_RECIPE: &str = "remote::estafette";
/// The variable that recipe takes the binary's path from.
pub const LAB_BINARY_VARIABLE: &str = "LAB_BINARY";
/// What a failure says of the invitation variable when it is set to a value that is not unicode, after the variable's name, never the value.
const INVITATION_NOT_UNICODE_FAILURE: &str =
    "is set but not valid unicode, so the live network cannot be joined";
/// What a failure to read the invitation tells its reader to do, after why.
const INVITATION_EXPORTED_BY: &str =
    "run `just gate::acceptance`, which exports it from the live lab host";

/// The network the live lab host serves, as the invitation the acceptance gate exports names it.
pub struct LiveNetwork {
    invitation: Invitation,
    keys: NetworkKeys,
    kept_files_policy: KeptFilesPolicy,
}

impl LiveNetwork {
    /// Reads the invitation and the running test's policy for the files it keeps from the variables the acceptance gate exports, failing with the invitation variable's name and the kind of failure, never its value, when it is unset or not unicode.
    pub fn from_environment() -> Self {
        let kept_files_policy = KeptFilesPolicy::from_environment();
        let text = match kept_files_policy.invitation() {
            Ok(Some(text)) => text.to_owned(),
            Ok(None) => panic!("{INVITATION_VARIABLE} is unset: {INVITATION_EXPORTED_BY}"),
            Err(unreadable) => panic!(
                "{INVITATION_VARIABLE} {INVITATION_NOT_UNICODE_FAILURE} ({}): {INVITATION_EXPORTED_BY}",
                unreadable.kind()
            ),
        };
        let invitation = parse_invitation(text.trim())
            .unwrap_or_else(|refusal| panic!("{INVITATION_VARIABLE} is refused: {refusal}"));
        let keys = NetworkKeys::derive(&invitation.secret);
        Self {
            invitation,
            keys,
            kept_files_policy,
        }
    }

    /// How the running test keeps the files of the members it starts on this network.
    pub const fn kept_files_policy(&self) -> &KeptFilesPolicy {
        &self.kept_files_policy
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
            EndpointId::from_bytes(host.id.as_bytes()).unwrap(),
            [TransportAddr::Relay(host.relay.as_str().parse().unwrap())],
        )
    }

    /// Writes the network configuration the invitation describes on the machine of these directories, as joining the network does.
    pub fn join(&self, directories: &Directories) {
        self.join_through(directories, &self.invitation.hosts);
    }

    /// Writes the network configuration of the invitation on the machine of these directories, naming the hosts given in place of the invitation's.
    pub fn join_through(&self, directories: &Directories, hosts: &[HostAddr]) {
        let config_file = directories.network_config_file(&self.name().parse().unwrap());
        fs::create_dir_all(config_file.parent().unwrap()).unwrap();
        self.configuration_through(hosts)
            .rewrite(&config_file, wall_clock_seconds())
            .unwrap();
    }

    /// The secrets a recipe run for the network with no member prints its failure searched for, read as a server member's are from the configuration the invitation describes and the invitation; or why they could not be read.
    pub fn kept_secrets(&self) -> io::Result<KeptSecrets> {
        let configuration = toml::to_string(&self.configuration_through(&self.invitation.hosts))
            .map_err(io::Error::other)?;
        let invitation = self.kept_files_policy.invitation()?;
        KeptSecrets::of_server_member(&configuration, invitation)
    }

    /// The network configuration of the invitation, naming the hosts given in place of the invitation's.
    fn configuration_through(&self, hosts: &[HostAddr]) -> NetworkConfig {
        NetworkConfig {
            secret: render_network_secret(&self.invitation.secret),
            owner: render_public_key(
                &iroh::PublicKey::from_bytes(self.invitation.owner.as_bytes()).unwrap(),
            ),
            hosts: hosts
                .iter()
                .map(|host| Host {
                    id: HEXLOWER.encode(host.id.as_bytes()),
                    relay: host.relay.as_str().to_owned(),
                    ts: None,
                    origin: HostOrigin::Configured,
                })
                .collect(),
        }
    }

    /// The owner's certificate of the host id, made by the binary's certify command on the lab owner's machine.
    pub fn certify(&self, host: &iroh::PublicKey) -> SignatureBytes {
        let host_line = render_host_line(&HostAddr {
            v: Version::CURRENT,
            id: wire::PublicKey::from(*host),
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
            recipe_failure(&certified, self.kept_secrets().ok().as_ref())
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

#[cfg(test)]
mod tests {
    use std::fs::{self, File};

    use super::{INVITATION_NOT_UNICODE_FAILURE, LiveNetwork};
    use crate::scratch_machine::owned_process::{
        FAILED_TEST_EXIT_CODE, FIXTURE_ENDED_WITHIN, FIXTURE_STANDARD_ERROR, FIXTURE_VARIABLE,
        fixture_run_within,
    };
    use crate::scratch_machine::{
        INVITATION_VARIABLE, PLANTED_UNREADABLE_INVITATION_MARKER, planted_invitation_not_unicode,
        remove_scratch, this_process_directory,
    };

    /// The fixture that reads the live network while the invitation variable is not unicode, by the name the test harness runs it by.
    const INVITATION_NOT_UNICODE_FIXTURE: &str =
        "live_network::tests::forced_failure_reading_an_invitation_variable_that_is_not_unicode";

    #[test]
    fn observability_decision_16_a_live_network_whose_invitation_variable_is_set_but_not_unicode_fails_naming_the_variable_and_prints_nothing_of_its_value()
     {
        let scratch = this_process_directory::fresh_directory_of_this_process_in(
            &std::env::temp_dir(),
            "estafette-live-invitation-not-unicode-test",
        )
        .unwrap();
        let standard_error = scratch.join(FIXTURE_STANDARD_ERROR);
        let invitation = planted_invitation_not_unicode();

        let run = fixture_run_within(
            INVITATION_NOT_UNICODE_FIXTURE,
            &[(INVITATION_VARIABLE, &invitation)],
            File::create(&standard_error).unwrap().into(),
            FIXTURE_ENDED_WITHIN,
        );

        let printed = String::from_utf8_lossy(&fs::read(&standard_error).unwrap()).into_owned();
        remove_scratch::remove_scratch(&scratch).unwrap();
        assert_eq!(run.exit.code(), Some(FAILED_TEST_EXIT_CODE), "{printed}");
        assert!(
            printed.contains(&format!(
                "{INVITATION_VARIABLE} {INVITATION_NOT_UNICODE_FAILURE}"
            )),
            "{printed}"
        );
        assert!(
            !printed.contains(PLANTED_UNREADABLE_INVITATION_MARKER),
            "standard error shows the invitation: {printed}"
        );
        assert!(
            !String::from_utf8_lossy(&run.standard_output)
                .contains(PLANTED_UNREADABLE_INVITATION_MARKER),
            "standard output shows the invitation"
        );
    }

    /// Reads the live network from the environment; run as the child process of a parent test, which sets the invitation to a value that is not unicode, and refusing to run without the fixture variable; ignored in the suite.
    #[test]
    #[ignore = "a fixture: the child process of the test of an invitation variable that is not unicode"]
    fn forced_failure_reading_an_invitation_variable_that_is_not_unicode() {
        assert_eq!(
            std::env::var(FIXTURE_VARIABLE).ok().as_deref(),
            Some(INVITATION_NOT_UNICODE_FIXTURE),
            "{INVITATION_NOT_UNICODE_FIXTURE} is a fixture its parent test runs with {FIXTURE_VARIABLE} naming it"
        );
        drop(LiveNetwork::from_environment());
    }
}
