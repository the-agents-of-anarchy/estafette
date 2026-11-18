use std::cell::Cell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use estafette::dirs::{
    Directories, DirectoriesError, DirectoryFilesystem, OperatingSystemFilesystem, TraceDirectory,
    trace_file_of,
};
use estafette::keys::parse_public_key;
use iroh::EndpointId;

#[path = "../common/fresh_directory.rs"]
pub mod fresh_directory;
#[path = "owned_process.rs"]
pub mod owned_process;
#[path = "../common/remove_scratch.rs"]
pub mod remove_scratch;
#[path = "server_member_end.rs"]
pub mod server_member_end;
#[path = "trace_reader.rs"]
pub mod trace_reader;

use owned_process::{END_FAILED_EXTENSION, OwnedProcess};
use server_member_end::succeeded;
use trace_reader::{WritingProcess, timeline_read_as};

use crate::member_events::MemberEvents;

/// The binary under test.
pub const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
/// What the print of a failing test's timelines says before each member's.
pub const TIMELINE_HEADING: &str = "timeline of member";
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
/// The variable naming the directory a member finds the machine's cards under, so a member may keep a data directory of its own (spec:534).
const CARDS_DIRECTORY_VARIABLE: &str = "ESTAFETTE_CARDS_DIR";
/// The search path the binary finds `ps` on.
const SEARCH_PATH_VARIABLE: &str = "PATH";
/// The configuration directory under a scratch root.
const CONFIG_HOME: &str = "c";
/// The data directory under a scratch root.
const DATA_HOME: &str = "d";
/// Where under the machine's data directory each member's own data directory lies, named by the member's number.
const MEMBERS_UNDER_DATA_HOME: &str = "members";
/// The runtime directory under a scratch root.
const RUNTIME_DIRECTORY: &str = "r";
/// The base directories under a scratch root: configuration, data and runtime.
const BASE_DIRECTORIES: [&str; 3] = [CONFIG_HOME, DATA_HOME, RUNTIME_DIRECTORY];
/// The extension of a member's run record file under a scratch root.
const RUN_RECORD_EXTENSION: &str = "stderr";
/// The extension of the file kept beside a member's run record when the member's stop failed, so a reader of what is kept knows the member may have run on while its records were taken.
const STOP_FAILED_EXTENSION: &str = "stop-failed";
/// The start of a member's run record file name, before the member's number on its machine.
const RUN_RECORD_PREFIX: &str = "member";
/// How a test names a member it started, before its number.
const MEMBER_LABEL: &str = "member";
/// The variable naming a directory where, when its scratch root is removed, every member's run record is kept, redacted, and every machine's data directory but its secret key files; unset, nothing is kept.
pub const KEPT_RUN_RECORDS_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_RUN_RECORDS";
/// The directory a machine's data directory is kept in, inside the directory its records are kept in.
const KEPT_DATA_DIRECTORY: &str = "data";
/// What joins a kept name to the number of a later keeping of that name, a form unlike the hyphen and number a scratch root's name takes when its own name is taken.
const LATER_KEEPING_SEPARATOR: &str = ".kept-";
/// The number the second keeping of a name takes, the first taking the name alone.
const SECOND_KEEPING: u32 = 2;
/// The extension of every file spec 3.3 gives a secret key in a data directory: a card's sidecar and a host's bootstrap key.
const SECRET_KEY_EXTENSION: &str = "key";
/// The extension of the note kept beside a data file that was kept with each invitation and token in it replaced by the redaction marker.
const REDACTED_NOTE_EXTENSION: &str = "redacted";
/// The extension spec 3.3 gives a relay's certificate cache directory, which holds its private key.
const CERTIFICATE_CACHE_EXTENSION: &str = "certs";
/// The field of a network configuration holding the network's secret (spec 3.3).
const NETWORK_SECRET_FIELD: &str = "secret";
/// The extension spec 3.3 gives a network configuration.
const NETWORK_CONFIG_EXTENSION: &str = "toml";
/// What a failure says of a network configuration that does not parse as TOML, in place of the parser's text, which quotes the line it stopped at.
const CONFIGURATION_NOT_TOML: &str = "does not parse as TOML";
/// The variable the acceptance gate exports the live host's invitation in.
pub const INVITATION_VARIABLE: &str = "ESTAFETTE_ACCEPTANCE_INVITATION";
/// What a kept run record holds in place of a secret.
pub const REDACTED: &str = "<redacted>";
/// The fewest letters and digits in a row that a key, a secret or an invitation is written with, so such a run is redacted even where the record quotes it apart from its line.
const SHORTEST_SECRET_RUN: usize = 16;
/// What a failing test prints in place of a member's timeline when no secrets stand to search it for, before why.
const TIMELINE_NOT_PRINTED: &str =
    "not printed, since the secrets to search it for could not be read";
/// What a machine's drop reports of each process it owned whose kill or wait failed, before the mark that says so.
const FAILED_END_REPORTED: &str = "the end of a process failed, as";
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

/// Every text a machine's or a server member's kept files are searched for, read from every source of its secrets before anything of it is kept, so no file is kept unsearched.
pub struct KeptSecrets {
    secrets_longest_first: Vec<String>,
    secrets_and_lines_of_other_files_longest_first: Vec<String>,
}

/// What a kept file is searched for beside the secrets of its machine or member.
#[derive(Clone, Copy)]
enum KeptAs {
    /// A run record, searched also for every line of its machine's or member's other readable files.
    RunRecord,
    /// A mark, a data file or a fetched trace file, searched for the secrets alone.
    Evidence,
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

    /// The invitation the gate exports, which nothing kept may show; none when the variable is unset.
    pub fn invitation(&self) -> Option<&str> {
        self.invitation.as_deref()
    }
}

impl KeptSecrets {
    /// The secrets of the machine, read before anything of it is kept: the invitation when set, the `secret` of every network configuration under its configuration directory, and the text of every secret key file and every file of a certificate cache under its configuration, data and runtime directories, a run record also searched for every line of the other files it can read there but debug traces; or the first listing, read or configuration that fails, naming the path.
    fn of_machine(machine: &ScratchMachine) -> io::Result<Self> {
        let mut secret_texts = Vec::new();
        let mut other_texts = Vec::new();
        for file in machine.base_directory_files()? {
            let in_root = file.strip_prefix(&machine.root).unwrap_or(&file);
            if holds_a_secret_key(in_root) {
                secret_texts.push(text_of(&file)?);
            } else if is_a_network_configuration(in_root) {
                let configuration = text_of(&file)?;
                secret_texts.push(
                    network_secret_named_in(&configuration)
                        .map_err(|failure| naming(&file, &failure))?,
                );
                other_texts.push(configuration);
            } else if !is_a_debug_trace(&file) {
                other_texts.extend(text_of(&file).ok());
            }
        }
        Ok(Self::searching_for(
            &secret_texts,
            &other_texts,
            machine.run_records.invitation(),
        ))
    }

    /// The secrets of a member serving on a server, read where it starts: the `secret` of the network configuration it wrote there and the invitation, a run record also searched for every line of that configuration; or why either is missing.
    pub fn of_server_member(configuration: &str, invitation: Option<&str>) -> io::Result<Self> {
        let secret = network_secret_named_in(configuration).map_err(|failure| {
            io::Error::new(
                failure.kind(),
                format!("the network configuration written to the member's server: {failure}"),
            )
        })?;
        let invitation = invitation.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{INVITATION_VARIABLE} is unset, so nothing can be searched for the invitation"
                ),
            )
        })?;
        Ok(Self::searching_for(
            &[secret],
            &[configuration.to_owned()],
            Some(invitation),
        ))
    }

    /// The secrets of the secret texts and the invitation, and for a run record also of the other texts.
    fn searching_for(
        secret_texts: &[String],
        other_texts: &[String],
        invitation: Option<&str>,
    ) -> Self {
        Self {
            secrets_longest_first: longest_first(
                secret_texts.iter().map(String::as_str),
                invitation,
            ),
            secrets_and_lines_of_other_files_longest_first: longest_first(
                secret_texts.iter().chain(other_texts).map(String::as_str),
                invitation,
            ),
        }
    }

    /// The text with each secret, and for a run record each line of the other files, replaced by the redaction marker, and how many of its lines held one.
    fn redacted(&self, text: &str, kept_as: KeptAs) -> (String, usize) {
        match kept_as {
            KeptAs::RunRecord => {
                redacted(text, &self.secrets_and_lines_of_other_files_longest_first)
            }
            KeptAs::Evidence => redacted(text, &self.secrets_longest_first),
        }
    }
}

/// Every non-empty line of the texts, each long run of letters and digits in one, and the invitation, longest first so a whole line is replaced before a part of it.
fn longest_first<'text>(
    texts: impl IntoIterator<Item = &'text str>,
    invitation: Option<&str>,
) -> Vec<String> {
    let mut secrets = BTreeSet::new();
    for line in texts.into_iter().flat_map(str::lines) {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            secrets.insert(trimmed.to_owned());
        }
        secrets.extend(long_alphanumeric_runs(line).map(str::to_owned));
    }
    secrets.extend(
        invitation
            .map(str::trim)
            .filter(|invitation| !invitation.is_empty())
            .map(str::to_owned),
    );
    let mut by_length: Vec<String> = secrets.into_iter().collect();
    by_length.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    by_length
}

/// The secret a network configuration's text names in its `secret` string, whether or not the rest of it loads, or the kind of failure that kept it from naming one, never a line of it.
fn network_secret_named_in(configuration: &str) -> io::Result<String> {
    let table: toml::Table = configuration
        .parse()
        .map_err(|_unparsed: toml::de::Error| {
            io::Error::new(io::ErrorKind::InvalidData, CONFIGURATION_NOT_TOML)
        })?;
    table
        .get(NETWORK_SECRET_FIELD)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other(format!("names no {NETWORK_SECRET_FIELD} string")))
}

/// The text of the file, any bytes that are not UTF-8 replaced, or the failure to read it, naming it.
fn text_of(file: &Path) -> io::Result<String> {
    fs::read(file)
        .map(|content| String::from_utf8_lossy(&content).into_owned())
        .map_err(|failure| naming(file, &failure))
}

/// Whether the file, named from its scratch root, is a network configuration by spec 3.3's naming: a TOML file under the configuration directory.
fn is_a_network_configuration(in_root: &Path) -> bool {
    in_root.starts_with(CONFIG_HOME)
        && in_root
            .extension()
            .is_some_and(|extension| extension == NETWORK_CONFIG_EXTENSION)
}

/// Whether the file is a debug trace, a record of the run like the run record itself.
fn is_a_debug_trace(file: &Path) -> bool {
    file.file_name()
        .and_then(|name| name.to_str())
        .and_then(trace_file_of)
        .is_some()
}

impl ScratchMachine {
    /// An empty scratch root named for the test and the machine, for the network of that name, its members' run records kept as the test asks.
    pub fn fresh(test_tag: &str, network: &str, run_records: &RunRecordKeeping) -> Self {
        let root = fresh_directory::fresh_directory_in(
            Path::new(SHORT_SCRATCH_PARENT),
            &format!("{SCRATCH_PREFIX}-{}-{test_tag}", std::process::id()),
        )
        .unwrap();
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
        self.established_directories().unwrap()
    }

    /// The directories the binary derives from this machine's environment, or why they could not be established, never panicking.
    fn established_directories(&self) -> Result<Directories, DirectoriesError> {
        let environment = environment(&self.root);
        Directories::establish(
            |name: &str| variable_of(&environment, name),
            &OperatingSystemFilesystem,
            None,
        )
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

    /// How many MCP servers this machine has started, which is also the number of the next one.
    pub fn members_started(&self) -> u32 {
        self.servers_started.get()
    }

    /// The timeline of each member this machine started, headed by the member's number and this machine's root, searched for the secrets read as it is made, for a failing test to print.
    fn member_timelines(&self) -> Vec<String> {
        let secrets = KeptSecrets::of_machine(self);
        (0..self.members_started())
            .map(|number| {
                let timeline = timeline_read_as(
                    &self.traces_directory_of_member(number),
                    WritingProcess::Running,
                );
                format!(
                    "{TIMELINE_HEADING} {number} on {}:\n{}",
                    self.root.display(),
                    timeline_to_print(&timeline, secrets.as_ref())
                )
            })
            .collect()
    }

    /// The trace directory of the member of that number, which no other member writes in, derived from its data directory alone so nothing is established on the way.
    pub fn traces_directory_of_member(&self, number: u32) -> PathBuf {
        let environment = [(
            DATA_HOME_VARIABLE,
            self.member_data_home(number).into_os_string(),
        )];
        TraceDirectory::resolve(&|name: &str| variable_of(&environment, name))
            .unwrap()
            .path()
    }

    /// Starts the binary's MCP server for this machine's network as the next member, with the reader of the events it records in its own trace directory, made before the start so nothing can fail between the start and the return.
    pub fn spawn_recorded_mcp_server(&self) -> (OwnedProcess, MemberEvents) {
        let number = self.members_started();
        let events = MemberEvents::new(
            &format!("{MEMBER_LABEL} {number}"),
            &self.traces_directory_of_member(number),
        );
        (self.spawn_mcp_server(), events)
    }

    /// The binary with the environment of the member of that number: the machine's, but a data directory of its own and the machine's cards.
    pub fn member_command(&self, number: u32) -> Command {
        let mut command = Command::new(BINARY);
        command.env_clear().envs(self.member_environment(number));
        command
    }

    /// Starts the binary's MCP server for the network with its standard input and output piped, its run record written to a file of the scratch root numbered for the server, its directories made as the binary makes them, private, and then a trace directory of its own made ready, owned from its spawn.
    pub fn spawn_mcp_server(&self) -> OwnedProcess {
        let started = self.members_started();
        self.servers_started.set(started.checked_add(1).unwrap());
        let environment = self.member_environment(started);
        Directories::establish(
            |name: &str| variable_of(&environment, name),
            &OperatingSystemFilesystem,
            None,
        )
        .unwrap();
        OperatingSystemFilesystem
            .create_private_directory(&self.traces_directory_of_member(started))
            .unwrap();
        let run_record = File::create(self.root.join(format!(
            "{RUN_RECORD_PREFIX}-{started}.{RUN_RECORD_EXTENSION}"
        )))
        .unwrap();
        self.spawn_owned(
            self.member_command(started)
                .args(["mcp", &self.network])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(run_record),
            &format!("{RUN_RECORD_PREFIX}-{started}"),
        )
    }

    /// Spawns the command as a process of this machine, owned from its spawn, a failed end of it marked under the label in the scratch root and kept beside the run records.
    #[track_caller]
    pub fn spawn_owned(&self, command: &mut Command, label: &str) -> OwnedProcess {
        OwnedProcess::spawned(command, &self.root, label)
    }

    /// Starts the command as a process of this machine owned from its spawn, its standard output read line by line with the instant each line arrived, and requires its first line within the wait, leaving that line to be read.
    #[track_caller]
    pub fn start_printing_lines(
        &self,
        command: &mut Command,
        label: &str,
        first_line_within: Duration,
    ) -> (OwnedProcess, Receiver<(Instant, String)>) {
        let mut process = self.spawn_owned(command.stdout(Stdio::piped()), label);
        let (forward, lines) = mpsc::channel();
        let (announce_first_line, first_line) = mpsc::sync_channel(1);
        let output = BufReader::new(process.stdout.take().unwrap());
        std::thread::spawn(move || {
            let mut announce_first_line = Some(announce_first_line);
            for line in output.lines().map_while(Result::ok) {
                if let Some(first) = announce_first_line.take()
                    && first.send(()).is_err()
                {
                    return;
                }
                if forward.send((Instant::now(), line)).is_err() {
                    return;
                }
            }
        });
        assert!(
            first_line.recv_timeout(first_line_within).is_ok(),
            "{label} printed no line within {first_line_within:?}"
        );
        (process, lines)
    }

    /// Keeps this machine's run records, the marks of its processes whose end failed, and its data directory together in a directory of its own beneath the test's, named for this machine and numbered after any earlier keeping of that name, searched for the secrets read as this keeping starts, and answers every failure of that keeping, none when all was kept; keeps nothing, not even that directory, when those secrets cannot be read.
    fn keep_records(&self, directory: &Path) -> Vec<io::Error> {
        let secrets = match KeptSecrets::of_machine(self) {
            Ok(secrets) => secrets,
            Err(unread) => return vec![nothing_kept(&unread)],
        };
        let kept = match fresh_kept_directory(directory, &self.name()) {
            Ok(kept) => kept,
            Err(failure) => return vec![failure],
        };
        let run_records = self.keep_run_records_searched_for(&kept, &secrets).err();
        let end_failures = self.keep_end_failures(&kept, &secrets).err();
        let mut failures: Vec<io::Error> =
            [run_records, end_failures].into_iter().flatten().collect();
        failures.extend(self.keep_data_directory(&kept, &secrets));
        failures
    }

    /// A report of each process of this machine whose kill or wait failed, naming the mark in its scratch root that says so, or the failure to list that root for marks.
    fn failed_ends(&self) -> Vec<String> {
        match self.root_files_with_extension(END_FAILED_EXTENSION) {
            Ok(marks) => marks
                .iter()
                .map(|mark| format!("{FAILED_END_REPORTED} {} says", mark.display()))
                .collect(),
            Err(unlisted) => vec![unlisted.to_string()],
        }
    }

    /// Copies every member's run record into the directory, named for the member, each line that holds a secret of this machine, read as this keeping starts, kept with that secret replaced by the redaction marker, and returns the kept files; keeps none when those secrets cannot be read.
    pub fn keep_run_records(&self, directory: &Path) -> io::Result<Vec<PathBuf>> {
        self.keep_run_records_searched_for(directory, &KeptSecrets::of_machine(self)?)
    }

    /// Copies every member's run record into the directory, named for the member, through the writer of kept files searching it for the secrets, and returns the kept files.
    fn keep_run_records_searched_for(
        &self,
        directory: &Path,
        secrets: &KeptSecrets,
    ) -> io::Result<Vec<PathBuf>> {
        let mut kept = Vec::new();
        for record in self.run_records()? {
            let member_name = record.file_stem().unwrap_or_default().to_string_lossy();
            kept.push(keep_run_record(
                directory,
                &member_name,
                &fs::read(&record).map_err(|failure| naming(&record, &failure))?,
                secrets,
            )?);
        }
        Ok(kept)
    }

    /// Copies the mark of every process of this machine whose end failed into the directory through the writer of kept files, and returns the kept marks.
    fn keep_end_failures(
        &self,
        directory: &Path,
        secrets: &KeptSecrets,
    ) -> io::Result<Vec<PathBuf>> {
        let mut kept = Vec::new();
        for mark in self.root_files_with_extension(END_FAILED_EXTENSION)? {
            let kept_mark = directory.join(mark.file_name().unwrap_or_default());
            let text = fs::read(&mark).map_err(|failure| naming(&mark, &failure))?;
            keep_file(secrets, KeptAs::Evidence, &kept_mark, &text)?;
            print_to_standard_error(&format!("failed end kept at {}", kept_mark.display()));
            kept.push(kept_mark);
        }
        Ok(kept)
    }

    /// Copies every file under this machine's data directory, its members' and its observers' trace files included, into the data directory inside the directory, searched for the secrets, leaving out each file that holds a secret key, going on past each file it cannot copy, and answers each such failure.
    fn keep_data_directory(&self, directory: &Path, secrets: &KeptSecrets) -> Vec<io::Error> {
        let data = self.root.join(DATA_HOME);
        let kept = directory.join(KEPT_DATA_DIRECTORY);
        let mut failures = Vec::new();
        for listed in files_beneath_for_keeping(&data) {
            let file = match listed {
                Ok(file) => file,
                Err(failure) => {
                    failures.push(failure);
                    continue;
                }
            };
            let Ok(in_data) = file.strip_prefix(&data) else {
                continue;
            };
            if holds_a_secret_key(in_data) {
                continue;
            }
            let copied = fs::read(&file)
                .map_err(|failure| naming(&file, &failure))
                .and_then(|content| {
                    keep_file(secrets, KeptAs::Evidence, &kept.join(in_data), &content)
                });
            if let Err(failure) = copied {
                failures.push(failure);
            }
        }
        print_to_standard_error(&format!("data directory kept at {}", kept.display()));
        failures
    }

    /// The name this machine's records are kept under: its scratch root's.
    fn name(&self) -> String {
        self.root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    /// The run record files of the members this machine started.
    pub fn run_records(&self) -> io::Result<Vec<PathBuf>> {
        self.root_files_with_extension(RUN_RECORD_EXTENSION)
    }

    /// The files directly in this machine's scratch root with the extension, in order of name, or the failure to list it, naming it.
    fn root_files_with_extension(&self, wanted: &str) -> io::Result<Vec<PathBuf>> {
        let listed: io::Result<Vec<PathBuf>> = fs::read_dir(&self.root).and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect()
        });
        let mut files: Vec<PathBuf> = listed
            .map_err(|failure| naming(&self.root, &failure))?
            .into_iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == wanted)
            })
            .collect();
        files.sort();
        Ok(files)
    }

    /// Every file under this machine's configuration, data and runtime directories, at any depth, or the first failure to list a directory beneath them, naming it; nothing of a directory that does not exist.
    pub fn base_directory_files(&self) -> io::Result<Vec<PathBuf>> {
        BASE_DIRECTORIES
            .into_iter()
            .flat_map(|base| files_beneath_for_keeping(&self.root.join(base)))
            .collect()
    }

    /// The environment of the member of that number: the machine's, but with a data directory of its own under the machine's data directory, and the machine's cards.
    fn member_environment(&self, number: u32) -> Vec<(&'static str, OsString)> {
        let cards = self.directories().cards_directory(&self.network).unwrap();
        let mut variables: Vec<(&'static str, OsString)> = environment(&self.root)
            .into_iter()
            .filter(|(name, _)| *name != DATA_HOME_VARIABLE)
            .collect();
        variables.push((
            DATA_HOME_VARIABLE,
            self.member_data_home(number).into_os_string(),
        ));
        variables.push((
            CARDS_DIRECTORY_VARIABLE,
            cards.parent().unwrap().as_os_str().to_owned(),
        ));
        variables
    }

    /// The data directory of the member of that number, under the machine's data directory.
    fn member_data_home(&self, number: u32) -> PathBuf {
        self.root
            .join(DATA_HOME)
            .join(MEMBERS_UNDER_DATA_HOME)
            .join(number.to_string())
    }
}

impl Drop for ScratchMachine {
    fn drop(&mut self) {
        if std::thread::panicking() {
            for timeline in self.member_timelines() {
                print_to_standard_error(&timeline);
            }
        }
        let failed_ends = self.failed_ends();
        let not_kept = self
            .run_records
            .kept_in()
            .map(|directory| self.keep_records(directory))
            .unwrap_or_default();
        let removed = remove_scratch::remove_scratch(&self.root);
        let mut failures = Vec::new();
        if !not_kept.is_empty() {
            failures.push(format!(
                "records of {} not kept: {}",
                self.root.display(),
                each_failure(&not_kept)
            ));
        }
        if let Err(failure) = removed {
            failures.push(failure.to_string());
        }
        failures.extend(failed_ends);
        if !failures.is_empty() {
            fail_unless_panicking(&failures.join("; "));
        }
    }
}

/// The failure of a keeping whose secrets to search what it keeps for could not be read, so nothing is kept.
fn nothing_kept(unread: &io::Error) -> io::Error {
    io::Error::new(
        unread.kind(),
        format!("nothing kept, since the secrets to search it for could not be read: {unread}"),
    )
}

/// The timeline with each secret replaced by the redaction marker, or, when no secrets stand to search it for, that it is not printed and why.
fn timeline_to_print(timeline: &str, secrets: Result<&KeptSecrets, &io::Error>) -> String {
    match secrets {
        Ok(secrets) => secrets.redacted(timeline, KeptAs::Evidence).0,
        Err(unread) => format!("{TIMELINE_NOT_PRINTED}: {unread}"),
    }
}

/// Every failure's text, joined so a reader meets each.
fn each_failure(failures: &[io::Error]) -> String {
    failures
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Fails the test with the message, or prints it when the test is already failing, so a cleanup failure never hides the failure that came first.
pub fn fail_unless_panicking(failure: &str) {
    if std::thread::panicking() {
        print_to_standard_error(failure);
    } else {
        panic!("{failure}");
    }
}

/// Writes the text and a line end straight to standard error, ignoring a write that fails, so a print a drop makes never panics, even while its thread unwinds.
pub fn print_to_standard_error(text: &str) {
    drop(writeln!(io::stderr(), "{text}"));
}

/// Creates the directory a machine's or a member's records are kept in beneath the test's, named for it, or for a later keeping of that name its number joined in a form no scratch root's name takes, so no keeping overwrites or merges into another and no number reads as a scratch root's own.
fn fresh_kept_directory(directory: &Path, name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(directory).map_err(|failure| naming(directory, &failure))?;
    let later_keepings =
        (SECOND_KEEPING..).map(|number| format!("{name}{LATER_KEEPING_SEPARATOR}{number}"));
    for candidate in std::iter::once(name.to_owned()).chain(later_keepings) {
        let kept = directory.join(candidate);
        match fs::create_dir(&kept) {
            Ok(()) => return Ok(kept),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{}: {error}", kept.display()),
                ));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "{}: every later keeping of {name} is taken",
            directory.display()
        ),
    ))
}

/// Writes the member's run record into the directory, named for the member, through the writer of kept files, and returns the kept file.
fn keep_run_record(
    directory: &Path,
    member_name: &str,
    record: &[u8],
    secrets: &KeptSecrets,
) -> io::Result<PathBuf> {
    let kept_record = directory.join(format!("{member_name}.{RUN_RECORD_EXTENSION}"));
    let redacted_lines = keep_file(secrets, KeptAs::RunRecord, &kept_record, record)?;
    print_to_standard_error(&format!(
        "run record kept at {}: {redacted_lines} lines redacted",
        kept_record.display()
    ));
    Ok(kept_record)
}

/// Writes beside the member's run record in the directory, when the member's stop failed or could not run, a file named for the member saying so, with how the stop ended and what it printed, each secret replaced by the redaction marker; nothing when the stop succeeded.
fn keep_stop_failure(
    directory: &Path,
    member_name: &str,
    stop: &io::Result<Output>,
    secrets: &KeptSecrets,
) -> io::Result<()> {
    let Err(failure) = succeeded(stop) else {
        return Ok(());
    };
    let mark = format!(
        "the stop of {member_name} failed, so it may have run on while its records were taken; the stop ended with {}",
        failure.marked()
    );
    keep_mark(
        directory,
        &format!("{member_name}.{STOP_FAILED_EXTENSION}"),
        &mark,
        secrets,
    )
}

/// Writes into the directory a mark file of that name holding the text, through the writer of kept files.
fn keep_mark(
    directory: &Path,
    mark_name: &str,
    text: &str,
    secrets: &KeptSecrets,
) -> io::Result<()> {
    keep_file(
        secrets,
        KeptAs::Evidence,
        &directory.join(mark_name),
        text.as_bytes(),
    )
    .map(drop)
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

/// Whether the file holds a secret key by spec 3.3's naming, a key file itself or any file of a relay's certificate cache, so no copy of it is kept.
fn holds_a_secret_key(file: &Path) -> bool {
    is_a_secret_key_file(file)
        || file.ancestors().skip(1).any(|directory| {
            directory
                .extension()
                .is_some_and(|extension| extension == CERTIFICATE_CACHE_EXTENSION)
        })
}

/// Whether the file is a secret key file by spec 3.3's naming: a card's sidecar, a host's bootstrap key or a network owner's key.
fn is_a_secret_key_file(file: &Path) -> bool {
    file.extension()
        .is_some_and(|extension| extension == SECRET_KEY_EXTENSION)
}

/// Writes one file into a kept directory, making the directories it lies in: byte for byte when it holds no text the secrets name for a file kept as that, else with each such text replaced by the redaction marker and a note beside it naming the file; answers how many of its lines were redacted, or the failure, naming the file or directory it could not write.
fn keep_file(
    secrets: &KeptSecrets,
    kept_as: KeptAs,
    kept_file: &Path,
    content: &[u8],
) -> io::Result<usize> {
    if let Some(directory) = kept_file.parent() {
        fs::create_dir_all(directory).map_err(|failure| naming(directory, &failure))?;
    }
    let (kept_text, redacted_lines) = secrets.redacted(&String::from_utf8_lossy(content), kept_as);
    if redacted_lines == 0 {
        fs::write(kept_file, content).map_err(|failure| naming(kept_file, &failure))?;
        return Ok(redacted_lines);
    }
    fs::write(kept_file, kept_text).map_err(|failure| naming(kept_file, &failure))?;
    let mut note = kept_file.as_os_str().to_owned();
    note.push(format!(".{REDACTED_NOTE_EXTENSION}"));
    fs::write(
        &note,
        format!(
            "{} was kept with each invitation and token in it replaced by {REDACTED}: {redacted_lines} lines redacted\n",
            kept_file.file_name().unwrap_or_default().to_string_lossy()
        ),
    )
    .map_err(|failure| naming(Path::new(&note), &failure))?;
    Ok(redacted_lines)
}

/// Every file beneath the directory at any depth, or the failure to read a directory or its entry, naming that directory; nothing when the directory does not exist.
fn files_beneath_for_keeping(directory: &Path) -> Vec<io::Result<PathBuf>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(failure) if failure.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(failure) => return vec![Err(naming(directory, &failure))],
    };
    let mut files = Vec::new();
    for entry in entries {
        match entry.map(|entry| entry.path()) {
            Ok(path) if path.is_dir() => files.extend(files_beneath_for_keeping(&path)),
            Ok(path) => files.push(Ok(path)),
            Err(failure) => files.push(Err(naming(directory, &failure))),
        }
    }
    files
}

/// The failure of the same kind with the path it concerns written before its text.
fn naming(path: &Path, failure: &io::Error) -> io::Error {
    io::Error::new(failure.kind(), format!("{}: {failure}", path.display()))
}

/// The value the environment gives the variable, none when it names none.
fn variable_of(environment: &[(&str, OsString)], name: &str) -> Option<OsString> {
    environment
        .iter()
        .find(|(variable, _)| *variable == name)
        .map(|(_, value)| value.clone())
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

#[cfg(test)]
mod tests {
    use super::trace_reader::parse_trace_lines;
    use super::trace_reader::tests::{
        MEMBER_LINE, second_line_still_zero_at_its_start_sized_ahead,
    };
    use estafette::config::NetworkConfig;
    use std::cell::RefCell;
    use std::os::unix::fs::PermissionsExt;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::owned_process::{FIXTURE_ENDED_WITHIN, FIXTURE_VARIABLE, fixture_exit_within};
    use super::*;

    /// The network the tests of this module configure their machines for.
    const NETWORK: &str = "lab";
    /// A command that does nothing and exits in failure, standing in for a stop that failed.
    const EXITS_IN_FAILURE: &str = "false";
    /// A command that does nothing and exits in success, standing in for a stop that succeeded.
    const EXITS_IN_SUCCESS: &str = "true";
    /// How a test names a process that ended cleanly.
    const ENDED_LABEL: &str = "ended";
    /// How a test names a process whose end it marks as failed.
    const FAILED_LABEL: &str = "failed";
    /// The failure a test plants as the cause of a failed end.
    const PLANTED_END_FAILURE: &str = "planted failure of the wait";
    /// The mode of a file or directory its owner can neither read nor list, so keeping it fails.
    const UNREADABLE: u32 = 0o000;
    /// The mode that makes such a file or directory its owner's again, so the test can remove it.
    pub(super) const READABLE: u32 = 0o700;
    /// An invitation a test plants in what a machine keeps, no real one.
    const PLANTED_INVITATION: &str = "estafette-invite-1:plantedinvitation0123456789";
    /// A token a test plants as a card's secret key and in what a machine keeps, no real one.
    const PLANTED_TOKEN: &str = "plantedtoken0123456789abcdef";
    /// A network secret a test plants in a machine's network configuration and in what it keeps, no real one.
    const PLANTED_NETWORK_SECRET: &str =
        "5ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2e75ec2";
    /// The owner key a test's planted network configuration names, no real one.
    const PLANTED_OWNER: &str = "0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e";
    /// The shell a test runs a stand-in's command line with, and the flag that hands it the line.
    const SHELL: &str = "sh";
    const SHELL_COMMAND_FLAG: &str = "-c";
    /// A stand-in that closes its standard output at its start and runs until it is ended, printing nothing.
    const CLOSES_ITS_OUTPUT_AND_RUNS_UNTIL_ENDED: &str = "exec tail -f /dev/null >&-";
    /// How a test names its stand-in.
    const STAND_IN_LABEL: &str = "stand-in";
    /// The bound a test gives a first-line wait that ends at once, its output closed, so the bound is never waited out.
    const FIRST_LINE_WITHIN: Duration = Duration::from_secs(60);
    /// The failure a test raises before a machine it holds is dropped.
    const FIRST_FAILURE: &str = "the test failed first";
    /// Where a test moves a machine's configuration base directory so a file can take its place.
    const CONFIGURATION_HOME_SET_ASIDE: &str = "c-set-aside";
    /// The card whose secret key file a test plants.
    const CARD: &str = "amy";
    /// The events file a test plants in a member's trace directory.
    const MEMBER_EVENTS_FILE: &str = "20261117T080000Z-1111111111111111-estafette.events.jsonl";
    /// The events file a test plants in a second member's trace directory.
    const SECOND_MEMBER_EVENTS_FILE: &str =
        "20261117T080001Z-2222222222222222-estafette.events.jsonl";
    /// What a test writes where a network configuration belongs, which does not load.
    const NOT_A_NETWORK_CONFIGURATION: &str = "not a network configuration\n";
    /// A network other than the one a test's machine is configured for, whose configuration lies beside its own.
    const OTHER_NETWORK: &str = "other";
    /// A network secret a test plants in the other network's configuration, no real one.
    const PLANTED_OTHER_NETWORK_SECRET: &str =
        "a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4";
    /// What a test writes as a network configuration that loads as TOML and names no secret.
    const CONFIGURATION_NAMING_NO_SECRET: &str = "owner = \"0e0e0e0e\"\n";
    /// The text a test plants in a network owner's key file, no real key.
    const PLANTED_OWNER_KEY: &str = "plantedownerkey0123456789abcdef";
    /// The text a test plants in a key file of the runtime directory, no real key.
    const PLANTED_RUNTIME_KEY: &str = "plantedruntimekey0123456789abc";
    /// A key file a test plants in a machine's runtime directory.
    const RUNTIME_KEY_FILE: &str = "planted.key";
    /// The text a test plants in a file of a relay's certificate cache, no real key.
    const PLANTED_CACHED_KEY: &str = "plantedcachedkey0123456789abcd";
    /// A file a test plants in a relay's certificate cache.
    const CACHED_ACCOUNT_FILE: &str = "acme-account.json";
    /// A marker a test plants in the `secret` line of a network configuration that does not parse, no real secret.
    const PLANTED_MALFORMED_SECRET: &str = "plantedmalformedsecret0123456789";
    /// A file a test makes where a directory is wanted, so no directory can be made beneath it.
    pub(super) const NOT_A_DIRECTORY: &str = "not-a-directory";
    /// The directory a test asks records be kept in, beneath that file.
    pub(super) const KEPT_BENEATH_NOT_A_DIRECTORY: &str = "kept";
    /// The length of a name a test gives a kept file, short enough to be kept and too long for the note beside it, the longest name a file system here takes being 255 bytes.
    const NAME_TOO_LONG_FOR_ITS_NOTE: usize = 240;
    /// The mode of a directory its owner may enter and write in but not list.
    const UNLISTABLE: u32 = 0o300;
    /// The mode of a directory its owner may list and enter but not write in.
    pub(super) const UNWRITABLE: u32 = 0o500;
    /// The file a test sends a fixture's standard error to.
    const FIXTURE_STANDARD_ERROR: &str = "fixture.stderr";
    /// The fixture whose machine prints while its thread unwinds, by the name the test harness runs it by.
    const PRINTING_WHILE_UNWINDING_FIXTURE: &str =
        "scratch_machine::tests::forced_failure_with_a_machine_printing_while_it_unwinds";
    /// The exit code of a test binary whose test failed, as against a signal that ends one that aborts.
    const FAILED_TEST_EXIT_CODE: i32 = 101;
    /// The mode spec 3.3 gives every directory the binary makes private.
    const SPEC_PRIVATE_DIRECTORY_MODE: u32 = 0o700;
    /// The bits of a mode that grant reading, writing and searching.
    const MODE_BITS: u32 = 0o777;

    #[test]
    fn observability_decision_12_a_failing_machine_prints_the_lines_of_a_member_that_may_still_run_before_one_still_zero_filled()
     {
        let machine = ScratchMachine::fresh(
            "still-zero",
            NETWORK,
            &RunRecordKeeping {
                kept_in: None,
                invitation: None,
            },
        );
        machine.servers_started.set(1);
        let traces = machine.traces_directory_of_member(0);
        OperatingSystemFilesystem
            .create_private_directory(&traces)
            .unwrap();
        fs::write(
            traces.join("20261112T225914Z-6c935f82c118f7b3-estafette.events.jsonl"),
            second_line_still_zero_at_its_start_sized_ahead(),
        )
        .unwrap();
        let expected_row = parse_trace_lines(MEMBER_LINE.as_bytes(), WritingProcess::Ended)
            .unwrap()[0]
            .to_string();

        let timelines = machine.member_timelines();

        assert_eq!(
            timelines,
            [format!(
                "{TIMELINE_HEADING} 0 on {}:\n{expected_row}",
                machine.root.display()
            )]
        );
    }

    #[test]
    fn observability_decision_15_a_failing_machine_prints_its_members_timeline_with_each_secret_of_the_machine_redacted()
     {
        let machine = machine_with_a_member_naming_the_network_secret("timeline-redacted");

        let timelines = machine.member_timelines();

        let expected_row = parse_trace_lines(
            member_line_naming(REDACTED).as_bytes(),
            WritingProcess::Ended,
        )
        .unwrap()[0]
            .to_string();
        assert_eq!(
            timelines,
            [format!(
                "{TIMELINE_HEADING} 0 on {}:\n{expected_row}",
                machine.root.display()
            )]
        );
    }

    #[test]
    fn observability_decision_15_a_failing_machine_whose_secrets_cannot_be_read_prints_no_timeline_and_says_so()
     {
        let machine = machine_with_a_member_naming_the_network_secret("timeline-unprinted");
        let configuration = machine.directories().network_config_file(NETWORK).unwrap();
        fs::set_permissions(&configuration, fs::Permissions::from_mode(UNREADABLE)).unwrap();

        let timelines = machine.member_timelines();

        fs::set_permissions(&configuration, fs::Permissions::from_mode(READABLE)).unwrap();
        let [timeline] = &timelines[..] else {
            panic!("one timeline for the one member: {timelines:?}")
        };
        assert!(
            timeline.starts_with(&format!(
                "{TIMELINE_HEADING} 0 on {}:",
                machine.root.display()
            )) && timeline.contains(TIMELINE_NOT_PRINTED)
                && timeline.contains(&configuration.display().to_string())
                && !timeline.contains(PLANTED_NETWORK_SECRET),
            "{timeline}"
        );
    }

    /// A machine keeping nothing, configured with the planted network secret, whose one member's events file holds one line naming that secret.
    fn machine_with_a_member_naming_the_network_secret(tag: &str) -> ScratchMachine {
        let machine = ScratchMachine::fresh(
            tag,
            NETWORK,
            &RunRecordKeeping {
                kept_in: None,
                invitation: None,
            },
        );
        planted_network_configuration(&machine, NETWORK);
        machine.servers_started.set(1);
        let traces = machine.traces_directory_of_member(0);
        fs::create_dir_all(&traces).unwrap();
        fs::write(
            traces.join(MEMBER_EVENTS_FILE),
            member_line_naming(PLANTED_NETWORK_SECRET),
        )
        .unwrap();
        machine
    }

    /// The member's trace line with its handle field holding the text.
    pub(super) fn member_line_naming(text: &str) -> String {
        MEMBER_LINE.replace("\"handle\":\"bob\"", &format!("\"handle\":\"{text}\""))
    }

    #[test]
    fn spec_3_3_a_member_spawned_before_any_card_is_created_leaves_its_data_directory_private() {
        let machine = ScratchMachine::fresh(
            "member-first",
            NETWORK,
            &RunRecordKeeping {
                kept_in: None,
                invitation: None,
            },
        );

        let member = machine.spawn_mcp_server();

        let data_directory = machine
            .traces_directory_of_member(0)
            .parent()
            .unwrap()
            .to_path_buf();
        let mode = fs::metadata(&data_directory).map(|metadata| metadata.permissions().mode());
        drop(member);
        assert_eq!(
            mode.unwrap() & MODE_BITS,
            SPEC_PRIVATE_DIRECTORY_MODE,
            "{}",
            data_directory.display()
        );
    }

    #[test]
    fn observability_decision_16_a_machine_dropped_while_keeping_leaves_its_members_and_observers_data_beside_its_run_records_but_no_secret_key_file()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-data-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = ScratchMachine::fresh(
            "kept-data",
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.clone()),
                invitation: None,
            },
        );
        let data = machine.root.join(DATA_HOME);
        let observer_traces =
            TraceDirectory::resolve(&|name: &str| variable_of(&environment(&machine.root), name))
                .unwrap()
                .path();
        let member_traces = machine.traces_directory_of_member(0);
        let directories = machine.directories();
        let written = [
            (
                member_traces.join("20261117T080000Z-1111111111111111-estafette.events.jsonl"),
                "member events line\n",
            ),
            (
                member_traces.join("20261117T080000Z-1111111111111111-estafette.raw.jsonl"),
                "member raw line\n",
            ),
            (
                observer_traces.join("20261117T080001Z-2222222222222222-estafette.events.jsonl"),
                "observer events line\n",
            ),
            (
                directories.card_file(NETWORK, "amy").unwrap(),
                "card of amy\n",
            ),
        ];
        let secret_key_files = [
            (
                directories.card_sidecar_file(NETWORK, "amy").unwrap(),
                "secret key of amy\n",
            ),
            (
                directories.infrastructure_key_file(NETWORK).unwrap(),
                "bootstrap key of the host\n",
            ),
        ];
        for (file, content) in written.iter().chain(&secret_key_files) {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, content).unwrap();
        }
        let kept_data = kept_in.join(machine.name()).join(KEPT_DATA_DIRECTORY);
        let expected: BTreeSet<(PathBuf, String)> = written
            .iter()
            .map(|(file, content)| {
                (
                    kept_data.join(file.strip_prefix(&data).unwrap()),
                    (*content).to_owned(),
                )
            })
            .collect();

        drop(machine);

        let kept: BTreeSet<(PathBuf, String)> = files_listed_beneath(&kept_in)
            .into_iter()
            .map(|file| {
                let content = fs::read_to_string(&file).unwrap();
                (file, content)
            })
            .collect();
        assert_eq!(kept, expected);
        assert!(!data.exists(), "{} was not removed", data.display());
        remove_scratch::remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn spec_3_3_a_machine_dropped_while_keeping_leaves_out_the_relay_certificate_cache_but_keeps_the_host_certificate()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!(
                "estafette-kept-certificate-cache-test-{}",
                std::process::id()
            ),
        )
        .unwrap();
        let machine = ScratchMachine::fresh(
            "kept-certificate-cache",
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.clone()),
                invitation: None,
            },
        );
        let data = machine.root.join(DATA_HOME);
        let directories = machine.directories();
        let host_certificate = directories
            .infrastructure_certificate_file(NETWORK)
            .unwrap();
        let cached_account = directories
            .infrastructure_certificate_cache_directory(NETWORK)
            .unwrap()
            .join("acme-account.json");
        for (file, content) in [
            (&host_certificate, "certificate of the host\n"),
            (&cached_account, "account and private key of the relay\n"),
        ] {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, content).unwrap();
        }
        let kept_data = kept_in.join(machine.name()).join(KEPT_DATA_DIRECTORY);
        let expected =
            BTreeSet::from([kept_data.join(host_certificate.strip_prefix(&data).unwrap())]);

        drop(machine);

        let kept: BTreeSet<PathBuf> = files_listed_beneath(&kept_in);
        assert_eq!(kept, expected);
        remove_scratch::remove_scratch(&kept_in).unwrap();
    }

    /// Drops a machine of the tag keeping into the directory, its first member's run record and events file each holding the text, and returns the machine's name and where under its data directory that events file lay.
    fn dropped_keeping(tag: &str, kept_in: &Path, text: &str) -> (String, PathBuf) {
        let machine = ScratchMachine::fresh(
            tag,
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.to_path_buf()),
                invitation: None,
            },
        );
        fs::write(
            machine
                .root
                .join(format!("{RUN_RECORD_PREFIX}-0.{RUN_RECORD_EXTENSION}")),
            text,
        )
        .unwrap();
        let events_file = machine
            .traces_directory_of_member(0)
            .join("20261117T080000Z-1111111111111111-estafette.events.jsonl");
        fs::create_dir_all(events_file.parent().unwrap()).unwrap();
        fs::write(&events_file, text).unwrap();
        let in_data = events_file
            .strip_prefix(machine.root.join(DATA_HOME))
            .unwrap()
            .to_path_buf();
        let name = machine.name();
        drop(machine);
        (name, in_data)
    }

    #[test]
    fn observability_decision_16_a_machine_keeping_under_the_name_an_earlier_machine_kept_under_takes_a_name_numbered_unlike_a_scratch_root_and_leaves_the_earlier_records_whole()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-twice-test-{}", std::process::id()),
        )
        .unwrap();
        let first_text = "line of the first machine\n";
        let second_text = "line of the second machine\n";

        let (first_name, in_data) = dropped_keeping("kept-twice", &kept_in, first_text);
        let (second_name, _) = dropped_keeping("kept-twice", &kept_in, second_text);

        assert_eq!(first_name, second_name);
        let record = format!("{RUN_RECORD_PREFIX}-0.{RUN_RECORD_EXTENSION}");
        let first = kept_in.join(&first_name);
        let second = kept_in.join(format!("{second_name}.kept-2"));
        let expected = BTreeSet::from([
            (first.join(&record), first_text.to_owned()),
            (
                first.join(KEPT_DATA_DIRECTORY).join(&in_data),
                first_text.to_owned(),
            ),
            (second.join(&record), second_text.to_owned()),
            (
                second.join(KEPT_DATA_DIRECTORY).join(&in_data),
                second_text.to_owned(),
            ),
        ]);
        let kept: BTreeSet<(PathBuf, String)> = files_listed_beneath(&kept_in)
            .into_iter()
            .map(|file| {
                let content = fs::read_to_string(&file).unwrap();
                (file, content)
            })
            .collect();
        assert_eq!(kept, expected);
        remove_scratch::remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn observability_decision_16_a_machine_dropped_on_a_test_that_was_not_failing_whose_run_record_and_data_both_fail_to_be_kept_reports_both_failures()
     {
        let mut unreadable = Vec::new();
        let dropped = dropped_with_a_source_unread("kept-both-failures", |machine| {
            let record = machine
                .root
                .join(format!("{RUN_RECORD_PREFIX}-0.{RUN_RECORD_EXTENSION}"));
            let data = machine
                .traces_directory_of_member(0)
                .join(MEMBER_EVENTS_FILE);
            for file in [&record, &data] {
                fs::set_permissions(file, fs::Permissions::from_mode(UNREADABLE)).unwrap();
            }
            unreadable.extend([record, data.clone()]);
            data
        });

        assert_eq!(dropped.kept, BTreeSet::new());
        for file in &unreadable {
            assert!(
                dropped.message.contains(&file.display().to_string()),
                "{} is not named: {}",
                file.display(),
                dropped.message
            );
        }
    }

    #[test]
    fn observability_decision_16_a_machine_dropped_on_a_test_that_was_not_failing_whose_data_subdirectory_cannot_be_read_reports_each_failure_of_its_keeping_and_the_failed_removal_of_its_scratch_root()
     {
        let dropped = dropped_with_a_source_unread("kept-unreadable", |machine| {
            let unreadable_directory = machine.traces_directory_of_member(1);
            fs::create_dir_all(&unreadable_directory).unwrap();
            fs::write(
                unreadable_directory.join(SECOND_MEMBER_EVENTS_FILE),
                "events line\n",
            )
            .unwrap();
            fs::set_permissions(
                &unreadable_directory,
                fs::Permissions::from_mode(UNREADABLE),
            )
            .unwrap();
            unreadable_directory
        });

        assert_eq!(dropped.made_in_kept_in, BTreeSet::new());
        assert_eq!(
            dropped
                .message
                .matches(&dropped.unread.display().to_string())
                .count(),
            1,
            "{}",
            dropped.message
        );
        assert!(
            dropped
                .message
                .contains(&format!("{}: ", dropped.root.display())),
            "the failed removal of {} is not reported: {}",
            dropped.root.display(),
            dropped.message
        );
    }

    #[test]
    fn observability_decision_16_a_kept_data_file_holding_the_invitation_or_a_token_is_kept_redacted_with_a_note_beside_it_and_any_other_byte_for_byte()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-redacted-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = ScratchMachine::fresh(
            "kept-redacted",
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.clone()),
                invitation: Some(PLANTED_INVITATION.to_owned()),
            },
        );
        let data = machine.root.join(DATA_HOME);
        let traces = machine.traces_directory_of_member(0);
        let sidecar = machine
            .directories()
            .card_sidecar_file(NETWORK, "amy")
            .unwrap();
        let naming_the_invitation =
            traces.join("20261117T080000Z-1111111111111111-estafette.events.jsonl");
        let naming_the_token = traces.join("20261117T080000Z-1111111111111111-estafette.raw.jsonl");
        let naming_neither = traces.join("20261117T080001Z-2222222222222222-estafette.raw.jsonl");
        let naming_the_network_secret =
            traces.join("20261117T080002Z-3333333333333333-estafette.events.jsonl");
        let neither_bytes = [b'n', b'o', 0xff, 0x00, b'\n'];
        let network_config_file = machine.directories().network_config_file(NETWORK).unwrap();
        fs::create_dir_all(network_config_file.parent().unwrap()).unwrap();
        NetworkConfig {
            secret: PLANTED_NETWORK_SECRET.to_owned(),
            owner: PLANTED_OWNER.to_owned(),
            hosts: Vec::new(),
        }
        .rewrite(&network_config_file, 0)
        .unwrap();
        fs::create_dir_all(&traces).unwrap();
        fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        fs::write(&sidecar, format!("{PLANTED_TOKEN}\n")).unwrap();
        fs::write(
            &naming_the_invitation,
            format!("first line\njoined by {PLANTED_INVITATION} here\n"),
        )
        .unwrap();
        fs::write(&naming_the_token, format!("key {PLANTED_TOKEN} seen\n")).unwrap();
        fs::write(&naming_neither, neither_bytes).unwrap();
        fs::write(
            &naming_the_network_secret,
            format!("network {PLANTED_NETWORK_SECRET} joined\n"),
        )
        .unwrap();
        let kept_data = kept_in.join(machine.name()).join(KEPT_DATA_DIRECTORY);
        let kept_of = |file: &Path| kept_data.join(file.strip_prefix(&data).unwrap());
        let note_of = |file: &Path| {
            let mut note = kept_of(file).into_os_string();
            note.push(format!(".{REDACTED_NOTE_EXTENSION}"));
            PathBuf::from(note)
        };

        drop(machine);

        let kept: BTreeSet<PathBuf> = files_listed_beneath(&kept_in);
        assert_eq!(
            kept,
            BTreeSet::from([
                kept_of(&naming_the_invitation),
                note_of(&naming_the_invitation),
                kept_of(&naming_the_token),
                note_of(&naming_the_token),
                kept_of(&naming_neither),
                kept_of(&naming_the_network_secret),
                note_of(&naming_the_network_secret),
            ])
        );
        assert_eq!(
            fs::read_to_string(kept_of(&naming_the_invitation)).unwrap(),
            format!("first line\njoined by {REDACTED} here\n")
        );
        assert_eq!(
            fs::read_to_string(kept_of(&naming_the_token)).unwrap(),
            format!("key {REDACTED} seen\n")
        );
        assert_eq!(
            fs::read_to_string(kept_of(&naming_the_network_secret)).unwrap(),
            format!("network {REDACTED} joined\n")
        );
        assert_eq!(fs::read(kept_of(&naming_neither)).unwrap(), neither_bytes);
        for file in [
            &naming_the_invitation,
            &naming_the_token,
            &naming_the_network_secret,
        ] {
            let note = fs::read_to_string(note_of(file)).unwrap();
            let name = file.file_name().unwrap().to_string_lossy();
            assert!(note.contains(name.as_ref()), "{note}");
        }
        remove_scratch::remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn observability_decision_16_a_process_whose_kill_or_wait_failed_is_marked_beside_its_machines_run_records_and_one_that_ended_is_not()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-end-failure-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = ScratchMachine::fresh(
            "kept-end-failure",
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.clone()),
                invitation: None,
            },
        );
        drop(machine.spawn_owned(&mut Command::new(EXITS_IN_SUCCESS), ENDED_LABEL));
        let mut failing_its_end =
            machine.spawn_owned(&mut Command::new(EXITS_IN_SUCCESS), FAILED_LABEL);
        let failing_process_id = failing_its_end.id();
        failing_its_end.end_through(|_process| Err(io::Error::other(PLANTED_END_FAILURE)));
        drop(failing_its_end);
        let kept_machine = kept_in.join(machine.name());

        let _failed_end_reported = failure_of_dropping(machine);

        let kept: BTreeSet<PathBuf> = files_listed_beneath(&kept_in);
        let mark = kept_machine.join(format!(
            "{FAILED_LABEL}-{failing_process_id}.{END_FAILED_EXTENSION}"
        ));
        assert_eq!(kept, BTreeSet::from([mark.clone()]));
        let marked = fs::read_to_string(&mark).unwrap();
        assert!(
            marked.contains(FAILED_LABEL)
                && marked.contains(&failing_process_id.to_string())
                && marked.contains(PLANTED_END_FAILURE),
            "{marked}"
        );
        remove_scratch::remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn observability_decision_16_a_process_whose_first_line_never_comes_is_ended_before_the_failure_reaches_its_machine()
     {
        let machine = ScratchMachine::fresh(
            "first-line",
            NETWORK,
            &RunRecordKeeping {
                kept_in: None,
                invitation: None,
            },
        );
        let (only_the_stand_in_writes, stand_in_end) = io::pipe().unwrap();
        let mut stand_in = Command::new(SHELL);
        stand_in
            .args([SHELL_COMMAND_FLAG, CLOSES_ITS_OUTPUT_AND_RUNS_UNTIL_ENDED])
            .stdin(Stdio::null())
            .stderr(stand_in_end);

        let started = catch_unwind(AssertUnwindSafe(|| {
            machine.start_printing_lines(&mut stand_in, STAND_IN_LABEL, FIRST_LINE_WITHIN)
        }));
        drop(stand_in);

        let Err(_failure) = started else {
            panic!("a stand-in that closed its output printed a first line")
        };
        owned_process::require_stand_in_ended_within(
            only_the_stand_in_writes,
            owned_process::STAND_IN_ENDED_WITHIN,
            "its first-line wait failed",
        );
    }

    #[test]
    fn observability_decision_16_a_member_whose_stop_failed_is_kept_with_a_mark_beside_its_run_record_saying_so_and_one_that_stopped_with_none()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-stop-failure-test-{}", std::process::id()),
        )
        .unwrap();
        let secret = "network-secret-0123456789abcdef";
        let secrets = KeptSecrets::of_server_member(
            &format!("{NETWORK_SECRET_FIELD} = \"{secret}\"\n"),
            Some(PLANTED_INVITATION),
        )
        .unwrap();
        let failed_stop = Command::new(EXITS_IN_FAILURE).status().unwrap();
        let clean_stop = Command::new(EXITS_IN_SUCCESS).status().unwrap();
        let printed_by_the_failed_stop = format!("ssh: the server refused {secret}\n");
        let stop_printing = |status, printed: &str| {
            Ok(Output {
                status,
                stdout: Vec::new(),
                stderr: printed.as_bytes().to_vec(),
            })
        };

        keep_run_record(&kept_in, "amy", b"record of amy\n", &secrets).unwrap();
        keep_stop_failure(
            &kept_in,
            "amy",
            &stop_printing(failed_stop, &printed_by_the_failed_stop),
            &secrets,
        )
        .unwrap();
        keep_run_record(&kept_in, "bob", b"record of bob\n", &secrets).unwrap();
        keep_stop_failure(
            &kept_in,
            "bob",
            &stop_printing(clean_stop, "bob printed this while stopping\n"),
            &secrets,
        )
        .unwrap();

        let kept: BTreeSet<PathBuf> = files_listed_beneath(&kept_in);
        assert_eq!(
            kept,
            BTreeSet::from([
                kept_in.join("amy.stderr"),
                kept_in.join("amy.stop-failed"),
                kept_in.join("amy.stop-failed.redacted"),
                kept_in.join("bob.stderr"),
            ])
        );
        let mark = fs::read_to_string(kept_in.join("amy.stop-failed")).unwrap();
        assert!(
            mark.contains(&failed_stop.to_string())
                && mark.contains(&format!("ssh: the server refused {REDACTED}\n"))
                && !mark.contains(secret),
            "{mark}"
        );
        remove_scratch::remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn observability_decision_16_a_failed_end_whose_mark_names_the_invitation_is_kept_redacted_with_a_note_beside_it()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-redacted-mark-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = ScratchMachine::fresh(
            "kept-redacted-mark",
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.clone()),
                invitation: Some(PLANTED_INVITATION.to_owned()),
            },
        );
        let mut failing_its_end =
            machine.spawn_owned(&mut Command::new(EXITS_IN_SUCCESS), FAILED_LABEL);
        let mark_name = format!(
            "{FAILED_LABEL}-{}.{END_FAILED_EXTENSION}",
            failing_its_end.id()
        );
        failing_its_end
            .end_through(|_process| Err(io::Error::other(format!("refused {PLANTED_INVITATION}"))));
        drop(failing_its_end);
        let kept_mark = kept_in.join(machine.name()).join(&mark_name);

        let _failed_end_reported = failure_of_dropping(machine);

        let marked = fs::read_to_string(&kept_mark).unwrap();
        let mut note = kept_mark.into_os_string();
        note.push(format!(".{REDACTED_NOTE_EXTENSION}"));
        let noted = fs::read_to_string(note).unwrap_or_default();
        remove_scratch::remove_scratch(&kept_in).unwrap();
        assert!(
            marked.contains(&format!("refused {REDACTED}")) && !marked.contains(PLANTED_INVITATION),
            "{marked}"
        );
        assert!(noted.contains(&mark_name), "{noted}");
    }

    #[test]
    fn observability_decision_16_a_machine_dropped_on_a_failing_test_whose_base_directories_cannot_be_made_leaves_that_failure_standing_and_keeps_no_unsearched_data()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-unmade-base-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = ScratchMachine::fresh(
            "unmade-base",
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.clone()),
                invitation: None,
            },
        );
        let unwound = catch_unwind(AssertUnwindSafe(move || {
            machine.directories();
            let data_file = machine
                .traces_directory_of_member(0)
                .join("20261117T080000Z-1111111111111111-estafette.events.jsonl");
            fs::create_dir_all(data_file.parent().unwrap()).unwrap();
            fs::write(&data_file, "events line\n").unwrap();
            let configuration_home = machine.root.join(CONFIG_HOME);
            fs::rename(
                &configuration_home,
                machine.root.join(CONFIGURATION_HOME_SET_ASIDE),
            )
            .unwrap();
            fs::write(&configuration_home, "").unwrap();
            machine.servers_started.set(1);
            panic!("{FIRST_FAILURE}");
        }));

        let first = unwound.expect_err("the test did not fail first");
        assert_eq!(
            first.downcast_ref::<String>().map(String::as_str),
            Some(FIRST_FAILURE)
        );
        assert!(
            files_listed_beneath(&kept_in).is_empty(),
            "data whose secrets could not be read was kept in {}",
            kept_in.display()
        );
        remove_scratch::remove_scratch(&kept_in).unwrap();
    }

    /// What a machine dropped on a test that was not failing left: the message its drop failed with, every file it kept, every entry made in the directory it kept in, its scratch root and the source the setup made unreadable.
    struct DroppedFailing {
        message: String,
        kept: BTreeSet<PathBuf>,
        made_in_kept_in: BTreeSet<PathBuf>,
        root: PathBuf,
        unread: PathBuf,
    }

    /// Drops, on a test that was not failing, a machine of the tag keeping into a fresh directory, its first member's run record and events file each holding the planted token, once the setup handed in made one source unreadable and named it, then makes that source readable again and removes what is left.
    fn dropped_with_a_source_unread(
        tag: &str,
        make_unreadable: impl FnOnce(&ScratchMachine) -> PathBuf,
    ) -> DroppedFailing {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-{tag}-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = ScratchMachine::fresh(
            tag,
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.clone()),
                invitation: None,
            },
        );
        let holding_the_token = format!("key {PLANTED_TOKEN} seen\n");
        fs::write(
            machine
                .root
                .join(format!("{RUN_RECORD_PREFIX}-0.{RUN_RECORD_EXTENSION}")),
            &holding_the_token,
        )
        .unwrap();
        let events_file = machine
            .traces_directory_of_member(0)
            .join(MEMBER_EVENTS_FILE);
        fs::create_dir_all(events_file.parent().unwrap()).unwrap();
        fs::write(&events_file, &holding_the_token).unwrap();
        let unread = make_unreadable(&machine);
        let root = machine.root.clone();

        let dropped = catch_unwind(AssertUnwindSafe(move || drop(machine)));

        if unread.exists() {
            fs::set_permissions(&unread, fs::Permissions::from_mode(READABLE)).unwrap();
        }
        let failure =
            dropped.expect_err("a machine with a source unread was dropped with no failure");
        let message = failure.downcast_ref::<String>().unwrap().clone();
        let kept: BTreeSet<PathBuf> = files_listed_beneath(&kept_in);
        let made_in_kept_in: BTreeSet<PathBuf> = fs::read_dir(&kept_in)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        remove_scratch::remove_scratch(&kept_in).unwrap();
        if root.exists() {
            remove_scratch::remove_scratch(&root).unwrap();
        }
        DroppedFailing {
            message,
            kept,
            made_in_kept_in,
            root,
            unread,
        }
    }

    #[test]
    fn observability_decision_16_a_machine_whose_network_configuration_does_not_parse_keeps_nothing_and_names_the_configuration()
     {
        let dropped = dropped_with_a_source_unread("unloaded-configuration", |machine| {
            let configuration = machine.directories().network_config_file(NETWORK).unwrap();
            fs::create_dir_all(configuration.parent().unwrap()).unwrap();
            fs::write(&configuration, NOT_A_NETWORK_CONFIGURATION).unwrap();
            configuration
        });

        assert_eq!(dropped.made_in_kept_in, BTreeSet::new());
        assert!(
            dropped
                .message
                .contains(&dropped.unread.display().to_string()),
            "{}",
            dropped.message
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_network_configurations_secret_line_does_not_parse_names_the_configuration_and_the_kind_of_failure_and_neither_prints_nor_keeps_the_line()
     {
        let dropped = dropped_with_a_source_unread("malformed-secret-line", |machine| {
            let configuration = machine.directories().network_config_file(NETWORK).unwrap();
            fs::create_dir_all(configuration.parent().unwrap()).unwrap();
            fs::write(
                &configuration,
                format!("{NETWORK_SECRET_FIELD} = \"{PLANTED_MALFORMED_SECRET}\n"),
            )
            .unwrap();
            configuration
        });

        assert_eq!(dropped.kept, BTreeSet::new());
        assert!(
            dropped
                .message
                .contains(&dropped.unread.display().to_string())
                && dropped.message.contains(CONFIGURATION_NOT_TOML)
                && !dropped.message.contains(PLANTED_MALFORMED_SECRET),
            "{}",
            dropped.message
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_network_configuration_names_its_secret_but_does_not_load_keeps_its_data_with_that_secret_redacted()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-unloaded-secret-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = ScratchMachine::fresh(
            "unloaded-secret",
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.clone()),
                invitation: None,
            },
        );
        let configuration = machine.directories().network_config_file(NETWORK).unwrap();
        fs::create_dir_all(configuration.parent().unwrap()).unwrap();
        fs::write(
            &configuration,
            format!("{NETWORK_SECRET_FIELD} = \"{PLANTED_NETWORK_SECRET}\"\n"),
        )
        .unwrap();
        let events_file = machine
            .traces_directory_of_member(0)
            .join(MEMBER_EVENTS_FILE);
        fs::create_dir_all(events_file.parent().unwrap()).unwrap();
        fs::write(
            &events_file,
            format!("network {PLANTED_NETWORK_SECRET} joined\n"),
        )
        .unwrap();
        let kept_events_file = kept_in.join(machine.name()).join(KEPT_DATA_DIRECTORY).join(
            events_file
                .strip_prefix(machine.root.join(DATA_HOME))
                .unwrap(),
        );

        drop(machine);

        assert_eq!(
            fs::read_to_string(&kept_events_file).unwrap(),
            format!("network {REDACTED} joined\n")
        );
        remove_scratch::remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn observability_decision_16_a_machine_whose_secret_key_file_cannot_be_read_keeps_neither_its_data_nor_its_run_records_unsearched_and_names_the_key_file()
     {
        let dropped = dropped_with_a_source_unread("unread-key-file", |machine| {
            let sidecar = machine
                .directories()
                .card_sidecar_file(NETWORK, CARD)
                .unwrap();
            fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
            fs::write(&sidecar, format!("{PLANTED_TOKEN}\n")).unwrap();
            fs::set_permissions(&sidecar, fs::Permissions::from_mode(UNREADABLE)).unwrap();
            sidecar
        });

        assert_eq!(dropped.made_in_kept_in, BTreeSet::new());
        assert!(
            dropped
                .message
                .contains(&dropped.unread.display().to_string()),
            "{}",
            dropped.message
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_directory_holding_a_secret_key_file_cannot_be_read_keeps_neither_its_data_nor_its_run_records_unsearched_and_names_the_directory()
     {
        let dropped = dropped_with_a_source_unread("unread-key-directory", |machine| {
            let sidecar = machine
                .directories()
                .card_sidecar_file(NETWORK, CARD)
                .unwrap();
            let cards = sidecar.parent().unwrap().to_path_buf();
            fs::create_dir_all(&cards).unwrap();
            fs::write(&sidecar, format!("{PLANTED_TOKEN}\n")).unwrap();
            fs::set_permissions(&cards, fs::Permissions::from_mode(UNREADABLE)).unwrap();
            cards
        });

        assert_eq!(dropped.made_in_kept_in, BTreeSet::new());
        assert!(
            dropped
                .message
                .contains(&dropped.unread.display().to_string()),
            "{}",
            dropped.message
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_network_configuration_cannot_be_read_keeps_nothing_and_names_the_configuration()
     {
        let dropped = dropped_with_a_source_unread("unread-configuration", |machine| {
            let configuration = planted_network_configuration(machine, NETWORK);
            fs::set_permissions(&configuration, fs::Permissions::from_mode(UNREADABLE)).unwrap();
            configuration
        });

        assert_eq!(dropped.made_in_kept_in, BTreeSet::new());
        assert!(
            dropped
                .message
                .contains(&dropped.unread.display().to_string()),
            "{}",
            dropped.message
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_other_networks_configuration_names_no_secret_keeps_nothing_and_names_that_configuration()
     {
        let dropped = dropped_with_a_source_unread("configuration-naming-no-secret", |machine| {
            let configuration = machine
                .directories()
                .network_config_file(OTHER_NETWORK)
                .unwrap();
            fs::create_dir_all(configuration.parent().unwrap()).unwrap();
            fs::write(&configuration, CONFIGURATION_NAMING_NO_SECRET).unwrap();
            configuration
        });

        assert_eq!(dropped.made_in_kept_in, BTreeSet::new());
        assert!(
            dropped
                .message
                .contains(&dropped.unread.display().to_string()),
            "{}",
            dropped.message
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_certificate_cache_file_cannot_be_read_keeps_nothing_and_names_the_file()
     {
        let dropped = dropped_with_a_source_unread("unread-certificate-cache", |machine| {
            let cached_account = planted_cached_account(machine);
            fs::set_permissions(&cached_account, fs::Permissions::from_mode(UNREADABLE)).unwrap();
            cached_account
        });

        assert_eq!(dropped.made_in_kept_in, BTreeSet::new());
        assert!(
            dropped
                .message
                .contains(&dropped.unread.display().to_string()),
            "{}",
            dropped.message
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_secrets_cannot_be_read_keeps_not_even_the_mark_of_a_failed_end_and_names_that_end_beside_the_unread_configuration()
     {
        let failed_mark = RefCell::new(PathBuf::new());
        let dropped = dropped_with_a_source_unread("unread-secrets-failed-end", |machine| {
            let mut failing_its_end =
                machine.spawn_owned(&mut Command::new(EXITS_IN_SUCCESS), FAILED_LABEL);
            failed_mark.replace(machine.root.join(format!(
                "{FAILED_LABEL}-{}.{END_FAILED_EXTENSION}",
                failing_its_end.id()
            )));
            failing_its_end.end_through(|_process| Err(io::Error::other(PLANTED_END_FAILURE)));
            drop(failing_its_end);
            let configuration = planted_network_configuration(machine, NETWORK);
            fs::set_permissions(&configuration, fs::Permissions::from_mode(UNREADABLE)).unwrap();
            configuration
        });

        assert_eq!(dropped.made_in_kept_in, BTreeSet::new());
        for named in [&dropped.unread, &failed_mark.into_inner()] {
            assert!(
                dropped.message.contains(&named.display().to_string()),
                "{} is not named: {}",
                named.display(),
                dropped.message
            );
        }
    }

    #[test]
    fn observability_decision_16_a_machine_keeps_its_data_and_run_records_with_the_secret_of_another_networks_configuration_redacted()
     {
        let text = format!("network {PLANTED_OTHER_NETWORK_SECRET} joined\n");
        let kept = kept_after_planting("other-network-secret", &text, |machine| {
            planted_network_configuration(machine, NETWORK);
            let other = machine
                .directories()
                .network_config_file(OTHER_NETWORK)
                .unwrap();
            NetworkConfig {
                secret: PLANTED_OTHER_NETWORK_SECRET.to_owned(),
                owner: PLANTED_OWNER.to_owned(),
                hosts: Vec::new(),
            }
            .rewrite(&other, 0)
            .unwrap();
        });

        let redacted = format!("network {REDACTED} joined\n");
        assert_eq!(kept.events, redacted.as_bytes());
        assert_eq!(kept.run_record, redacted.as_bytes());
    }

    #[test]
    fn observability_decision_16_a_machine_keeps_its_data_and_run_records_with_the_text_of_a_key_file_under_each_base_directory_redacted_and_no_key_file()
     {
        let text = format!("keys {PLANTED_OWNER_KEY} {PLANTED_TOKEN} {PLANTED_RUNTIME_KEY}\n");
        let kept = kept_after_planting("key-under-each-base", &text, |machine| {
            let directories = machine.directories();
            for (key_file, key) in [
                (
                    directories.owner_key_file(NETWORK).unwrap(),
                    PLANTED_OWNER_KEY,
                ),
                (
                    directories.card_sidecar_file(NETWORK, CARD).unwrap(),
                    PLANTED_TOKEN,
                ),
                (
                    directories
                        .inbox_socket_file(std::process::id())
                        .with_file_name(RUNTIME_KEY_FILE),
                    PLANTED_RUNTIME_KEY,
                ),
            ] {
                fs::create_dir_all(key_file.parent().unwrap()).unwrap();
                fs::write(&key_file, format!("{key}\n")).unwrap();
            }
        });

        let redacted = format!("keys {REDACTED} {REDACTED} {REDACTED}\n");
        assert_eq!(kept.events, redacted.as_bytes());
        assert_eq!(kept.run_record, redacted.as_bytes());
        let key_files_kept: Vec<&PathBuf> = kept
            .files
            .iter()
            .filter(|file| is_a_secret_key_file(file))
            .collect();
        assert_eq!(key_files_kept, Vec::<&PathBuf>::new());
    }

    #[test]
    fn observability_decision_16_a_machine_keeps_its_data_and_run_records_with_the_text_of_a_certificate_cache_file_redacted_and_leaves_the_cache_out()
     {
        let text = format!("cache {PLANTED_CACHED_KEY} read\n");
        let kept = kept_after_planting("certificate-cache-text", &text, |machine| {
            planted_cached_account(machine);
        });

        let redacted = format!("cache {REDACTED} read\n");
        assert_eq!(kept.events, redacted.as_bytes());
        assert_eq!(kept.run_record, redacted.as_bytes());
        let cache_files_kept: Vec<&PathBuf> = kept
            .files
            .iter()
            .filter(|file| file.ends_with(CACHED_ACCOUNT_FILE))
            .collect();
        assert_eq!(cache_files_kept, Vec::<&PathBuf>::new());
    }

    #[test]
    fn observability_decision_16_a_machines_run_record_holding_a_secret_is_kept_redacted_with_a_note_beside_it_naming_it_and_one_holding_none_byte_for_byte()
     {
        let holding = format!("network {PLANTED_NETWORK_SECRET} joined\n");
        let holding_none = [b'n', b'o', 0xff, 0x00, b'\n'];

        let redacted = kept_after_planting("run-record-note", &holding, |machine| {
            planted_network_configuration(machine, NETWORK);
        });
        let kept_whole = kept_after_planting(
            "run-record-whole",
            &String::from_utf8_lossy(&holding_none),
            |machine| {
                fs::write(
                    machine
                        .root
                        .join(format!("{RUN_RECORD_PREFIX}-0.{RUN_RECORD_EXTENSION}")),
                    holding_none,
                )
                .unwrap();
            },
        );

        assert_eq!(
            redacted.run_record,
            format!("network {REDACTED} joined\n").as_bytes()
        );
        let note = redacted.run_record_note.unwrap_or_default();
        assert!(
            note.contains(&format!("{RUN_RECORD_PREFIX}-0.{RUN_RECORD_EXTENSION}")),
            "{note}"
        );
        assert_eq!(kept_whole.run_record, holding_none);
        assert_eq!(kept_whole.run_record_note, None);
    }

    #[test]
    fn observability_decision_16_a_secret_whose_source_is_written_after_one_keeping_of_run_records_is_redacted_from_what_the_machines_drop_keeps()
     {
        let holding = format!("network {PLANTED_NETWORK_SECRET} joined\n");
        let earlier = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!(
                "estafette-kept-before-the-secret-test-{}",
                std::process::id()
            ),
        )
        .unwrap();

        let kept = kept_after_planting("secret-written-after-a-keeping", &holding, |machine| {
            machine.keep_run_records(&earlier).unwrap();
            planted_network_configuration(machine, NETWORK);
        });

        let kept_earlier =
            fs::read(earlier.join(format!("{RUN_RECORD_PREFIX}-0.{RUN_RECORD_EXTENSION}")))
                .unwrap();
        remove_scratch::remove_scratch(&earlier).unwrap();
        assert_eq!(kept_earlier, holding.as_bytes());
        let redacted = format!("network {REDACTED} joined\n");
        assert_eq!(kept.events, redacted.as_bytes());
        assert_eq!(kept.run_record, redacted.as_bytes());
    }

    #[test]
    fn observability_decision_16_a_keeping_of_run_records_whose_secrets_cannot_be_read_writes_nothing_and_the_machines_drop_once_they_can_keeps_them_redacted()
     {
        let holding = format!("network {PLANTED_NETWORK_SECRET} joined\n");
        let earlier = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-while-unread-test-{}", std::process::id()),
        )
        .unwrap();

        let kept = kept_after_planting("unbuilt-at-a-keeping", &holding, |machine| {
            let configuration = planted_network_configuration(machine, NETWORK);
            fs::set_permissions(&configuration, fs::Permissions::from_mode(UNREADABLE)).unwrap();
            let unbuilt = machine.keep_run_records(&earlier).map(drop);
            fs::set_permissions(&configuration, fs::Permissions::from_mode(READABLE)).unwrap();
            let failure = unbuilt.expect_err("run records were kept with their secrets unread");
            assert!(
                failure
                    .to_string()
                    .contains(&configuration.display().to_string()),
                "{failure}"
            );
        });

        let made_in_earlier: BTreeSet<PathBuf> = fs::read_dir(&earlier)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        remove_scratch::remove_scratch(&earlier).unwrap();
        assert_eq!(made_in_earlier, BTreeSet::new());
        let redacted = format!("network {REDACTED} joined\n");
        assert_eq!(kept.events, redacted.as_bytes());
        assert_eq!(kept.run_record, redacted.as_bytes());
    }

    #[test]
    fn observability_decision_16_a_process_whose_kill_or_wait_failed_fails_a_test_that_was_not_failing_once_its_machine_has_kept_the_mark_and_removed_its_scratch_root()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-failed-end-fails-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = machine_keeping_in("failed-end-fails", &kept_in);
        let mut failing_its_end =
            machine.spawn_owned(&mut Command::new(EXITS_IN_SUCCESS), FAILED_LABEL);
        let mark_name = format!(
            "{FAILED_LABEL}-{}.{END_FAILED_EXTENSION}",
            failing_its_end.id()
        );
        failing_its_end.end_through(|_process| Err(io::Error::other(PLANTED_END_FAILURE)));
        drop(failing_its_end);
        let root = machine.root.clone();
        let kept_mark = kept_in.join(machine.name()).join(&mark_name);

        let message = failure_of_dropping(machine);

        let mark_kept = kept_mark.is_file();
        remove_scratch::remove_scratch(&kept_in).unwrap();
        assert!(
            message.contains(&format!(
                "{FAILED_END_REPORTED} {} says",
                root.join(&mark_name).display()
            )),
            "{message}"
        );
        assert!(mark_kept, "{} was not kept", kept_mark.display());
        assert!(!root.exists(), "{} was not removed", root.display());
    }

    #[test]
    fn observability_decision_16_a_process_whose_kill_or_wait_failed_on_a_test_already_failing_is_printed_by_its_machines_drop()
     {
        let scratch = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!(
                "estafette-printing-while-unwinding-test-{}",
                std::process::id()
            ),
        )
        .unwrap();
        let standard_error = scratch.join(FIXTURE_STANDARD_ERROR);

        fixture_exit_within(
            PRINTING_WHILE_UNWINDING_FIXTURE,
            &[],
            File::create(&standard_error).unwrap().into(),
            FIXTURE_ENDED_WITHIN,
        );

        let printed = String::from_utf8_lossy(&fs::read(&standard_error).unwrap()).into_owned();
        remove_scratch::remove_scratch(&scratch).unwrap();
        assert!(
            printed.contains(FIRST_FAILURE) && printed.contains(FAILED_END_REPORTED),
            "{printed}"
        );
    }

    #[test]
    fn observability_decision_16_a_machine_dropped_on_a_failing_test_whose_standard_error_is_closed_ends_in_that_failure_and_never_aborts()
     {
        let (closed_by_its_reader, standard_error) = io::pipe().unwrap();
        drop(closed_by_its_reader);

        let child = fixture_exit_within(
            PRINTING_WHILE_UNWINDING_FIXTURE,
            &[],
            standard_error.into(),
            FIXTURE_ENDED_WITHIN,
        );

        assert_eq!(child.code(), Some(FAILED_TEST_EXIT_CODE), "{child:?}");
    }

    /// Starts a member's count and a process whose end fails on a machine that keeps nothing, so its drop prints a timeline and that failed end, then fails first, so the drop prints while the thread unwinds; run as the child process of a parent test, which sets the variable, and refusing to run without it; ignored in the suite.
    #[test]
    #[ignore = "a fixture: the child process of the tests of a machine printing while its test fails"]
    fn forced_failure_with_a_machine_printing_while_it_unwinds() {
        assert_eq!(
            std::env::var(FIXTURE_VARIABLE).ok().as_deref(),
            Some(PRINTING_WHILE_UNWINDING_FIXTURE),
            "{PRINTING_WHILE_UNWINDING_FIXTURE} is a fixture its parent test runs with {FIXTURE_VARIABLE} naming it"
        );
        let machine = ScratchMachine::fresh(
            "printing-while-unwinding",
            NETWORK,
            &RunRecordKeeping {
                kept_in: None,
                invitation: None,
            },
        );
        machine.servers_started.set(1);
        let mut failing_its_end =
            machine.spawn_owned(&mut Command::new(EXITS_IN_SUCCESS), FAILED_LABEL);
        failing_its_end.end_through(|_process| Err(io::Error::other(PLANTED_END_FAILURE)));
        drop(failing_its_end);
        panic!("{FIRST_FAILURE}");
    }

    #[test]
    fn observability_decision_16_a_machine_whose_test_directory_cannot_be_made_names_that_directory()
     {
        let scratch = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!(
                "estafette-kept-unmade-test-directory-test-{}",
                std::process::id()
            ),
        )
        .unwrap();
        fs::write(scratch.join(NOT_A_DIRECTORY), "").unwrap();
        let test_directory = scratch
            .join(NOT_A_DIRECTORY)
            .join(KEPT_BENEATH_NOT_A_DIRECTORY);
        let machine = machine_keeping_in("unmade-test-directory", &test_directory);

        let message = failure_of_dropping(machine);

        remove_scratch::remove_scratch(&scratch).unwrap();
        assert!(
            message.contains(&format!("{}: ", test_directory.display())),
            "{message}"
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_scratch_root_cannot_be_listed_names_it_in_the_failure_of_each_step_that_lists_it()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-unlisted-root-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = machine_keeping_in("unlisted-root", &kept_in);
        let root = machine.root.clone();
        fs::set_permissions(&root, fs::Permissions::from_mode(UNLISTABLE)).unwrap();

        let message = failure_of_dropping(machine);

        fs::set_permissions(&root, fs::Permissions::from_mode(READABLE)).unwrap();
        remove_scratch::remove_scratch(&root).unwrap();
        remove_scratch::remove_scratch(&kept_in).unwrap();
        let failed_ends_run_records_end_marks_and_removal = 4;
        assert_eq!(
            message.matches(&format!("{}: ", root.display())).count(),
            failed_ends_run_records_end_marks_and_removal,
            "{message}"
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_end_mark_cannot_be_read_names_the_mark() {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-unread-mark-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = machine_keeping_in("unread-mark", &kept_in);
        let mark = machine
            .root
            .join(format!("{FAILED_LABEL}-1.{END_FAILED_EXTENSION}"));
        fs::write(&mark, PLANTED_END_FAILURE).unwrap();
        fs::set_permissions(&mark, fs::Permissions::from_mode(UNREADABLE)).unwrap();

        let message = failure_of_dropping(machine);

        remove_scratch::remove_scratch(&kept_in).unwrap();
        assert!(
            message.contains(&format!("{}: ", mark.display())),
            "{message}"
        );
    }

    #[test]
    fn observability_decision_16_a_machine_whose_writer_cannot_write_a_kept_file_names_that_kept_file_for_a_run_record_an_end_mark_and_a_data_file()
     {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-unwritten-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = machine_keeping_in("unwritten", &kept_in);
        planted_network_configuration(&machine, NETWORK);
        let long_name = "a".repeat(NAME_TOO_LONG_FOR_ITS_NOTE);
        let holding_the_secret = format!("network {PLANTED_NETWORK_SECRET} joined\n");
        let run_record = format!("{long_name}.{RUN_RECORD_EXTENSION}");
        let end_mark = format!("{long_name}.{END_FAILED_EXTENSION}");
        for file in [
            machine.root.join(&run_record),
            machine.root.join(&end_mark),
            machine.root.join(DATA_HOME).join(&run_record),
        ] {
            fs::write(file, &holding_the_secret).unwrap();
        }
        let kept = kept_in.join(machine.name());
        let unwritten_notes = [
            kept.join(format!("{run_record}.{REDACTED_NOTE_EXTENSION}")),
            kept.join(format!("{end_mark}.{REDACTED_NOTE_EXTENSION}")),
            kept.join(KEPT_DATA_DIRECTORY)
                .join(format!("{run_record}.{REDACTED_NOTE_EXTENSION}")),
        ];

        let message = failure_of_dropping(machine);

        remove_scratch::remove_scratch(&kept_in).unwrap();
        for note in &unwritten_notes {
            assert!(
                message.contains(&format!("{}: ", note.display())),
                "{} is not named: {message}",
                note.display()
            );
        }
    }

    /// A machine of the tag whose records are kept in the directory, with no invitation set.
    fn machine_keeping_in(tag: &str, kept_in: &Path) -> ScratchMachine {
        ScratchMachine::fresh(
            tag,
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.to_path_buf()),
                invitation: None,
            },
        )
    }

    /// The message the machine's drop fails a test that was not failing with, failing the test when the drop reports nothing.
    fn failure_of_dropping(machine: ScratchMachine) -> String {
        let dropped = catch_unwind(AssertUnwindSafe(move || drop(machine)));
        let failure = dropped.expect_err("the machine was dropped with no failure");
        failure.downcast_ref::<String>().unwrap().clone()
    }

    /// Writes the network configuration of the network on the machine naming the planted secret, and returns its file.
    fn planted_network_configuration(machine: &ScratchMachine, network: &str) -> PathBuf {
        let configuration = machine.directories().network_config_file(network).unwrap();
        fs::create_dir_all(configuration.parent().unwrap()).unwrap();
        NetworkConfig {
            secret: PLANTED_NETWORK_SECRET.to_owned(),
            owner: PLANTED_OWNER.to_owned(),
            hosts: Vec::new(),
        }
        .rewrite(&configuration, 0)
        .unwrap();
        configuration
    }

    /// Writes a file holding the planted cached key in the relay's certificate cache of the machine, and returns it.
    fn planted_cached_account(machine: &ScratchMachine) -> PathBuf {
        let cached_account = machine
            .directories()
            .infrastructure_certificate_cache_directory(NETWORK)
            .unwrap()
            .join(CACHED_ACCOUNT_FILE);
        fs::create_dir_all(cached_account.parent().unwrap()).unwrap();
        fs::write(
            &cached_account,
            format!("{{\"key\":\"{PLANTED_CACHED_KEY}\"}}\n"),
        )
        .unwrap();
        cached_account
    }

    /// What a machine dropped while keeping left: every file it kept, the kept copies of its first member's events file and run record, and the note beside that run record, none when it has none.
    struct KeptAfterPlanting {
        files: BTreeSet<PathBuf>,
        events: Vec<u8>,
        run_record: Vec<u8>,
        run_record_note: Option<String>,
    }

    /// Drops, keeping into a fresh directory, a machine of the tag whose first member's run record and events file each hold the text, once the setup handed in planted its sources, requiring the drop to report no failure, then removes what was kept.
    fn kept_after_planting(
        tag: &str,
        text: &str,
        plant: impl FnOnce(&ScratchMachine),
    ) -> KeptAfterPlanting {
        let kept_in = fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-kept-{tag}-test-{}", std::process::id()),
        )
        .unwrap();
        let machine = ScratchMachine::fresh(
            tag,
            NETWORK,
            &RunRecordKeeping {
                kept_in: Some(kept_in.clone()),
                invitation: None,
            },
        );
        let record_name = format!("{RUN_RECORD_PREFIX}-0.{RUN_RECORD_EXTENSION}");
        fs::write(machine.root.join(&record_name), text).unwrap();
        let events_file = machine
            .traces_directory_of_member(0)
            .join(MEMBER_EVENTS_FILE);
        fs::create_dir_all(events_file.parent().unwrap()).unwrap();
        fs::write(&events_file, text).unwrap();
        plant(&machine);
        let kept_machine = kept_in.join(machine.name());
        let kept_events = kept_machine.join(KEPT_DATA_DIRECTORY).join(
            events_file
                .strip_prefix(machine.root.join(DATA_HOME))
                .unwrap(),
        );

        drop(machine);

        let kept = KeptAfterPlanting {
            files: files_listed_beneath(&kept_in),
            events: fs::read(kept_events).unwrap(),
            run_record: fs::read(kept_machine.join(&record_name)).unwrap(),
            run_record_note: fs::read_to_string(
                kept_machine.join(format!("{record_name}.{REDACTED_NOTE_EXTENSION}")),
            )
            .ok(),
        };
        remove_scratch::remove_scratch(&kept_in).unwrap();
        kept
    }

    /// Every file beneath the kept directory at any depth, failing the test, naming the directory, when it or one beneath it cannot be listed, so an unlistable directory never reads as one holding nothing.
    pub(super) fn files_listed_beneath(directory: &Path) -> BTreeSet<PathBuf> {
        if let Err(failure) = fs::read_dir(directory) {
            panic!("{}: {failure}", directory.display());
        }
        files_beneath_for_keeping(directory)
            .into_iter()
            .map(|listed| listed.unwrap_or_else(|failure| panic!("{failure}")))
            .collect()
    }
}
