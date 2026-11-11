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

/// A lab server the acceptance gate brought up, reached through the remote recipes.
pub struct LabServer {
    name: Option<String>,
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
}
