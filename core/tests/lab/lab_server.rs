use std::io::{self, Write};
use std::process::{Command, Output, Stdio};

use crate::live_network::{LAB_BINARY_VARIABLE, TASK_RUNNER};
use crate::scratch_machine::server_member_end::{fail_unless_succeeded, recipe_failure, succeeded};
use crate::scratch_machine::{BINARY, KeptSecrets};

/// The variable the remote recipes take the server's name from; left as the gate left it, they reach the live host's server.
const LAB_SERVER_VARIABLE: &str = "LAB_SERVER";
/// How messages and kept records name the live host's server, whose name the gate leaves to the recipes.
const LIVE_HOST_SERVER_LABEL: &str = "live-host-server";
/// The recipe that drops UDP between a server and this machine.
const RELAY_ONLY_RECIPE: &str = "remote::relay-only-to-here";
/// The recipe that deletes those drops, printing their counters.
const RELAY_PATHS_RESTORED_RECIPE: &str = "remote::relay-paths-restored";

/// Which lab server the remote recipes reach: the live host's, whose name the gate leaves to them, or one by name.
enum LabServerName {
    LiveHost,
    Named(String),
}

/// A lab server the acceptance gate brought up, reached through the remote recipes.
pub struct LabServer {
    name: LabServerName,
}

/// UDP between a lab server and this machine dropped both ways, so members on the two reach each other through a relay alone; restored when dropped, whether the test ended or failed.
pub struct RelayOnlyPath<'server> {
    server: &'server LabServer,
}

impl LabServer {
    /// The server of the live host, which the remote recipes reach as the gate left them.
    pub const fn of_the_live_host() -> Self {
        Self {
            name: LabServerName::LiveHost,
        }
    }

    /// The server of that name.
    pub const fn named(name: String) -> Self {
        Self {
            name: LabServerName::Named(name),
        }
    }

    /// The server's name in messages and kept records.
    pub fn label(&self) -> &str {
        match &self.name {
            LabServerName::LiveHost => LIVE_HOST_SERVER_LABEL,
            LabServerName::Named(name) => name,
        }
    }

    /// The task runner set to run the remote recipe with the arguments against this server, with this machine's binary.
    pub fn recipe_command(&self, recipe: &str, arguments: &[&str]) -> Command {
        let mut command = Command::new(TASK_RUNNER);
        command
            .arg(recipe)
            .args(arguments)
            .env(LAB_BINARY_VARIABLE, BINARY);
        match &self.name {
            LabServerName::LiveHost => {}
            LabServerName::Named(name) => {
                command.env(LAB_SERVER_VARIABLE, name);
            }
        }
        command
    }

    /// Runs the remote recipe against this server with the input on its standard input, requiring success, its failure printed with its standard error searched for the secrets or left out when none stand, and returns what it printed.
    #[track_caller]
    pub fn run(
        &self,
        recipe: &str,
        arguments: &[&str],
        input: &[u8],
        secrets: Option<&KeptSecrets>,
    ) -> String {
        let output = self.attempt(recipe, arguments, input);
        assert!(
            output.status.success(),
            "{recipe} {arguments:?} on {} failed: {}",
            self.label(),
            recipe_failure(&output, secrets)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Runs the remote recipe against this server with the input on its standard input and returns how it ended.
    #[track_caller]
    pub fn attempt(&self, recipe: &str, arguments: &[&str], input: &[u8]) -> Output {
        self.attempt_without_panicking(recipe, arguments, input)
            .unwrap()
    }

    /// Runs the remote recipe against this server with the input on its standard input and returns how it ended, or why it could not run, never panicking.
    pub fn attempt_without_panicking(
        &self,
        recipe: &str,
        arguments: &[&str],
        input: &[u8],
    ) -> io::Result<Output> {
        let mut running = self
            .recipe_command(recipe, arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let written = running
            .stdin
            .take()
            .map_or(Ok(()), |mut recipe_input| recipe_input.write_all(input));
        let output = running.wait_with_output()?;
        written.map(|()| output)
    }

    /// Drops UDP between this server and this machine until the returned path is dropped, which also undoes a drop that failed half way.
    #[track_caller]
    pub fn relay_only_to_here(&self) -> RelayOnlyPath<'_> {
        let path = RelayOnlyPath { server: self };
        self.run(RELAY_ONLY_RECIPE, &[], &[], None);
        path
    }
}

impl Drop for RelayOnlyPath<'_> {
    fn drop(&mut self) {
        let restored = self
            .server
            .attempt_without_panicking(RELAY_PATHS_RESTORED_RECIPE, &[], &[]);
        if let Ok(output) = succeeded(&restored) {
            eprintln!(
                "UDP paths between {} and this machine restored; the drops counted by each rule:\n{}",
                self.server.label(),
                String::from_utf8_lossy(&output.stdout)
            );
        }
        fail_unless_succeeded(
            &restored,
            &format!("the UDP drops on {} were not deleted", self.server.label()),
        );
    }
}
