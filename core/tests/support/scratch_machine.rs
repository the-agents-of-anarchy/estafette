use std::cell::Cell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use estafette::dirs::{Directories, OperatingSystemFilesystem};
use estafette::keys::parse_public_key;
use iroh::EndpointId;

/// The binary under test.
pub const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
/// Where scratch roots go, short enough for an inbox socket path beneath them.
const SHORT_SCRATCH_PARENT: &str = "/tmp";
/// The start of every scratch root's name.
const SCRATCH_PREFIX: &str = "ea";
/// The base directory variable for configuration (spec 3.3).
const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
/// The base directory variable for data (spec 3.3).
const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
/// The base directory variable for runtime files (spec 3.3).
const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
/// The search path the binary finds `ps` on.
const SEARCH_PATH_VARIABLE: &str = "PATH";
/// The configuration directory under a scratch root.
const CONFIG_HOME: &str = "c";
/// The data directory under a scratch root.
const DATA_HOME: &str = "d";
/// The runtime directory under a scratch root.
const RUNTIME_DIRECTORY: &str = "r";
/// The extension of a member's run record file under a scratch root.
const RUN_RECORD_EXTENSION: &str = "stderr";
/// The start of a member's run record file name, before the member's number on its machine.
const RUN_RECORD_PREFIX: &str = "member";
/// The variable naming a directory where every member's run record is kept, redacted, when its scratch root is removed; unset, nothing is kept.
pub const KEPT_RUN_RECORDS_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_RUN_RECORDS";
/// The variable the acceptance gate exports the live host's invitation in.
pub const INVITATION_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_INVITATION";
/// What a kept run record holds in place of a secret.
pub const REDACTED: &str = "<redacted>";
/// The fewest letters and digits in a row that a key, a secret or an invitation is written with, so such a run is redacted even where the record quotes it apart from its line.
const SHORTEST_SECRET_RUN: usize = 16;
/// The directory a test's records are kept under when the harness gave its thread no name.
const UNNAMED_TEST: &str = "unnamed-test";

/// One machine's worth of estafette state under a scratch root of its own, short enough for an inbox socket path, configured for one network, counting the MCP servers it started so each keeps its own run record.
pub struct ScratchMachine {
    root: PathBuf,
    network: String,
    servers_started: Cell<u32>,
    run_records: RunRecordKeeping,
}

/// What a test asks of its members' run records, as the test reads it from the environment the acceptance gate exports: the directory they are kept in, none while unset, and the invitation they must not show.
#[derive(Clone)]
pub struct RunRecordKeeping {
    kept_in: Option<PathBuf>,
    invitation: Option<String>,
}

/// A card the binary created: the name init claims it by and the node id its key gives.
pub struct Card {
    pub name: String,
    pub id: EndpointId,
}

impl RunRecordKeeping {
    /// The running test's keeping: a directory named for the test beneath the one the variable names, and the invitation the gate exports.
    pub fn from_environment() -> Self {
        let test = std::thread::current()
            .name()
            .unwrap_or(UNNAMED_TEST)
            .replace(':', "-");
        Self {
            kept_in: std::env::var_os(KEPT_RUN_RECORDS_VARIABLE)
                .map(|directory| PathBuf::from(directory).join(test)),
            invitation: std::env::var(INVITATION_VARIABLE).ok(),
        }
    }

    /// The directory the test keeps its members' run records in; none when it keeps none.
    pub fn kept_in(&self) -> Option<&Path> {
        self.kept_in.as_deref()
    }

    /// Every non-empty line of the texts, each long run of letters and digits in one, and the invitation, longest first so a whole line is replaced before a part of it.
    pub fn secrets_longest_first<'text>(
        &self,
        texts: impl IntoIterator<Item = &'text str>,
    ) -> Vec<String> {
        let mut secrets = BTreeSet::new();
        for line in texts.into_iter().flat_map(str::lines) {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                secrets.insert(trimmed.to_owned());
            }
            secrets.extend(long_alphanumeric_runs(line).map(str::to_owned));
        }
        if let Some(invitation) = &self.invitation {
            secrets.insert(invitation.trim().to_owned());
        }
        let mut by_length: Vec<String> = secrets.into_iter().collect();
        by_length.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        by_length
    }
}

impl ScratchMachine {
    /// An empty scratch root named for the test and the machine, for the network of that name, its members' run records kept as the test asks.
    pub fn fresh(test_tag: &str, network: &str, run_records: &RunRecordKeeping) -> Self {
        let root = PathBuf::from(SHORT_SCRATCH_PARENT).join(format!(
            "{SCRATCH_PREFIX}-{}-{test_tag}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        Self {
            root,
            network: network.to_owned(),
            servers_started: Cell::new(0),
            run_records: run_records.clone(),
        }
    }

    /// The name of the network this machine is configured for.
    pub fn network(&self) -> &str {
        &self.network
    }

    /// The directories the binary derives from this machine's environment.
    pub fn directories(&self) -> Directories {
        let environment = environment(&self.root);
        Directories::establish(
            |name: &str| {
                environment
                    .iter()
                    .find(|(variable, _)| *variable == name)
                    .map(|(_, value)| value.clone())
            },
            &OperatingSystemFilesystem,
            None,
        )
        .unwrap()
    }

    /// The binary with this machine's environment alone.
    pub fn command(&self) -> Command {
        let mut command = Command::new(BINARY);
        command.env_clear().envs(environment(&self.root));
        command
    }

    /// Runs the binary to completion on this machine, requiring success, and returns what it printed.
    pub fn run(&self, arguments: &[&str]) -> String {
        let output = self.command().args(arguments).output().unwrap();
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    /// Creates a card requesting the handle by the binary's own command.
    pub fn create_card(&self, name: &str, handle: &str) -> Card {
        let printed_key = self.run(&["create-card", self.network(), name, handle]);
        Card {
            name: name.to_owned(),
            id: parse_public_key(printed_key.trim_end()).unwrap(),
        }
    }

    /// Starts the binary's MCP server for the network with its standard input and output piped, its run record written to a file of the scratch root numbered for the server.
    pub fn spawn_mcp_server(&self) -> Child {
        let started = self.servers_started.get();
        self.servers_started.set(started.checked_add(1).unwrap());
        let run_record = File::create(self.root.join(format!(
            "{RUN_RECORD_PREFIX}-{started}.{RUN_RECORD_EXTENSION}"
        )))
        .unwrap();
        self.command()
            .args(["mcp", &self.network])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(run_record)
            .spawn()
            .unwrap()
    }

    /// Copies every member's run record into the directory, named for this machine and the member, each line that holds a secret of this machine kept with that secret replaced by the redaction marker, and returns the kept files.
    pub fn keep_run_records(&self, directory: &Path) -> io::Result<Vec<PathBuf>> {
        let secrets = self.secrets_by_length();
        let machine_name = self.root.file_name().unwrap_or_default().to_string_lossy();
        let mut kept = Vec::new();
        for record in self.run_records()? {
            let member_name = record.file_stem().unwrap_or_default().to_string_lossy();
            kept.push(keep_run_record(
                directory,
                &machine_name,
                &member_name,
                &fs::read_to_string(&record)?,
                &secrets,
            )?);
        }
        Ok(kept)
    }

    /// The run record files of the members this machine started.
    pub fn run_records(&self) -> io::Result<Vec<PathBuf>> {
        let mut records: Vec<PathBuf> = fs::read_dir(&self.root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<PathBuf>>>()?
            .into_iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == RUN_RECORD_EXTENSION)
            })
            .collect();
        records.sort();
        Ok(records)
    }

    /// Every file under this machine's configuration, data and runtime directories, at any depth.
    pub fn base_directory_files(&self) -> Vec<PathBuf> {
        [CONFIG_HOME, DATA_HOME, RUNTIME_DIRECTORY]
            .into_iter()
            .flat_map(|base| files_beneath(&self.root.join(base)))
            .collect()
    }

    /// The secrets of every readable file under this machine's base directories but its debug traces, which are a record of the run like the run record itself, longest first.
    fn secrets_by_length(&self) -> Vec<String> {
        let traces = self.directories().traces_directory();
        let contents: Vec<String> = self
            .base_directory_files()
            .iter()
            .filter(|file| !file.starts_with(&traces))
            .filter_map(|file| fs::read(file).ok())
            .map(|content| String::from_utf8_lossy(&content).into_owned())
            .collect();
        self.run_records
            .secrets_longest_first(contents.iter().map(String::as_str))
    }
}

impl Drop for ScratchMachine {
    fn drop(&mut self) {
        let kept = self
            .run_records
            .kept_in()
            .map(|directory| self.keep_run_records(directory));
        drop(fs::remove_dir_all(&self.root));
        if let Some(Err(failure)) = kept {
            fail_unless_panicking(&format!(
                "run records of {} not kept: {failure}",
                self.root.display()
            ));
        }
    }
}

/// Fails the test with the message, or prints it when the test is already failing, so a cleanup failure never hides the failure that came first.
pub fn fail_unless_panicking(failure: &str) {
    if std::thread::panicking() {
        eprintln!("{failure}");
    } else {
        panic!("{failure}");
    }
}

/// Writes the member's run record into the directory, named for its machine and the member, each secret replaced by the redaction marker, and returns the kept file.
pub fn keep_run_record(
    directory: &Path,
    machine_name: &str,
    member_name: &str,
    record: &str,
    secrets_by_length: &[String],
) -> io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
    let kept_record = directory.join(format!(
        "{machine_name}-{member_name}.{RUN_RECORD_EXTENSION}"
    ));
    let (text, redacted_lines) = redacted(record, secrets_by_length);
    fs::write(&kept_record, text)?;
    eprintln!(
        "run record kept at {}: {redacted_lines} lines redacted",
        kept_record.display()
    );
    Ok(kept_record)
}

/// The record with each secret replaced by the redaction marker, and how many of its lines held one.
fn redacted(record: &str, secrets_by_length: &[String]) -> (String, usize) {
    let mut redacted_lines = 0_usize;
    let mut kept = String::with_capacity(record.len());
    for line in record.split_inclusive('\n') {
        let mut kept_line = line.to_owned();
        for secret in secrets_by_length {
            kept_line = kept_line.replace(secret.as_str(), REDACTED);
        }
        if kept_line != line {
            redacted_lines = redacted_lines.saturating_add(1);
        }
        kept.push_str(&kept_line);
    }
    (kept, redacted_lines)
}

/// The runs of letters and digits in the line long enough to be a key, a secret or an invitation.
fn long_alphanumeric_runs(line: &str) -> impl Iterator<Item = &str> {
    line.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|run| run.len() >= SHORTEST_SECRET_RUN)
}

/// Every file beneath the directory, at any depth; none when it does not exist.
fn files_beneath(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for path in entries.filter_map(Result::ok).map(|entry| entry.path()) {
        if path.is_dir() {
            files.extend(files_beneath(&path));
        } else {
            files.push(path);
        }
    }
    files
}

/// The base directories under the scratch root, and the search path the binary finds `ps` on.
fn environment(root: &Path) -> Vec<(&'static str, OsString)> {
    vec![
        (
            CONFIG_HOME_VARIABLE,
            root.join(CONFIG_HOME).into_os_string(),
        ),
        (DATA_HOME_VARIABLE, root.join(DATA_HOME).into_os_string()),
        (
            RUNTIME_DIRECTORY_VARIABLE,
            root.join(RUNTIME_DIRECTORY).into_os_string(),
        ),
        (
            SEARCH_PATH_VARIABLE,
            std::env::var_os(SEARCH_PATH_VARIABLE).unwrap_or_default(),
        ),
    ]
}
