use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::lab_machine::LabMachine;
use crate::lab_server::LabServer;
use crate::live_network::LiveNetwork;
use crate::scratch_machine::{INVITATION_VARIABLE, KEPT_RUN_RECORDS_VARIABLE, REDACTED};

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
/// How long a member on a server may take to claim its card and answer a tool call, each call crossing SSH and the cloud API.
const SERVING_WAIT: Duration = Duration::from_secs(60);
/// How long to wait before asking a member on a server again, since each ask crosses SSH and the cloud API.
const REMOTE_POLL: Duration = Duration::from_millis(1000);
/// What separates the server's clock stamp from the envelope a remote tool call prints.
const STAMP_SEPARATOR: char = '\t';
/// The fewest letters and digits in a row that a key, a secret or an invitation is written with.
const SHORTEST_SECRET_RUN: usize = 16;
/// The directory a test's records are kept under when the harness gave its thread no name.
const UNNAMED_TEST: &str = "unnamed-test";
/// The extension of a kept run record.
const RUN_RECORD_EXTENSION: &str = "stderr";

/// A lab member serving a card on a lab server, configured as the live network's invitation describes, stopped when dropped with its run record kept when asked.
pub struct ServerMember<'server> {
    server: &'server LabServer,
    card: String,
    secrets: Vec<String>,
}

impl<'server> ServerMember<'server> {
    /// Writes the network configuration the invitation describes on the server, creates a card there named for the handle and requesting it, and serves it until it answers a tool call.
    #[track_caller]
    pub fn started(server: &'server LabServer, network: &LiveNetwork, handle: &str) -> Self {
        let configuration = invitation_configuration(network, handle);
        server.run(NETWORK_ON_SERVER_RECIPE, &[], configuration.as_bytes());
        let card_key = server.run(CARD_ON_SERVER_RECIPE, &[handle, handle], &[]);
        server.run(MEMBER_ON_SERVER_RECIPE, &[handle], &[]);
        let member = Self {
            server,
            card: handle.to_owned(),
            secrets: secrets_by_length(&configuration, card_key.trim()),
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
            let listed = self.call("peers", &json!({}));
            let entry = listed["result"]["peers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|peer| peer["handle"] == handle);
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
                    .attempt(CALL_ON_SERVER_RECIPE, &[&self.card, "peers"], b"{}");
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

    /// Writes the member's run record, each secret replaced by the redaction marker, under the directory in a directory named for the running test.
    fn keep_run_record(&self, directory: &Path) {
        let printed = self
            .server
            .attempt(RUN_RECORD_ON_SERVER_RECIPE, &[&self.card], &[]);
        let test = std::thread::current()
            .name()
            .unwrap_or(UNNAMED_TEST)
            .replace(':', "-");
        let kept = directory.join(test).join(format!(
            "{}-{}.{RUN_RECORD_EXTENSION}",
            self.server.label(),
            self.card
        ));
        let record = redacted(&String::from_utf8_lossy(&printed.stdout), &self.secrets);
        let written = kept
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&kept, record));
        match written {
            Ok(()) if printed.status.success() => {
                eprintln!("run record kept at {}", kept.display());
            }
            Ok(()) => eprintln!(
                "run record of {} on {} not read: {}",
                self.card,
                self.server.label(),
                String::from_utf8_lossy(&printed.stderr)
            ),
            Err(failure) => eprintln!("run record of {} not kept: {failure}", self.card),
        }
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
        if let Some(directory) = std::env::var_os(KEPT_RUN_RECORDS_VARIABLE) {
            self.keep_run_record(Path::new(&directory));
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

/// Every line of the configuration holding a long run of letters and digits, each such run, the card's key and the exported invitation, longest first so a whole line is replaced before a part of it.
fn secrets_by_length(configuration: &str, card_key: &str) -> Vec<String> {
    let mut secrets = BTreeSet::from([card_key.to_owned()]);
    for line in configuration.lines() {
        let runs: Vec<&str> = line
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|run| run.len() >= SHORTEST_SECRET_RUN)
            .collect();
        if !runs.is_empty() {
            secrets.insert(line.trim().to_owned());
        }
        secrets.extend(runs.into_iter().map(str::to_owned));
    }
    if let Ok(invitation) = std::env::var(INVITATION_VARIABLE) {
        secrets.insert(invitation.trim().to_owned());
    }
    let mut by_length: Vec<String> = secrets.into_iter().collect();
    by_length.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    by_length
}

/// The record with each secret replaced by the redaction marker.
fn redacted(record: &str, secrets_by_length: &[String]) -> String {
    secrets_by_length
        .iter()
        .filter(|secret| !secret.is_empty())
        .fold(record.to_owned(), |kept, secret| {
            kept.replace(secret.as_str(), REDACTED)
        })
}
