use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use estafette::invite::parse_host_line;
use estafette::wire::HostAddr;

use crate::lab_server::LabServer;
use crate::scratch_machine::fail_unless_panicking;

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

/// A second lab host, relay and bootstrap node in one process on a lab server of its own, which the acceptance gate lets the test stop, start and kill.
pub struct KillableHost {
    server: LabServer,
    address: HostAddr,
}

/// When the kill was sent over the open session, and when its command returned.
pub struct KillStamps {
    pub sent: Instant,
    pub returned: Instant,
}

/// The killable host started again when dropped, whether the test that stopped or killed it ended or failed, so every later test finds it serving.
pub struct HostStartedWhenDropped<'killable> {
    killable: &'killable KillableHost,
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
            server: LabServer::named(exported(SERVER_VARIABLE)),
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

    /// The server the host runs on, for members and paths on it.
    pub const fn server(&self) -> &LabServer {
        &self.server
    }

    /// Starts the host on its server unless it runs, with the identity and certificate it had, failing with the server's name when the server does not exist.
    #[track_caller]
    pub fn start(&self) {
        self.server.run(HOST_STARTED_RECIPE, &[], &[]);
    }

    /// Stops the host on its server when it runs, relay and bootstrap node together.
    #[track_caller]
    pub fn stop(&self) {
        self.server.run(HOST_STOPPED_RECIPE, &[], &[]);
    }

    /// Starts the host again once the returned guard is dropped, so a test may stop or kill it and leave it serving.
    pub const fn started_when_dropped(&self) -> HostStartedWhenDropped<'_> {
        HostStartedWhenDropped { killable: self }
    }

    /// Opens the kill's SSH session and waits for the host to stand ready, then stamps the instant, sends the kill over the open session and stamps its return, so only the warmed round trip separates the stamp from the kill.
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
        let sent = Instant::now();
        trigger.write_all(b"\n").unwrap();
        drop(trigger);
        let killed = session.wait_with_output().unwrap();
        let returned = Instant::now();
        assert!(
            killed.status.success(),
            "the kill on {} failed: {}",
            self.server.label(),
            String::from_utf8_lossy(&killed.stderr)
        );
        KillStamps { sent, returned }
    }
}

impl Drop for HostStartedWhenDropped<'_> {
    fn drop(&mut self) {
        let server = &self.killable.server;
        let started = server.attempt(HOST_STARTED_RECIPE, &[], &[]);
        if !started.status.success() {
            fail_unless_panicking(&format!(
                "the host on {} was not started again: {}",
                server.label(),
                String::from_utf8_lossy(&started.stderr)
            ));
        }
    }
}

/// The remote command, quoted as one word for the recipe's shell: announce the open session, wait for the trigger line, then SIGKILL the host's main process, relay and bootstrap node together.
fn armed_kill() -> String {
    format!(
        "'echo {ARMED_LINE} && read -r _ && systemctl kill --kill-whom=main --signal=SIGKILL {HOST_UNIT}'"
    )
}
