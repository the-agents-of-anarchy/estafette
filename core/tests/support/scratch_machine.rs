use std::cell::Cell;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use estafette::dirs::{
    Directories, DirectoryFilesystem, OperatingSystemFilesystem, TraceDirectory, trace_file_of,
};
use estafette::keys::parse_public_key;
use iroh::EndpointId;

#[path = "../common/fresh_directory.rs"]
pub mod fresh_directory;
#[path = "owned_process.rs"]
pub mod owned_process;
#[path = "../common/remove_scratch.rs"]
pub mod remove_scratch;
#[path = "trace_reader.rs"]
pub mod trace_reader;

use owned_process::{END_FAILED_EXTENSION, OwnedProcess};
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
/// The extension spec 3.3 gives a relay's certificate cache directory, which holds its private key.
const CERTIFICATE_CACHE_EXTENSION: &str = "certs";
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
        let environment = environment(&self.root);
        Directories::establish(
            |name: &str| variable_of(&environment, name),
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

    /// How many MCP servers this machine has started, which is also the number of the next one.
    pub fn members_started(&self) -> u32 {
        self.servers_started.get()
    }

    /// The timeline of each member this machine started, headed by the member's number and this machine's root, for a failing test to print.
    fn member_timelines(&self) -> Vec<String> {
        (0..self.members_started())
            .map(|number| {
                format!(
                    "{TIMELINE_HEADING} {number} on {}:\n{}",
                    self.root.display(),
                    timeline_read_as(
                        &self.traces_directory_of_member(number),
                        WritingProcess::Running
                    )
                )
            })
            .collect()
    }

    /// The trace directory of the member of that number, which no other member writes in.
    pub fn traces_directory_of_member(&self, number: u32) -> PathBuf {
        let environment = self.member_environment(number);
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

    /// Starts the binary's MCP server for the network with its standard input and output piped, its run record written to a file of the scratch root numbered for the server, and a trace directory of its own made ready, owned from its spawn.
    pub fn spawn_mcp_server(&self) -> OwnedProcess {
        let started = self.members_started();
        self.servers_started.set(started.checked_add(1).unwrap());
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

    /// Keeps this machine's run records, the marks of its processes whose end failed, and its data directory together in a directory of its own beneath the test's, named for this machine and numbered after any earlier keeping of that name.
    fn keep_records(&self, directory: &Path) -> io::Result<()> {
        let kept = fresh_kept_directory(directory, &self.name())?;
        let run_records = self.keep_run_records(&kept);
        let end_failures = self.keep_end_failures(&kept);
        let data = self.keep_data_directory(&kept);
        run_records.and(end_failures).and(data).map(|_data| ())
    }

    /// Copies every member's run record into the directory, named for the member, each line that holds a secret of this machine kept with that secret replaced by the redaction marker, and returns the kept files.
    pub fn keep_run_records(&self, directory: &Path) -> io::Result<Vec<PathBuf>> {
        let secrets = self.secrets_by_length();
        let mut kept = Vec::new();
        for record in self.run_records()? {
            let member_name = record.file_stem().unwrap_or_default().to_string_lossy();
            kept.push(keep_run_record(
                directory,
                &member_name,
                &fs::read_to_string(&record)?,
                &secrets,
            )?);
        }
        Ok(kept)
    }

    /// Copies the mark of every process of this machine whose end failed into the directory as it was written, and returns the kept marks.
    fn keep_end_failures(&self, directory: &Path) -> io::Result<Vec<PathBuf>> {
        let mut kept = Vec::new();
        for mark in self.root_files_with_extension(END_FAILED_EXTENSION)? {
            let kept_mark = directory.join(mark.file_name().unwrap_or_default());
            fs::copy(&mark, &kept_mark)?;
            eprintln!("failed end kept at {}", kept_mark.display());
            kept.push(kept_mark);
        }
        Ok(kept)
    }

    /// Copies every file under this machine's data directory, its members' and its observers' trace files included, into the data directory inside the directory, leaving out each file that holds a secret key, and returns the kept data directory.
    fn keep_data_directory(&self, directory: &Path) -> io::Result<PathBuf> {
        let data = self.root.join(DATA_HOME);
        let kept = directory.join(KEPT_DATA_DIRECTORY);
        for file in files_beneath(&data) {
            let in_data = file.strip_prefix(&data).map_err(io::Error::other)?;
            if holds_a_secret_key(in_data) {
                continue;
            }
            let kept_file = kept.join(in_data);
            fs::create_dir_all(kept_file.parent().unwrap_or(&kept))?;
            fs::copy(&file, &kept_file)?;
        }
        eprintln!("data directory kept at {}", kept.display());
        Ok(kept)
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

    /// The files directly in this machine's scratch root with the extension, in order of name.
    fn root_files_with_extension(&self, wanted: &str) -> io::Result<Vec<PathBuf>> {
        let mut files: Vec<PathBuf> = fs::read_dir(&self.root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<PathBuf>>>()?
            .into_iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == wanted)
            })
            .collect();
        files.sort();
        Ok(files)
    }

    /// Every file under this machine's configuration, data and runtime directories, at any depth.
    pub fn base_directory_files(&self) -> Vec<PathBuf> {
        [CONFIG_HOME, DATA_HOME, RUNTIME_DIRECTORY]
            .into_iter()
            .flat_map(|base| files_beneath(&self.root.join(base)))
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
            self.root
                .join(DATA_HOME)
                .join(MEMBERS_UNDER_DATA_HOME)
                .join(number.to_string())
                .into_os_string(),
        ));
        variables.push((
            CARDS_DIRECTORY_VARIABLE,
            cards.parent().unwrap().as_os_str().to_owned(),
        ));
        variables
    }

    /// The secrets of every readable file under this machine's base directories but its debug traces, which are a record of the run like the run record itself, longest first.
    fn secrets_by_length(&self) -> Vec<String> {
        let contents: Vec<String> = self
            .base_directory_files()
            .iter()
            .filter(|file| {
                file.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(trace_file_of)
                    .is_none()
            })
            .filter_map(|file| fs::read(file).ok())
            .map(|content| String::from_utf8_lossy(&content).into_owned())
            .collect();
        self.run_records
            .secrets_longest_first(contents.iter().map(String::as_str))
    }
}

impl Drop for ScratchMachine {
    fn drop(&mut self) {
        if std::thread::panicking() {
            for timeline in self.member_timelines() {
                eprintln!("{timeline}");
            }
        }
        let kept = self
            .run_records
            .kept_in()
            .map(|directory| self.keep_records(directory));
        let removed = remove_scratch::remove_scratch(&self.root);
        if let Some(Err(failure)) = kept {
            fail_unless_panicking(&format!(
                "records of {} not kept: {failure}",
                self.root.display()
            ));
        }
        if let Err(failure) = removed {
            fail_unless_panicking(&failure.to_string());
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

/// Creates the directory a machine's or a member's records are kept in beneath the test's, named for it, or for a later keeping of that name its number joined in a form no scratch root's name takes, so no keeping overwrites or merges into another and no number reads as a scratch root's own.
pub fn fresh_kept_directory(directory: &Path, name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(directory)?;
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

/// Writes the member's run record into the directory, named for the member, each secret replaced by the redaction marker, and returns the kept file.
pub fn keep_run_record(
    directory: &Path,
    member_name: &str,
    record: &str,
    secrets_by_length: &[String],
) -> io::Result<PathBuf> {
    let kept_record = directory.join(format!("{member_name}.{RUN_RECORD_EXTENSION}"));
    let (text, redacted_lines) = redacted(record, secrets_by_length);
    fs::write(&kept_record, text)?;
    eprintln!(
        "run record kept at {}: {redacted_lines} lines redacted",
        kept_record.display()
    );
    Ok(kept_record)
}

/// Writes beside the member's run record in the directory, when the member's stop ended in failure, a file named for the member saying so, with how the stop ended and what it printed, each secret replaced by the redaction marker; nothing when the stop succeeded.
pub fn keep_stop_failure(
    directory: &Path,
    member_name: &str,
    stop: ExitStatus,
    printed_by_the_stop: &[u8],
    secrets_by_length: &[String],
) -> io::Result<()> {
    if stop.success() {
        return Ok(());
    }
    let mark = format!(
        "the stop of {member_name} ended with {stop}, so it may have run on while its records were taken; the stop printed:\n{}",
        String::from_utf8_lossy(printed_by_the_stop)
    );
    let (text, _redacted_lines) = redacted(&mark, secrets_by_length);
    fs::write(
        directory.join(format!("{member_name}.{STOP_FAILED_EXTENSION}")),
        text,
    )
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
    file.extension()
        .is_some_and(|extension| extension == SECRET_KEY_EXTENSION)
        || file.ancestors().skip(1).any(|directory| {
            directory
                .extension()
                .is_some_and(|extension| extension == CERTIFICATE_CACHE_EXTENSION)
        })
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
    /// The process id a test marks a failed end under, no process of its own.
    const PLANTED_PROCESS_ID: u32 = 4242;
    /// The failure a test plants as the cause of a failed end.
    const PLANTED_END_FAILURE: &str = "planted failure of the wait";

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
        let cards = machine.directories().cards_directory(NETWORK).unwrap();
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
            (cards.join("amy.yaml"), "card of amy\n"),
        ];
        let secret_key_file = cards.join("amy.key");
        for (file, content) in &written {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, content).unwrap();
        }
        fs::write(&secret_key_file, "secret key of amy\n").unwrap();
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

        let kept: BTreeSet<(PathBuf, String)> = files_beneath(&kept_in)
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

        let kept: BTreeSet<PathBuf> = files_beneath(&kept_in).into_iter().collect();
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
        let kept: BTreeSet<(PathBuf, String)> = files_beneath(&kept_in)
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
        owned_process::mark_end_failure(
            &machine.root,
            FAILED_LABEL,
            PLANTED_PROCESS_ID,
            &io::Error::other(PLANTED_END_FAILURE),
        );
        let kept_machine = kept_in.join(machine.name());

        drop(machine);

        let kept: BTreeSet<PathBuf> = files_beneath(&kept_in).into_iter().collect();
        let mark = kept_machine.join(format!(
            "{FAILED_LABEL}-{PLANTED_PROCESS_ID}.{END_FAILED_EXTENSION}"
        ));
        assert_eq!(kept, BTreeSet::from([mark.clone()]));
        let marked = fs::read_to_string(&mark).unwrap();
        assert!(
            marked.contains(FAILED_LABEL)
                && marked.contains(&PLANTED_PROCESS_ID.to_string())
                && marked.contains(PLANTED_END_FAILURE),
            "{marked}"
        );
        remove_scratch::remove_scratch(&kept_in).unwrap();
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
        let secrets = [secret.to_owned()];
        let failed_stop = Command::new(EXITS_IN_FAILURE).status().unwrap();
        let clean_stop = Command::new(EXITS_IN_SUCCESS).status().unwrap();
        let printed_by_the_failed_stop = format!("ssh: the server refused {secret}\n");

        keep_run_record(&kept_in, "amy", "record of amy\n", &secrets).unwrap();
        keep_stop_failure(
            &kept_in,
            "amy",
            failed_stop,
            printed_by_the_failed_stop.as_bytes(),
            &secrets,
        )
        .unwrap();
        keep_run_record(&kept_in, "bob", "record of bob\n", &secrets).unwrap();
        keep_stop_failure(
            &kept_in,
            "bob",
            clean_stop,
            b"bob printed this while stopping\n",
            &secrets,
        )
        .unwrap();

        let kept: BTreeSet<PathBuf> = files_beneath(&kept_in).into_iter().collect();
        assert_eq!(
            kept,
            BTreeSet::from([
                kept_in.join("amy.stderr"),
                kept_in.join("amy.stop-failed"),
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
}
