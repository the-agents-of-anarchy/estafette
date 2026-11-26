use std::io::{self, BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use estafette::invite::parse_host_line;
use estafette::wire::HostAddr;

use crate::lab_server::LabServer;
use crate::live_network::LiveNetwork;
use crate::payload_placeholders::wall_clock_ms;
use crate::scratch_machine::KeptSecrets;
use crate::scratch_machine::recipe_run::{fail_unless_succeeded, recipe_failure};

/// The variable the acceptance gate names the lab server in whose host the test may kill.
const SERVER_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_KILLABLE_SERVER";
/// The variable the acceptance gate exports that host's address line in.
const HOST_LINE_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_KILLABLE_HOST_LINE";
/// The recipe that runs a command on a lab server as root.
const REMOTE_SHELL_RECIPE: &str = "remote::ssh";
/// The user the kill's command runs as on the server.
const REMOTE_SHELL_USER: &str = "root";
/// The recipe that starts the network's host on a server that already exists, refusing to create one.
const HOST_STARTED_RECIPE: &str = "remote::host-start";
/// The recipe that stops the network's host on a server when it runs.
const HOST_STOPPED_RECIPE: &str = "remote::host-down";
/// The line the host prints once the SSH session is open and the kill waits only for its trigger.
const ARMED_LINE: &str = "armed";
/// The systemd unit the infrastructure recipe runs a host's relay and bootstrap node in.
const HOST_UNIT: &str = "estafette-host";
/// How long the kill's SSH session may take to open and stand ready, the server's address lookup included.
const ARMED_WAIT: Duration = Duration::from_secs(60);

/// A second lab host, relay and bootstrap node in one process on a lab server of its own, which the acceptance gate lets the test stop, start and kill, each recipe's failure printed with its standard error searched for the live network's secrets or left out when they could not be read.
pub struct KillableHost {
    server: LabServer,
    address: HostAddr,
    secrets: io::Result<KeptSecrets>,
}

/// The instant the kill was sent, on this machine's wall clock in milliseconds since the Unix epoch, as a member's trace stamps `wall_ms`, which no monotonic reading converts into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KillInstant {
    wall_ms: u64,
}

impl KillInstant {
    /// The kill's instant as a wall-clock reading in milliseconds since the Unix epoch.
    pub const fn at_wall_ms(wall_ms: u64) -> Self {
        Self { wall_ms }
    }

    /// The kill's instant in milliseconds since the Unix epoch on the wall clock.
    pub const fn wall_ms(self) -> u64 {
        self.wall_ms
    }
}

/// When the kill was sent over the open session, on this machine's wall clock, and on its monotonic clock, and when its command returned.
pub struct KillStamps {
    pub instant: KillInstant,
    pub sent: Instant,
    pub returned: Instant,
}

/// The killable host started again when dropped, whether the test that stopped or killed it ended or failed, so every later test finds it serving.
pub struct HostStartedWhenDropped<'killable> {
    killable: &'killable KillableHost,
}

impl KillableHost {
    /// Reads the server and the host address line the acceptance gate exports, refusing to run when either is unset, with the live network's secrets.
    pub fn from_environment(network: &LiveNetwork) -> Self {
        let exported = |variable: &str| {
            std::env::var(variable).unwrap_or_else(|source| {
                panic!(
                    "{variable} is not usable ({source}): run `just gate::acceptance`, which exports the host the test may kill"
                )
            })
        };
        let address = parse_host_line(exported(HOST_LINE_VARIABLE).trim())
            .unwrap_or_else(|refusal| panic!("{HOST_LINE_VARIABLE} is refused: {refusal}"));
        Self {
            server: LabServer::named(exported(SERVER_VARIABLE)),
            address,
            secrets: network.kept_secrets(),
        }
    }

    /// The live network's secrets a recipe's failure is printed searched for, none when they could not be read.
    pub fn secrets(&self) -> Option<&KeptSecrets> {
        self.secrets.as_ref().ok()
    }

    /// The host's id and relay, as its address line names them.
    pub const fn address(&self) -> &HostAddr {
        &self.address
    }

    /// The host's relay URL.
    pub fn relay(&self) -> &str {
        self.address.relay.as_str()
    }

    /// The server the host runs on, for members and paths on it.
    pub const fn server(&self) -> &LabServer {
        &self.server
    }

    /// Starts the host on its server unless it runs, with the identity and certificate it had, failing with the server's name when the server does not exist.
    #[track_caller]
    pub fn start(&self) {
        self.server
            .run(HOST_STARTED_RECIPE, &[], &[], self.secrets());
    }

    /// Stops the host on its server when it runs, relay and bootstrap node together.
    #[track_caller]
    pub fn stop(&self) {
        self.server
            .run(HOST_STOPPED_RECIPE, &[], &[], self.secrets());
    }

    /// Starts the host again once the returned guard is dropped, so a test may stop or kill it and leave it serving.
    pub const fn started_when_dropped(&self) -> HostStartedWhenDropped<'_> {
        HostStartedWhenDropped { killable: self }
    }

    /// Opens the kill's SSH session and waits for the host to stand ready, then stamps the instant on the wall clock and on the monotonic clock, sends the kill over the open session and stamps its return, so only the warmed round trip separates the stamps from the kill.
    #[track_caller]
    pub fn kill(&self) -> KillStamps {
        let mut session = self
            .server
            .recipe_command(REMOTE_SHELL_RECIPE, &[REMOTE_SHELL_USER, &armed_kill()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = session.stdout.take().unwrap();
        let (forward, announced) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let read = BufReader::new(output).read_line(&mut line).map(|_| line);
            drop(forward.send(read));
        });
        let announcement = announced.recv_timeout(ARMED_WAIT);
        let stood_ready = matches!(&announcement, Ok(Ok(line)) if line.trim_end() == ARMED_LINE);
        if !stood_ready {
            drop(session.kill());
            drop(session.wait());
            panic!(
                "the kill session on {} did not print {ARMED_LINE:?} within {ARMED_WAIT:?}: {announcement:?}",
                self.server.label()
            );
        }
        let mut trigger = session.stdin.take().unwrap();
        let instant = KillInstant::at_wall_ms(wall_clock_ms());
        let sent = Instant::now();
        trigger.write_all(b"\n").unwrap();
        drop(trigger);
        let killed = session.wait_with_output().unwrap();
        let returned = Instant::now();
        assert!(
            killed.status.success(),
            "the kill on {} failed: {}",
            self.server.label(),
            recipe_failure(&killed, self.secrets())
        );
        KillStamps {
            instant,
            sent,
            returned,
        }
    }
}

impl Drop for HostStartedWhenDropped<'_> {
    fn drop(&mut self) {
        let server = &self.killable.server;
        let started = server.attempt_without_panicking(HOST_STARTED_RECIPE, &[], &[]);
        fail_unless_succeeded(
            &started,
            &format!("the host on {} was not started again", server.label()),
            self.killable.secrets(),
        );
    }
}

/// The remote command, quoted as one word for the recipe's shell: announce the open session, wait for the trigger line, then SIGKILL the host's main process, relay and bootstrap node together.
fn armed_kill() -> String {
    format!(
        "'echo {ARMED_LINE} && read -r _ && systemctl kill --kill-whom=main --signal=SIGKILL {HOST_UNIT}'"
    )
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt;
    use std::panic::{self, AssertUnwindSafe};
    use std::process::{ExitStatus, Output};

    use estafette::wire::{RelayUrl, Version};

    use super::*;
    use crate::lab_server::tests::{
        FIRST_FAILURE, require_ended_in_the_first_failure, server_running,
        server_whose_recipes_cannot_run,
    };
    use crate::scratch_machine::REDACTED;

    /// The id a test's killable host names, no real one.
    const PLANTED_HOST_ID: [u8; 32] = [0x0f; 32];
    /// The relay a test's killable host names, no real one.
    const PLANTED_RELAY: &str = "https://relay.lab-server.invalid";
    /// A network secret a test plants in the live network's configuration and in what a recipe prints, no real one.
    const PLANTED_TOKEN: &str = "plantedtoken0123456789abcdef";
    /// An invitation a test plants as the live network's, no real one.
    const PLANTED_INVITATION: &str = "estafette-invite-1:plantedinvitation0123456789";
    /// What a test's failing recipe prints on its standard error, the planted network secret in it.
    const REFUSAL_NAMING_THE_TOKEN: &str = "ssh: refused plantedtoken0123456789abcdef";
    /// The raw wait status of a recipe that exited with code 1.
    const EXITED_WITH_ONE: i32 = 1 << 8;

    /// The live network's configuration in a test, naming the planted network secret.
    fn configuration_naming_the_token() -> String {
        format!("secret = \"{PLANTED_TOKEN}\"\n")
    }

    /// A killable host on the server with the address a test plants and the secrets given.
    fn host_on(server: LabServer, secrets: io::Result<KeptSecrets>) -> KillableHost {
        KillableHost {
            server,
            address: HostAddr {
                v: Version::CURRENT,
                id: PLANTED_HOST_ID,
                relay: RelayUrl::try_from(PLANTED_RELAY.to_owned()).unwrap(),
            },
            secrets,
        }
    }

    /// The failure a test that was not failing ends in when its host is started again by a drop whose recipe fails printing the planted secret.
    fn failure_of_starting_again(secrets: io::Result<KeptSecrets>) -> String {
        let killable = host_on(
            server_running(|_server, _recipe, _arguments, _input| {
                Ok(Output {
                    status: ExitStatus::from_raw(EXITED_WITH_ONE),
                    stdout: Vec::new(),
                    stderr: REFUSAL_NAMING_THE_TOKEN.as_bytes().to_vec(),
                })
            }),
            secrets,
        );
        let ended = panic::catch_unwind(AssertUnwindSafe(|| {
            let _host_back = killable.started_when_dropped();
        }));
        let payload = ended.expect_err("a failed start did not fail the test");
        payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default()
    }

    #[test]
    fn observability_decision_16_a_host_started_again_when_dropped_on_a_failing_test_whose_start_cannot_run_leaves_that_failure_standing()
     {
        let killable = host_on(
            server_whose_recipes_cannot_run(),
            KeptSecrets::of_server_member(
                &configuration_naming_the_token(),
                Some(PLANTED_INVITATION),
            ),
        );

        let ended = panic::catch_unwind(AssertUnwindSafe(|| {
            let _host_back = killable.started_when_dropped();
            panic!("{FIRST_FAILURE}");
        }));

        require_ended_in_the_first_failure(ended);
    }

    #[test]
    fn observability_decision_16_a_host_not_started_again_fails_a_test_that_was_not_failing_with_the_recipes_standard_error_searched_for_the_live_networks_secrets()
     {
        let failure = failure_of_starting_again(KeptSecrets::of_server_member(
            &configuration_naming_the_token(),
            Some(PLANTED_INVITATION),
        ));

        assert!(
            failure.contains(&format!("ssh: refused {REDACTED}"))
                && !failure.contains(PLANTED_TOKEN),
            "{failure}"
        );
    }

    #[test]
    fn observability_decision_16_a_host_not_started_again_while_the_live_networks_secrets_cannot_be_read_fails_the_test_leaving_out_the_recipes_standard_error()
     {
        let failure = failure_of_starting_again(KeptSecrets::of_server_member(
            &configuration_naming_the_token(),
            None,
        ));

        assert!(
            failure.contains(&ExitStatus::from_raw(EXITED_WITH_ONE).to_string())
                && !failure.contains(REFUSAL_NAMING_THE_TOKEN)
                && !failure.contains(PLANTED_TOKEN),
            "{failure}"
        );
    }
}
