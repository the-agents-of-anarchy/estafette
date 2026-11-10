use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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
/// The line the host prints once the SSH session is open and the kill waits only for its trigger.
const ARMED_LINE: &str = "armed";
/// The systemd unit the infrastructure recipe runs a host's relay and bootstrap node in.
const HOST_UNIT: &str = "estafette-host";
/// How long the kill's SSH session may take to open and stand ready, the server's address lookup included.
const ARMED_WAIT: Duration = Duration::from_secs(60);

/// A second lab host, relay and bootstrap node in one process, which the acceptance gate lets the test kill.
pub struct KillableHost {
    server: String,
    address: HostAddr,
}

/// When the kill was sent over the open session, and when its command returned.
pub struct KillStamps {
    pub sent: Instant,
    pub returned: Instant,
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

    /// Opens the kill's SSH session and waits for the host to stand ready, then stamps the instant, sends the kill over the open session and stamps its return, so only the warmed round trip separates the stamp from the kill.
    #[track_caller]
    pub fn kill(&self) -> KillStamps {
        let mut session = Command::new(TASK_RUNNER)
            .args([REMOTE_SHELL_RECIPE, "root", &armed_kill()])
            .env(LAB_SERVER_VARIABLE, &self.server)
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
                self.server
            );
        }
        let mut trigger = session.stdin.take().unwrap();
        let sent = Instant::now();
        trigger.write_all(b"\n").unwrap();
        drop(trigger);
        let killed = session.wait_with_output().unwrap();
        let returned = Instant::now();
        assert!(
            killed.status.success(),
            "the kill on {} failed: {}",
            self.server,
            String::from_utf8_lossy(&killed.stderr)
        );
        KillStamps { sent, returned }
    }
}

/// The remote command, quoted as one word for the recipe's shell: announce the open session, wait for the trigger line, then SIGKILL the host's main process, relay and bootstrap node together.
fn armed_kill() -> String {
    format!(
        "'echo {ARMED_LINE} && read -r _ && systemctl kill --kill-whom=main --signal=SIGKILL {HOST_UNIT}'"
    )
}
