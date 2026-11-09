use std::process::Command;
use std::time::Instant;

use estafette::invite::parse_host_line;
use estafette::wire::HostAddr;

use crate::live_network::TASK_RUNNER;

/// The variable the acceptance gate names the lab server in whose host the test may kill.
const SERVER_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_KILLABLE_SERVER";
/// The variable the acceptance gate exports that host's address line in.
const HOST_LINE_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_KILLABLE_HOST_LINE";
/// The recipe that runs a command on a lab server as root.
const REMOTE_SHELL_RECIPE: &str = "remote::ssh";
/// The variable that recipe takes the server's name from.
const LAB_SERVER_VARIABLE: &str = "LAB_SERVER";
/// The systemd unit the infrastructure recipe runs a host's relay and bootstrap node in.
const HOST_UNIT: &str = "estafette-host";

/// A second lab host, relay and bootstrap node in one process, which the acceptance gate lets the test kill.
pub struct KillableHost {
    server: String,
    address: HostAddr,
}

impl KillableHost {
    /// Reads the server and the host address line the acceptance gate exports, refusing to run when either is unset.
    pub fn from_environment() -> Self {
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
            server: exported(SERVER_VARIABLE),
            address,
        }
    }

    /// The host's id and relay, as its address line names them.
    pub const fn address(&self) -> &HostAddr {
        &self.address
    }

    /// The host's relay URL.
    pub fn relay(&self) -> &str {
        self.address.relay.as_str()
    }

    /// Kills the host's process, relay and bootstrap node together, with SIGKILL, and returns the instant the kill command returned.
    pub fn kill(&self) -> Instant {
        let killed = Command::new(TASK_RUNNER)
            .args([
                REMOTE_SHELL_RECIPE,
                "root",
                "systemctl",
                "kill",
                "--kill-whom=main",
                "--signal=SIGKILL",
                HOST_UNIT,
            ])
            .env(LAB_SERVER_VARIABLE, &self.server)
            .output()
            .unwrap();
        let returned = Instant::now();
        assert!(
            killed.status.success(),
            "the kill of {HOST_UNIT} on {} failed: {}",
            self.server,
            String::from_utf8_lossy(&killed.stderr)
        );
        returned
    }
}
