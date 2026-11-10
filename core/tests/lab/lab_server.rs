use std::io::Write;
use std::process::{Command, Output, Stdio};

use crate::live_network::TASK_RUNNER;
use crate::scratch_machine::BINARY;

/// The variable the acceptance gate names the server of the host the test may kill in.
const KILLABLE_SERVER_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_KILLABLE_SERVER";
/// The variable the remote recipes take the server's name from; left as the gate left it, they reach the live host's server.
const LAB_SERVER_VARIABLE: &str = "LAB_SERVER";
/// The variable the remote recipes take this machine's binary from.
const LAB_BINARY_VARIABLE: &str = "LAB_BINARY";
/// How messages and kept records name the live host's server, whose name the gate leaves to the recipes.
const LIVE_HOST_SERVER_LABEL: &str = "live-host-server";
/// The recipe that drops UDP between a server and this machine.
const RELAY_ONLY_RECIPE: &str = "remote::relay-only-to-here";
/// The recipe that deletes those drops, printing their counters.
const RELAY_PATHS_RESTORED_RECIPE: &str = "remote::relay-paths-restored";
/// The recipe that starts the network's host on a server, reusing a live one.
const HOST_STARTED_RECIPE: &str = "remote::infrastructure";

/// A lab server the acceptance gate brought up, reached through the remote recipes.
pub struct LabServer {
    name: Option<String>,
}

/// UDP between a lab server and this machine dropped both ways, so members on the two reach each other through a relay alone; restored when dropped, whether the test ended or failed.
pub struct RelayOnlyPath<'server> {
    server: &'server LabServer,
}

/// The server of the host the test may kill, the one server whose host the test stops and starts.
pub struct KillableHostServer {
    server: LabServer,
}

impl LabServer {
    /// The server of the live host, which the remote recipes reach as the gate left them.
    pub const fn of_the_live_host() -> Self {
        Self { name: None }
    }

    /// The server's name in messages and kept records.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(LIVE_HOST_SERVER_LABEL)
    }

    /// Runs the remote recipe against this server with the input on its standard input, requiring success, and returns what it printed.
    #[track_caller]
    pub fn run(&self, recipe: &str, arguments: &[&str], input: &[u8]) -> String {
        let output = self.attempt(recipe, arguments, input);
        assert!(
            output.status.success(),
            "{recipe} {arguments:?} on {} failed: {}",
            self.label(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Runs the remote recipe against this server with the input on its standard input and returns how it ended.
    pub fn attempt(&self, recipe: &str, arguments: &[&str], input: &[u8]) -> Output {
        let mut command = Command::new(TASK_RUNNER);
        command
            .arg(recipe)
            .args(arguments)
            .env(LAB_BINARY_VARIABLE, BINARY)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(name) = &self.name {
            command.env(LAB_SERVER_VARIABLE, name);
        }
        let mut running = command.spawn().unwrap();
        running.stdin.take().unwrap().write_all(input).unwrap();
        running.wait_with_output().unwrap()
    }

    /// Drops UDP between this server and this machine until the returned path is dropped, which also undoes a drop that failed half way.
    #[track_caller]
    pub fn relay_only_to_here(&self) -> RelayOnlyPath<'_> {
        let path = RelayOnlyPath { server: self };
        self.run(RELAY_ONLY_RECIPE, &[], &[]);
        path
    }
}

impl KillableHostServer {
    /// Reads the server's name from the variable the acceptance gate exports, refusing to run when it is unset.
    pub fn from_environment() -> Self {
        let name = std::env::var(KILLABLE_SERVER_VARIABLE).unwrap_or_else(|source| {
            panic!(
                "{KILLABLE_SERVER_VARIABLE} is not usable ({source}): run `just gate::acceptance`, which exports the server of the host the test may kill"
            )
        });
        Self {
            server: LabServer { name: Some(name) },
        }
    }

    /// The server itself, for members and paths on it.
    pub const fn server(&self) -> &LabServer {
        &self.server
    }

    /// Starts the host on the server unless it runs, with the identity and certificate it had.
    #[track_caller]
    pub fn start_host(&self) {
        self.server.run(HOST_STARTED_RECIPE, &[], &[]);
    }
}

impl Drop for RelayOnlyPath<'_> {
    fn drop(&mut self) {
        let restored = self.server.attempt(RELAY_PATHS_RESTORED_RECIPE, &[], &[]);
        eprintln!(
            "UDP paths between {} and this machine restored; the drops counted by each rule:\n{}",
            self.server.label(),
            String::from_utf8_lossy(&restored.stdout)
        );
        if restored.status.success() {
            return;
        }
        let failure = format!(
            "the UDP drops on {} were not deleted: {}",
            self.server.label(),
            String::from_utf8_lossy(&restored.stderr)
        );
        if std::thread::panicking() {
            eprintln!("{failure}");
        } else {
            panic!("{failure}");
        }
    }
}
