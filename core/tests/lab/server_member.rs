use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::lab_machine::LabMachine;
use crate::lab_server::LabServer;
use crate::live_network::LiveNetwork;
use crate::mcp_server::{HANDLE_KEY, PEERS_KEY, PEERS_TOOL, RESULT_KEY, SEND_TOOL, send_arguments};
use crate::scratch_machine::server_member_end::{
    ServerMemberStep, TestSoFar, ended_server_member, recipe_failure,
};
use crate::scratch_machine::{
    KeptSecrets, TIMELINE_HEADING, fail_unless_panicking, print_to_standard_error,
};

/// The recipe that writes the lab network's configuration on a server from its standard input.
const NETWORK_ON_SERVER_RECIPE: &str = "remote::network-on-server";
/// The recipe that creates a card on a server and prints its public key.
const CARD_ON_SERVER_RECIPE: &str = "remote::card-on-server";
/// The recipe that serves a card as a lab member on a server.
const MEMBER_ON_SERVER_RECIPE: &str = "remote::member-on-server";
/// The recipe that calls one tool of a member on a server, the arguments on its standard input.
const CALL_ON_SERVER_RECIPE: &str = "remote::call-on-server";
/// The recipe that stops a member on a server cleanly, waiting out its departure.
const STOP_MEMBER_ON_SERVER_RECIPE: &str = "remote::stop-member-on-server";
/// The recipe that prints the run record of a member on a server.
const RUN_RECORD_ON_SERVER_RECIPE: &str = "remote::run-record-on-server";
/// The recipe that fetches both trace files of each run of a member on a server into a directory.
const TRACE_FILES_ON_SERVER_RECIPE: &str = "remote::trace-files-on-server";
/// How long a member on a server may take to claim its card and answer a tool call, each call crossing SSH and the cloud API.
const SERVING_WAIT: Duration = Duration::from_secs(60);
/// The pause the re-asks of a member on a server were once spaced by, added to each re-ask's bound so the last re-ask may start as late as it once could.
const RE_ASKS_ONCE_SPACED_BY: Duration = Duration::from_secs(1);
/// What separates the server's clock stamp from the envelope a remote tool call prints.
const STAMP_SEPARATOR: char = '\t';

/// A lab member serving a card on a lab server, configured as the live network's invitation describes, stopped when dropped with its run record and trace files kept when the test asks, searched for the secrets read where it started, a failed stop marked beside them, and its timeline printed when the test fails.
pub struct ServerMember<'server> {
    server: &'server LabServer,
    card: String,
    secrets: io::Result<KeptSecrets>,
    kept_in: Option<PathBuf>,
}

impl<'server> ServerMember<'server> {
    /// Writes the network configuration the invitation describes on the server, creates a card there named for the handle and requesting it, and serves it, owned from before its start so a failed start still stops it, until it answers a tool call.
    #[track_caller]
    pub fn started(server: &'server LabServer, network: &LiveNetwork, handle: &str) -> Self {
        let configuration = invitation_configuration(network, handle);
        let run_records = network.run_records();
        let secrets = KeptSecrets::of_server_member(&configuration, run_records.invitation());
        server.run(
            NETWORK_ON_SERVER_RECIPE,
            &[],
            configuration.as_bytes(),
            secrets.as_ref().ok(),
        );
        server.run(
            CARD_ON_SERVER_RECIPE,
            &[handle, handle],
            &[],
            secrets.as_ref().ok(),
        );
        let member = Self {
            server,
            card: handle.to_owned(),
            secrets,
            kept_in: run_records.kept_in().map(Path::to_path_buf),
        };
        server.run(
            MEMBER_ON_SERVER_RECIPE,
            &[handle],
            &[],
            member.secrets.as_ref().ok(),
        );
        member.serving_within(SERVING_WAIT);
        member
    }

    /// The handle the member requested, which no earlier member on the network holds.
    pub fn handle(&self) -> &str {
        &self.card
    }

    /// Calls the tool over the server's control socket and returns the envelope its answer carries.
    #[track_caller]
    pub fn call(&self, tool: &str, arguments: &Value) -> Value {
        let printed = self.server.run(
            CALL_ON_SERVER_RECIPE,
            &[&self.card, tool],
            arguments.to_string().as_bytes(),
            self.secrets.as_ref().ok(),
        );
        let (_stamp, envelope) = printed
            .trim_end()
            .split_once(STAMP_SEPARATOR)
            .unwrap_or_else(|| {
                panic!(
                    "{tool} on {} printed no stamped envelope: {printed}",
                    self.server.label()
                )
            });
        serde_json::from_str(envelope).unwrap()
    }

    /// Sends the body to the handle through the send tool and returns the envelope.
    #[track_caller]
    pub fn send(&self, to: &str, body: &str) -> Value {
        self.call(SEND_TOOL, &send_arguments(to, body))
    }

    /// The peer of that handle as the peers tool lists it once the peer meets the condition, requiring that within the wait.
    #[track_caller]
    pub fn peer_within(
        &self,
        handle: &str,
        within: Duration,
        condition: impl Fn(&Value) -> bool,
    ) -> Value {
        let started = Instant::now();
        let until = within.saturating_add(RE_ASKS_ONCE_SPACED_BY);
        loop {
            let listed = self.call(PEERS_TOOL, &json!({}));
            let entry = listed[RESULT_KEY][PEERS_KEY]
                .as_array()
                .into_iter()
                .flatten()
                .find(|peer| peer[HANDLE_KEY] == handle);
            if let Some(peer) = entry.filter(|peer| condition(peer)) {
                return peer.clone();
            }
            assert!(
                started.elapsed() < until,
                "{handle} was not listed as wanted by {} on {} within {until:?}; its entry: {entry:?}",
                self.card,
                self.server.label()
            );
        }
    }

    /// Waits until the member answers a tool call, the sign it claimed its card and listens on its control socket.
    #[track_caller]
    fn serving_within(&self, within: Duration) {
        let started = Instant::now();
        let until = within.saturating_add(RE_ASKS_ONCE_SPACED_BY);
        loop {
            let answered =
                self.server
                    .attempt(CALL_ON_SERVER_RECIPE, &[&self.card, PEERS_TOOL], b"{}");
            if answered.status.success() {
                return;
            }
            assert!(
                started.elapsed() < until,
                "{} on {} answered no tool call within {until:?}: {}",
                self.card,
                self.server.label(),
                recipe_failure(&answered, self.secrets.as_ref().ok())
            );
        }
    }

    /// Runs the recipe of the step of the member's end on its server, unredacted where it fetches trace files, so its raw files are not safe to post, never panicking.
    fn run_end_step(&self, step: ServerMemberStep<'_>) -> io::Result<Output> {
        match step {
            ServerMemberStep::Stop => self.server.attempt_without_panicking(
                STOP_MEMBER_ON_SERVER_RECIPE,
                &[&self.card],
                &[],
            ),
            ServerMemberStep::PrintRunRecord => self.server.attempt_without_panicking(
                RUN_RECORD_ON_SERVER_RECIPE,
                &[&self.card],
                &[],
            ),
            ServerMemberStep::FetchTraceFiles(into) => self.server.attempt_without_panicking(
                TRACE_FILES_ON_SERVER_RECIPE,
                &[&self.card, &into.to_string_lossy()],
                &[],
            ),
        }
    }

    /// Prints the timeline a failing test shows for the member.
    fn print_timeline(&self, timeline: &str) {
        print_to_standard_error(&format!(
            "{TIMELINE_HEADING} {} on {}:\n{timeline}",
            self.card,
            self.server.label()
        ));
    }
}

impl Drop for ServerMember<'_> {
    fn drop(&mut self) {
        let test = if std::thread::panicking() {
            TestSoFar::Failing
        } else {
            TestSoFar::NotFailing
        };
        let ended = ended_server_member(
            &self.card,
            self.server.label(),
            self.kept_in.as_deref(),
            &self.secrets,
            test,
            |step| self.run_end_step(step),
        );
        if let Some(timeline) = &ended.timeline {
            self.print_timeline(timeline);
        }
        if !ended.failures.is_empty() {
            fail_unless_panicking(&ended.failures.join("\n"));
        }
    }
}

/// The network configuration the invitation describes, as joining the network writes it on a scratch machine named for the handle.
fn invitation_configuration(network: &LiveNetwork, handle: &str) -> String {
    let configured = LabMachine::joined(handle, network);
    fs::read_to_string(
        configured
            .directories()
            .network_config_file(network.name())
            .unwrap(),
    )
    .unwrap()
}
