use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::lab_machine::LabMachine;
use crate::lab_server::LabServer;
use crate::live_network::LiveNetwork;
use crate::mcp_server::{HANDLE_KEY, PEERS_KEY, PEERS_TOOL, RESULT_KEY, SEND_TOOL, send_arguments};
use crate::scratch_machine::trace_reader::timeline_in;
use crate::scratch_machine::{TIMELINE_HEADING, fail_unless_panicking, keep_run_record};

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
/// What ends the name of the directory a member's fetched trace files are kept in, after its server and card.
const KEPT_TRACE_FILES_SUFFIX: &str = "traces";
/// What begins the name of the scratch directory a failing test fetches a member's trace files into for its timeline alone.
const FETCHED_FOR_THE_PRINT_PREFIX: &str = "ea-timeline";
/// What a failing test prints in place of a member's timeline when its trace files could not be fetched.
const TRACE_FILES_NOT_FETCHED: &str =
    "its trace files were not fetched, so what it recorded is unknown";
/// How long a member on a server may take to claim its card and answer a tool call, each call crossing SSH and the cloud API.
const SERVING_WAIT: Duration = Duration::from_secs(60);
/// How long to wait before asking a member on a server again, since each ask crosses SSH and the cloud API.
const REMOTE_POLL: Duration = Duration::from_millis(1000);
/// What separates the server's clock stamp from the envelope a remote tool call prints.
const STAMP_SEPARATOR: char = '\t';

/// A lab member serving a card on a lab server, configured as the live network's invitation describes, stopped when dropped with its run record and trace files kept when the test asks and its timeline printed when the test fails.
pub struct ServerMember<'server> {
    server: &'server LabServer,
    card: String,
    secrets: Vec<String>,
    kept_in: Option<PathBuf>,
}

impl<'server> ServerMember<'server> {
    /// Writes the network configuration the invitation describes on the server, creates a card there named for the handle and requesting it, and serves it until it answers a tool call.
    #[track_caller]
    pub fn started(server: &'server LabServer, network: &LiveNetwork, handle: &str) -> Self {
        let configuration = invitation_configuration(network, handle);
        server.run(NETWORK_ON_SERVER_RECIPE, &[], configuration.as_bytes());
        let card_key = server.run(CARD_ON_SERVER_RECIPE, &[handle, handle], &[]);
        server.run(MEMBER_ON_SERVER_RECIPE, &[handle], &[]);
        let run_records = network.run_records();
        let member = Self {
            server,
            card: handle.to_owned(),
            secrets: run_records.secrets_longest_first([configuration.as_str(), card_key.as_str()]),
            kept_in: run_records.kept_in().map(Path::to_path_buf),
        };
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
        );
        let (_stamp, envelope) = printed.trim_end().split_once(STAMP_SEPARATOR).unwrap();
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
                started.elapsed() < within,
                "{handle} was not listed as wanted by {} on {} within {within:?}; its entry: {entry:?}",
                self.card,
                self.server.label()
            );
            std::thread::sleep(REMOTE_POLL);
        }
    }

    /// Waits until the member answers a tool call, the sign it claimed its card and listens on its control socket.
    #[track_caller]
    fn serving_within(&self, within: Duration) {
        let started = Instant::now();
        loop {
            let answered =
                self.server
                    .attempt(CALL_ON_SERVER_RECIPE, &[&self.card, PEERS_TOOL], b"{}");
            if answered.status.success() {
                return;
            }
            assert!(
                started.elapsed() < within,
                "{} on {} answered no tool call within {within:?}: {}",
                self.card,
                self.server.label(),
                String::from_utf8_lossy(&answered.stderr)
            );
            std::thread::sleep(REMOTE_POLL);
        }
    }

    /// Keeps the member's run record in the directory by the scratch machines' layout and redaction rule, its secrets those of its configuration and card, failing the test when the record cannot be read or kept.
    fn keep_run_record(&self, directory: &Path) {
        let printed = self
            .server
            .attempt(RUN_RECORD_ON_SERVER_RECIPE, &[&self.card], &[]);
        if !printed.status.success() {
            fail_unless_panicking(&format!(
                "run record of {} on {} not read: {}",
                self.card,
                self.server.label(),
                String::from_utf8_lossy(&printed.stderr)
            ));
            return;
        }
        let record = String::from_utf8_lossy(&printed.stdout);
        if let Err(failure) = keep_run_record(
            directory,
            self.server.label(),
            &self.card,
            &record,
            &self.secrets,
        ) {
            fail_unless_panicking(&format!("run record of {} not kept: {failure}", self.card));
        }
    }

    /// The directory beside the member's run record that its trace files are kept in.
    fn kept_trace_files_directory(&self, directory: &Path) -> PathBuf {
        directory.join(format!(
            "{}-{}-{KEPT_TRACE_FILES_SUFFIX}",
            self.server.label(),
            self.card
        ))
    }

    /// Fetches both trace files of each of the member's runs into the directory, unredacted, so its raw files are not safe to post, and answers the directory when they were fetched, failing the test when they were not.
    fn fetch_trace_files<'into>(&self, into: &'into Path) -> Option<&'into Path> {
        let fetched = self.server.attempt(
            TRACE_FILES_ON_SERVER_RECIPE,
            &[&self.card, &into.to_string_lossy()],
            &[],
        );
        if !fetched.status.success() {
            fail_unless_panicking(&format!(
                "trace files of {} on {} not fetched: {}",
                self.card,
                self.server.label(),
                String::from_utf8_lossy(&fetched.stderr)
            ));
        }
        fetched.status.success().then_some(into)
    }

    /// Keeps both trace files of each of the member's runs beside its run record and answers where, when they were fetched.
    fn keep_trace_files(&self, directory: &Path) -> Option<PathBuf> {
        let kept_trace_files = self.kept_trace_files_directory(directory);
        let fetched = self.fetch_trace_files(&kept_trace_files)?;
        eprintln!("trace files of {} kept at {}", self.card, fetched.display());
        Some(fetched.to_path_buf())
    }

    /// Prints the timeline a failing test shows for the member.
    fn print_timeline(&self, timeline: &str) {
        eprintln!(
            "{TIMELINE_HEADING} {} on {}:\n{timeline}",
            self.card,
            self.server.label()
        );
    }

    /// The member's timeline read from a copy of its trace files fetched into a scratch directory, which is removed once read.
    fn timeline_fetched_for_the_print(&self) -> String {
        let fetched_for_the_print = std::env::temp_dir().join(format!(
            "{FETCHED_FOR_THE_PRINT_PREFIX}-{}-{}-{}",
            std::process::id(),
            self.server.label(),
            self.card
        ));
        let timeline = timeline_of_fetched(self.fetch_trace_files(&fetched_for_the_print));
        drop(fs::remove_dir_all(&fetched_for_the_print));
        timeline
    }
}

impl Drop for ServerMember<'_> {
    fn drop(&mut self) {
        let stopped = self
            .server
            .attempt(STOP_MEMBER_ON_SERVER_RECIPE, &[&self.card], &[]);
        if !stopped.status.success() {
            eprintln!(
                "{} on {} did not stop cleanly: {}",
                self.card,
                self.server.label(),
                String::from_utf8_lossy(&stopped.stderr)
            );
        }
        let kept_trace_files = self.kept_in.as_deref().map(|directory| {
            self.keep_run_record(directory);
            self.keep_trace_files(directory)
        });
        if std::thread::panicking() {
            let timeline = kept_trace_files.map_or_else(
                || self.timeline_fetched_for_the_print(),
                |kept| timeline_of_fetched(kept.as_deref()),
            );
            self.print_timeline(&timeline);
        }
    }
}

/// The member's timeline read from the directory its trace files were fetched into, or, when they were not fetched, that its timeline is unknown.
fn timeline_of_fetched(fetched_into: Option<&Path>) -> String {
    fetched_into.map_or_else(|| TRACE_FILES_NOT_FETCHED.to_owned(), timeline_in)
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{TRACE_FILES_NOT_FETCHED, timeline_of_fetched};
    use crate::scratch_machine::trace_reader::NO_EVENT_RECORDED;

    #[test]
    fn observability_decision_15_a_member_whose_trace_files_were_not_fetched_is_printed_as_not_fetched_not_as_recording_nothing()
     {
        let fetched_into =
            std::env::temp_dir().join(format!("server-member-{}-fetched-into", std::process::id()));
        fs::create_dir_all(&fetched_into).unwrap();

        let not_fetched = timeline_of_fetched(None);
        let fetched_with_no_event = timeline_of_fetched(Some(&fetched_into));

        assert_eq!(not_fetched, TRACE_FILES_NOT_FETCHED);
        assert_eq!(fetched_with_no_event, NO_EVENT_RECORDED);
        drop(fs::remove_dir(&fetched_into));
    }
}
