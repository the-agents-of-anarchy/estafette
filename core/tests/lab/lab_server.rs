use std::io::Write;
use std::process::{Command, Output, Stdio};

use crate::live_network::{LAB_BINARY_VARIABLE, TASK_RUNNER};
use crate::scratch_machine::{BINARY, fail_unless_panicking};

/// The variable the remote recipes take the server's name from; left as the gate left it, they reach the live host's server.
const LAB_SERVER_VARIABLE: &str = "LAB_SERVER";
/// How messages and kept records name the live host's server, whose name the gate leaves to the recipes.
const LIVE_HOST_SERVER_LABEL: &str = "live-host-server";
/// The recipe that drops UDP between a server and this machine.
const RELAY_ONLY_RECIPE: &str = "remote::relay-only-to-here";
/// The recipe that deletes those drops, printing their counters.
const RELAY_PATHS_RESTORED_RECIPE: &str = "remote::relay-paths-restored";

/// A lab server the acceptance gate brought up, reached through the remote recipes.
pub struct LabServer {
    name: Option<String>,
}

/// UDP between a lab server and this machine dropped both ways, so members on the two reach each other through a relay alone; restored when dropped, whether the test ended or failed.
pub struct RelayOnlyPath<'server> {
    server: &'server LabServer,
}

impl LabServer {
    /// The server of the live host, which the remote recipes reach as the gate left them.
    pub const fn of_the_live_host() -> Self {
        Self { name: None }
    }

    /// The server of that name.
    pub const fn named(name: String) -> Self {
        Self { name: Some(name) }
    }

    /// The server's name in messages and kept records.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(LIVE_HOST_SERVER_LABEL)
    }

    /// The task runner set to run the remote recipe with the arguments against this server, with this machine's binary.
    pub fn recipe_command(&self, recipe: &str, arguments: &[&str]) -> Command {
        let mut command = Command::new(TASK_RUNNER);
        command
            .arg(recipe)
            .args(arguments)
            .env(LAB_BINARY_VARIABLE, BINARY);
        if let Some(name) = &self.name {
            command.env(LAB_SERVER_VARIABLE, name);
        }
        command
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
        let mut running = self
            .recipe_command(recipe, arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
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

impl Drop for RelayOnlyPath<'_> {
    fn drop(&mut self) {
        let restored = self.server.attempt(RELAY_PATHS_RESTORED_RECIPE, &[], &[]);
        eprintln!(
            "UDP paths between {} and this machine restored; the drops counted by each rule:\n{}",
            self.server.label(),
            String::from_utf8_lossy(&restored.stdout)
        );
        if !restored.status.success() {
            fail_unless_panicking(&format!(
                "the UDP drops on {} were not deleted: {}",
                self.server.label(),
                String::from_utf8_lossy(&restored.stderr)
            ));
        }
    }
}
