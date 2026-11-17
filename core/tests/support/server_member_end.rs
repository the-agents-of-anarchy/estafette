use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;

use super::fresh_directory::fresh_directory_in;
use super::remove_scratch::remove_scratch;
use super::trace_reader::timeline_in;
use super::{
    KeptAs, KeptSecrets, fail_unless_panicking, files_beneath_for_keeping, fresh_kept_directory,
    keep_file, keep_mark, keep_run_record, keep_stop_failure, naming,
};

/// The directory a member's fetched trace files are kept in, inside the directory its records are kept in.
const KEPT_TRACE_FILES_DIRECTORY: &str = "traces";
/// The extension of the file kept beside a member's records when its run record could not be read, so a reader knows why none is kept.
const RUN_RECORD_READ_FAILED_EXTENSION: &str = "run-record-read-failed";
/// The extension of the file kept beside the trace files' directory, named as it is, when the fetch failed or a fetched file could not be kept, so an absent, empty or partial directory is not read as a whole fetch.
const FETCH_FAILED_EXTENSION: &str = "fetch-failed";
/// What begins the name of the scratch directory, outside every kept directory, a member's trace files are fetched into before they are kept or read.
const STAGING_PREFIX: &str = "ea-fetched";
/// The name, inside that scratch directory, of the directory the recipe fetches into.
const STAGED_DIRECTORY: &str = "fetched";
/// What a failing test prints in place of a member's timeline when its trace files could not be fetched.
const TRACE_FILES_NOT_FETCHED: &str =
    "its trace files were not fetched, so what it recorded is unknown";

/// A step of the end of a member serving on a server, each a recipe run on that server.
#[derive(Clone, Copy)]
pub enum ServerMemberStep<'into> {
    /// Stops the member cleanly, waiting out its departure.
    Stop,
    /// Prints the member's run record.
    PrintRunRecord,
    /// Fetches both trace files of each of the member's runs into the directory.
    FetchTraceFiles(&'into Path),
}

/// Whether the test running a member's end had already failed when the end began, so the end reads the member's timeline for its print.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestSoFar {
    /// The test had failed: its thread was panicking.
    Failing,
    /// The test had not failed.
    NotFailing,
}

/// The timeline a failing test prints for the member, none when the test was not failing, and each failure of the end's steps in the order they ran.
pub struct ServerMemberEnd {
    pub timeline: Option<String>,
    pub failures: Vec<String>,
}

/// A fetch of a member's trace files into a scratch directory outside every kept directory.
enum StagedFetch {
    /// No scratch directory could be made, so nothing was fetched; why.
    NotStaged(String),
    /// Fetched beneath the scratch directory, whole unless the fetch's failure is named.
    Staged {
        holder: PathBuf,
        failure: Option<String>,
    },
}

impl StagedFetch {
    /// Why the fetch is not whole, none when it is.
    fn failure(&self) -> Option<&str> {
        match self {
            Self::NotStaged(failure) => Some(failure),
            Self::Staged { failure, .. } => failure.as_deref(),
        }
    }

    /// The directory the files were fetched into, whole or not, none when nothing could be fetched.
    fn fetched_into(&self) -> Option<PathBuf> {
        match self {
            Self::NotStaged(_failure) => None,
            Self::Staged { holder, .. } => Some(holder.join(STAGED_DIRECTORY)),
        }
    }

    /// The directory the files of a whole fetch are in, none unless the fetch was whole.
    fn whole(&self) -> Option<PathBuf> {
        self.failure()
            .map_or_else(|| self.fetched_into(), |_failure| None)
    }

    /// Removes the scratch directory with what was fetched into it, nothing when none was made.
    fn removed(&self) -> io::Result<()> {
        match self {
            Self::NotStaged(_failure) => Ok(()),
            Self::Staged { holder, .. } => remove_scratch(holder),
        }
    }
}

/// Stops the member on the server through the runner and, when the test keeps records and the member's secrets were read, keeps a mark of a failed stop, its run record and its trace files through the writer of kept files in a directory of its own named for the server and the member, the trace files fetched first into a scratch directory outside it; when the test is failing, reads the member's timeline from that fetch; each step runs whatever an earlier one did, and every failure, a failed stop and secrets that could not be read included, is answered once all have run.
pub fn ended_server_member(
    member: &str,
    server: &str,
    kept_in: Option<&Path>,
    secrets: &io::Result<KeptSecrets>,
    test: TestSoFar,
    run: impl Fn(ServerMemberStep<'_>) -> io::Result<Output>,
) -> ServerMemberEnd {
    let stop = run(ServerMemberStep::Stop);
    let mut failures = Vec::new();
    if let Err(failure) = succeeded(&stop) {
        failures.push(format!(
            "{member} on {server} did not stop cleanly: {failure}"
        ));
    }
    let kept = kept_directory(member, server, kept_in, secrets, &mut failures);
    if let Some((kept, secrets)) = &kept {
        if let Err(failure) = keep_stop_failure(kept, member, &stop, secrets) {
            failures.push(format!("failed stop of {member} not marked: {failure}"));
        }
        keep_printed_run_record(member, server, kept, secrets, &run, &mut failures);
    }
    let failing = test == TestSoFar::Failing;
    if kept.is_none() && !failing {
        return ServerMemberEnd {
            timeline: None,
            failures,
        };
    }
    let staged = staged_fetch(member, server, &run);
    if let Some(failure) = staged.failure() {
        failures.push(format!(
            "trace files of {member} on {server} not fetched: {failure}"
        ));
    }
    if let Some((kept, secrets)) = &kept {
        keep_staged_trace_files(member, server, kept, secrets, &staged, &mut failures);
    }
    let timeline = failing.then(|| timeline_of_fetched(staged.whole().as_deref()));
    if let Err(failure) = staged.removed() {
        failures.push(failure.to_string());
    }
    ServerMemberEnd { timeline, failures }
}

/// The name a member serving on a server keeps its records under, beside the machines' directories: the server's, then the member's.
fn kept_name(server: &str, member: &str) -> String {
    format!("{server}-{member}")
}

/// The directory the member's records are kept in, made beneath the test's, with the secrets they are searched for; none when the test keeps none, or, adding the failure, when the secrets could not be read or the directory could not be made.
fn kept_directory<'secrets>(
    member: &str,
    server: &str,
    kept_in: Option<&Path>,
    secrets: &'secrets io::Result<KeptSecrets>,
    failures: &mut Vec<String>,
) -> Option<(PathBuf, &'secrets KeptSecrets)> {
    let directory = kept_in?;
    let secrets = match secrets {
        Ok(secrets) => secrets,
        Err(unread) => {
            failures.push(format!(
                "nothing of {member} on {server} kept, since the secrets to search it for could not be read: {unread}"
            ));
            return None;
        }
    };
    match fresh_kept_directory(directory, &kept_name(server, member)) {
        Ok(kept) => Some((kept, secrets)),
        Err(failure) => {
            failures.push(format!(
                "records of {member} not kept in {}: {failure}",
                directory.display()
            ));
            None
        }
    }
}

/// Keeps in the directory the member's run record as the runner prints it, or, when it cannot be read, a mark saying so, adding each failure.
fn keep_printed_run_record(
    member: &str,
    server: &str,
    kept: &Path,
    secrets: &KeptSecrets,
    run: &impl Fn(ServerMemberStep<'_>) -> io::Result<Output>,
    failures: &mut Vec<String>,
) {
    let printed = run(ServerMemberStep::PrintRunRecord);
    match succeeded(&printed) {
        Ok(output) => {
            if let Err(failure) = keep_run_record(kept, member, &output.stdout, secrets) {
                failures.push(format!("run record of {member} not kept: {failure}"));
            }
        }
        Err(failure) => {
            failures.push(format!(
                "run record of {member} on {server} not read: {failure}"
            ));
            let mark = format!(
                "the run record of {member} on {server} was not read, so none is kept here; the read ended with {failure}"
            );
            let mark_name = format!("{member}.{RUN_RECORD_READ_FAILED_EXTENSION}");
            if let Err(unmarked) = keep_mark(kept, &mark_name, &mark, secrets) {
                failures.push(format!(
                    "failed read of the run record of {member} not marked: {unmarked}"
                ));
            }
        }
    }
}

/// Fetches the member's trace files into a fresh scratch directory outside every kept directory.
fn staged_fetch(
    member: &str,
    server: &str,
    run: &impl Fn(ServerMemberStep<'_>) -> io::Result<Output>,
) -> StagedFetch {
    match fresh_directory_in(
        &std::env::temp_dir(),
        &format!("{STAGING_PREFIX}-{}-{server}-{member}", std::process::id()),
    ) {
        Ok(holder) => {
            let fetched = run(ServerMemberStep::FetchTraceFiles(
                &holder.join(STAGED_DIRECTORY),
            ));
            StagedFetch::Staged {
                failure: succeeded(&fetched).err(),
                holder,
            }
        }
        Err(failure) => StagedFetch::NotStaged(failure.to_string()),
    }
}

/// Keeps every file of the staged fetch in the trace files' directory inside the directory through the writer of kept files, adding a failure for each file that cannot be read or kept, and marks beside that directory, with each failure, a fetch that is not whole or a file not kept.
fn keep_staged_trace_files(
    member: &str,
    server: &str,
    kept: &Path,
    secrets: &KeptSecrets,
    staged: &StagedFetch,
    failures: &mut Vec<String>,
) {
    let kept_trace_files = kept.join(KEPT_TRACE_FILES_DIRECTORY);
    let unkept: Vec<String> = staged.fetched_into().map_or_else(Vec::new, |fetched_into| {
        files_beneath_for_keeping(&fetched_into)
            .into_iter()
            .filter_map(|listed| {
                listed
                    .and_then(|file| {
                        let in_fetch = file.strip_prefix(&fetched_into).unwrap_or(&file);
                        fs::read(&file)
                            .and_then(|content| {
                                keep_file(
                                    secrets,
                                    KeptAs::Evidence,
                                    &kept_trace_files.join(in_fetch),
                                    &content,
                                )
                            })
                            .map_err(|failure| naming(&file, &failure))
                    })
                    .err()
            })
            .map(|failure| failure.to_string())
            .collect()
    });
    for failure in &unkept {
        failures.push(format!("a trace file of {member} not kept: {failure}"));
    }
    let not_whole: Vec<&str> = staged
        .failure()
        .into_iter()
        .chain(unkept.iter().map(String::as_str))
        .collect();
    if not_whole.is_empty() {
        eprintln!(
            "trace files of {member} kept at {}",
            kept_trace_files.display()
        );
        return;
    }
    let mark = format!(
        "the trace files of {member} on {server} were not kept whole, so {KEPT_TRACE_FILES_DIRECTORY}/ beside this file may be absent, empty or partial: {}",
        not_whole.join("; ")
    );
    let mark_name = format!("{KEPT_TRACE_FILES_DIRECTORY}.{FETCH_FAILED_EXTENSION}");
    if let Err(unmarked) = keep_mark(kept, &mark_name, &mark, secrets) {
        failures.push(format!(
            "failed fetch of the trace files of {member} not marked: {unmarked}"
        ));
    }
}

/// The member's timeline read from the directory its trace files were fetched into, or, when they were not fetched, that its timeline is unknown.
fn timeline_of_fetched(fetched_into: Option<&Path>) -> String {
    fetched_into.map_or_else(|| TRACE_FILES_NOT_FETCHED.to_owned(), timeline_in)
}

/// The output of a recipe run that succeeded, or why it failed: its exit status and standard error, or the error that kept it from running.
pub fn succeeded(ran: &io::Result<Output>) -> Result<&Output, String> {
    match ran {
        Ok(output) if output.status.success() => Ok(output),
        Ok(output) => Err(format!(
            "{}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(failure) => Err(failure.to_string()),
    }
}

/// Fails a test that was not failing, saying what was left undone and why, when a recipe run from a drop failed or could not run, and prints that instead on a test already failing, so no drop panics on a thread already panicking.
pub fn fail_unless_succeeded(ran: &io::Result<Output>, left_undone: &str) {
    if let Err(failure) = succeeded(ran) {
        fail_unless_panicking(&format!("{left_undone}: {failure}"));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::*;
    use crate::scratch_machine::fresh_directory::fresh_directory_in;
    use crate::scratch_machine::remove_scratch::remove_scratch;
    use crate::scratch_machine::trace_reader::NO_EVENT_RECORDED;
    use crate::scratch_machine::{
        INVITATION_VARIABLE, REDACTED, REDACTED_NOTE_EXTENSION, STOP_FAILED_EXTENSION,
    };

    /// The card a test's member serves.
    const MEMBER: &str = "amy";
    /// The server label a test's member serves on.
    const SERVER: &str = "lab-server";
    /// The raw wait status of a recipe that exited with code 1.
    const EXITED_WITH_ONE: i32 = 1 << 8;
    /// What a failed stop of a test's member printed.
    const STOP_REFUSAL: &str = "ssh: the stop was refused";
    /// What a failed read of a test's member's run record printed.
    const RUN_RECORD_REFUSAL: &str = "ssh: the run record was refused";
    /// The file a test's stand-in fetch writes as a trace file.
    const FETCHED_TRACE_FILE: &str = "20261117T120000Z-1111111111111111-estafette.events.jsonl";
    /// A fetched trace file a test's stand-in writes holding the planted invitation.
    const FETCHED_NAMING_THE_INVITATION: &str =
        "20261117T120000Z-1111111111111111-estafette.raw.jsonl";
    /// A fetched trace file a test's stand-in writes holding the planted token.
    const FETCHED_NAMING_THE_TOKEN: &str =
        "20261117T120001Z-2222222222222222-estafette.events.jsonl";
    /// A fetched trace file a test's stand-in writes holding neither, in bytes that are no text.
    const FETCHED_NAMING_NEITHER: &str = "20261117T120001Z-2222222222222222-estafette.raw.jsonl";
    /// An invitation a test plants among a member's secrets and in what is fetched of it, no real one.
    const PLANTED_INVITATION: &str = "estafette-invite-1:plantedinvitation0123456789";
    /// A token a test plants among a member's secrets and in what is fetched of it, no real one.
    const PLANTED_TOKEN: &str = "plantedtoken0123456789abcdef";
    /// A file a test makes where a directory is wanted, so no directory can be made beneath it.
    const NOT_A_DIRECTORY: &str = "not-a-directory";
    /// The directory a test asks a member's records be kept in, beneath that file.
    const KEPT_BENEATH_NOT_A_DIRECTORY: &str = "kept";
    /// Why a test's recipe could not run.
    const RECIPE_UNRUNNABLE: &str = "the task runner could not be started";
    /// What a test's drop says its recipe left undone.
    const LEFT_UNDONE: &str = "the UDP drops on lab-server were not deleted";
    /// The failure a test raises before a drop reports its recipe.
    const FIRST_FAILURE: &str = "the test failed first";
    /// The owner key a test's member's network configuration names, no real one.
    const PLANTED_OWNER: &str = "0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e";
    /// What a test writes as the network configuration a member wrote to its server, naming no secret.
    const CONFIGURATION_NAMING_NO_SECRET: &str = "owner = \"0e0e0e0e\"\n";
    /// The field of a network configuration naming its secret, as spec 3.3 names it.
    const NETWORK_SECRET_FIELD: &str = "secret";
    /// The mode of a fetched file nobody can read, so keeping it fails.
    const UNREADABLE: u32 = 0o000;

    /// The network configuration a test's member wrote to its server, naming the secret.
    fn configuration_naming(secret: &str) -> String {
        format!("{NETWORK_SECRET_FIELD} = \"{secret}\"\nowner = \"{PLANTED_OWNER}\"\n")
    }

    /// The secrets of a test's member: the planted token as its network's secret, and the planted invitation.
    fn secrets_of_the_member() -> io::Result<KeptSecrets> {
        KeptSecrets::of_server_member(
            &configuration_naming(PLANTED_TOKEN),
            Some(PLANTED_INVITATION),
        )
    }

    /// Ends a test's member whose stop failed, whose run record prints and whose fetch writes one trace file, keeping into a fresh directory with the secrets handed in, and answers its end and every file kept, then removes what was kept.
    fn ended_with_stop_failing(
        tag: &str,
        secrets: &io::Result<KeptSecrets>,
    ) -> (ServerMemberEnd, BTreeSet<PathBuf>) {
        let kept_in = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-server-member-{tag}-test-{}", std::process::id()),
        )
        .unwrap();
        let stand_in_for_the_recipes = |step: ServerMemberStep<'_>| match step {
            ServerMemberStep::Stop => Ok(failed_run(STOP_REFUSAL)),
            ServerMemberStep::PrintRunRecord => Ok(succeeded_run("record of amy\n")),
            ServerMemberStep::FetchTraceFiles(into) => {
                fs::create_dir_all(into).unwrap();
                fs::write(into.join(FETCHED_TRACE_FILE), "").unwrap();
                Ok(succeeded_run(""))
            }
        };

        let ended = ended_server_member(
            MEMBER,
            SERVER,
            Some(&kept_in),
            secrets,
            TestSoFar::NotFailing,
            stand_in_for_the_recipes,
        );

        let kept = fs::read_dir(&kept_in)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        remove_scratch(&kept_in).unwrap();
        (ended, kept)
    }

    /// Requires that the end answered the failure the secrets were built with and the failed stop, having kept nothing.
    fn require_nothing_kept_and_each_failure_answered(
        secrets: &io::Result<KeptSecrets>,
        ended: &ServerMemberEnd,
        kept: &BTreeSet<PathBuf>,
    ) {
        let Err(unread) = secrets else {
            panic!("the member's secrets were built")
        };
        assert_eq!(kept, &BTreeSet::new());
        for answered in [unread.to_string(), STOP_REFUSAL.to_owned()] {
            assert!(
                ended
                    .failures
                    .iter()
                    .any(|failure| failure.contains(&answered)),
                "{answered} is not answered: {:?}",
                ended.failures
            );
        }
    }

    #[test]
    fn observability_decision_16_a_server_member_whose_network_configuration_names_no_secret_keeps_nothing_and_answers_that_beside_its_failed_stop()
     {
        let secrets =
            KeptSecrets::of_server_member(CONFIGURATION_NAMING_NO_SECRET, Some(PLANTED_INVITATION));

        let (ended, kept) = ended_with_stop_failing("no-secret", &secrets);

        require_nothing_kept_and_each_failure_answered(&secrets, &ended, &kept);
        assert!(
            ended
                .failures
                .iter()
                .any(|failure| failure.contains(NETWORK_SECRET_FIELD)),
            "{:?}",
            ended.failures
        );
    }

    #[test]
    fn observability_decision_16_a_server_member_started_with_the_invitation_unset_keeps_nothing_and_answers_that_beside_its_failed_stop()
     {
        let secrets = KeptSecrets::of_server_member(&configuration_naming(PLANTED_TOKEN), None);

        let (ended, kept) = ended_with_stop_failing("invitation-unset", &secrets);

        require_nothing_kept_and_each_failure_answered(&secrets, &ended, &kept);
        assert!(
            ended
                .failures
                .iter()
                .any(|failure| failure.contains(INVITATION_VARIABLE)),
            "{:?}",
            ended.failures
        );
    }

    /// A recipe run that exited with code 1 having printed the text on standard error.
    fn failed_run(printed: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(EXITED_WITH_ONE),
            stdout: Vec::new(),
            stderr: printed.as_bytes().to_vec(),
        }
    }

    /// A recipe run that exited in success having printed the text on standard output.
    fn succeeded_run(printed: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(0),
            stdout: printed.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn observability_decision_16_a_server_members_fetched_trace_file_holding_its_invitation_or_a_token_is_kept_redacted_with_a_note_beside_it_and_any_other_byte_for_byte()
     {
        let kept_in = fresh_directory_in(
            &std::env::temp_dir(),
            &format!(
                "estafette-server-member-redacted-test-{}",
                std::process::id()
            ),
        )
        .unwrap();
        let neither_bytes = [b'n', b'o', 0xff, 0x00, b'\n'];
        let stand_in_for_the_recipes = |step: ServerMemberStep<'_>| match step {
            ServerMemberStep::Stop => Ok(succeeded_run("")),
            ServerMemberStep::PrintRunRecord => Ok(succeeded_run("record of amy\n")),
            ServerMemberStep::FetchTraceFiles(into) => {
                fs::create_dir_all(into).unwrap();
                fs::write(
                    into.join(FETCHED_NAMING_THE_INVITATION),
                    format!("first line\njoined by {PLANTED_INVITATION} here\n"),
                )
                .unwrap();
                fs::write(
                    into.join(FETCHED_NAMING_THE_TOKEN),
                    format!("key {PLANTED_TOKEN} seen\n"),
                )
                .unwrap();
                fs::write(into.join(FETCHED_NAMING_NEITHER), neither_bytes).unwrap();
                Ok(succeeded_run(""))
            }
        };

        let ended = ended_server_member(
            MEMBER,
            SERVER,
            Some(&kept_in),
            &secrets_of_the_member(),
            TestSoFar::NotFailing,
            stand_in_for_the_recipes,
        );

        let kept_traces = kept_in
            .join(kept_name(SERVER, MEMBER))
            .join(KEPT_TRACE_FILES_DIRECTORY);
        let note_of = |name: &str| kept_traces.join(format!("{name}.{REDACTED_NOTE_EXTENSION}"));
        assert!(ended.failures.is_empty(), "{:?}", ended.failures);
        let kept: BTreeSet<PathBuf> = fs::read_dir(&kept_traces)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(
            kept,
            BTreeSet::from([
                kept_traces.join(FETCHED_NAMING_THE_INVITATION),
                note_of(FETCHED_NAMING_THE_INVITATION),
                kept_traces.join(FETCHED_NAMING_THE_TOKEN),
                note_of(FETCHED_NAMING_THE_TOKEN),
                kept_traces.join(FETCHED_NAMING_NEITHER),
            ])
        );
        assert_eq!(
            fs::read_to_string(kept_traces.join(FETCHED_NAMING_THE_INVITATION)).unwrap(),
            format!("first line\njoined by {REDACTED} here\n")
        );
        assert_eq!(
            fs::read_to_string(kept_traces.join(FETCHED_NAMING_THE_TOKEN)).unwrap(),
            format!("key {REDACTED} seen\n")
        );
        assert_eq!(
            fs::read(kept_traces.join(FETCHED_NAMING_NEITHER)).unwrap(),
            neither_bytes
        );
        for name in [FETCHED_NAMING_THE_INVITATION, FETCHED_NAMING_THE_TOKEN] {
            let note = fs::read_to_string(note_of(name)).unwrap();
            assert!(note.contains(name), "{note}");
        }
        remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn observability_decision_16_a_server_member_whose_stop_and_run_record_read_failed_still_has_its_trace_files_fetched_and_each_failure_answered()
     {
        let kept_in = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-server-member-end-test-{}", std::process::id()),
        )
        .unwrap();
        let steps_run = RefCell::new(Vec::new());
        let stand_in_for_the_recipes = |step: ServerMemberStep<'_>| match step {
            ServerMemberStep::Stop => {
                steps_run.borrow_mut().push("stop".to_owned());
                Ok(failed_run(STOP_REFUSAL))
            }
            ServerMemberStep::PrintRunRecord => {
                steps_run.borrow_mut().push("print run record".to_owned());
                Ok(failed_run(RUN_RECORD_REFUSAL))
            }
            ServerMemberStep::FetchTraceFiles(into) => {
                steps_run.borrow_mut().push("fetch trace files".to_owned());
                fs::create_dir_all(into).unwrap();
                fs::write(into.join(FETCHED_TRACE_FILE), "").unwrap();
                Ok(Output {
                    status: ExitStatus::from_raw(0),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                })
            }
        };

        let ended = ended_server_member(
            MEMBER,
            SERVER,
            Some(&kept_in),
            &secrets_of_the_member(),
            TestSoFar::Failing,
            stand_in_for_the_recipes,
        );

        let kept = kept_in.join(kept_name(SERVER, MEMBER));
        let kept_traces = kept.join(KEPT_TRACE_FILES_DIRECTORY);
        assert_eq!(
            steps_run.into_inner(),
            ["stop", "print run record", "fetch trace files"]
        );
        assert_eq!(ended.timeline.as_deref(), Some(NO_EVENT_RECORDED));
        assert_eq!(ended.failures.len(), 2, "{:?}", ended.failures);
        assert!(
            ended.failures[0].contains(MEMBER)
                && ended.failures[0].contains(SERVER)
                && ended.failures[0].contains(STOP_REFUSAL),
            "{:?}",
            ended.failures
        );
        assert!(
            ended.failures[1].contains(MEMBER)
                && ended.failures[1].contains(SERVER)
                && ended.failures[1].contains(RUN_RECORD_REFUSAL),
            "{:?}",
            ended.failures
        );
        let kept_files: BTreeSet<PathBuf> = fs::read_dir(&kept)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(
            kept_files,
            BTreeSet::from([
                kept.join(format!("{MEMBER}.{STOP_FAILED_EXTENSION}")),
                kept.join(format!("{MEMBER}.{RUN_RECORD_READ_FAILED_EXTENSION}")),
                kept_traces
            ])
        );
        remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn observability_decision_16_a_server_member_whose_stop_failed_fails_a_test_that_was_not_failing_once_every_fetch_has_run_beside_its_mark()
     {
        let kept_in = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-server-member-stop-test-{}", std::process::id()),
        )
        .unwrap();
        let steps_run = RefCell::new(Vec::new());
        let stand_in_for_the_recipes = |step: ServerMemberStep<'_>| match step {
            ServerMemberStep::Stop => {
                steps_run.borrow_mut().push("stop");
                Ok(failed_run(STOP_REFUSAL))
            }
            ServerMemberStep::PrintRunRecord => {
                steps_run.borrow_mut().push("print run record");
                Ok(succeeded_run("record of amy\n"))
            }
            ServerMemberStep::FetchTraceFiles(into) => {
                steps_run.borrow_mut().push("fetch trace files");
                fs::create_dir_all(into).unwrap();
                Ok(succeeded_run(""))
            }
        };

        let ended = ended_server_member(
            MEMBER,
            SERVER,
            Some(&kept_in),
            &secrets_of_the_member(),
            TestSoFar::NotFailing,
            stand_in_for_the_recipes,
        );

        let kept = kept_in.join(kept_name(SERVER, MEMBER));
        assert_eq!(
            steps_run.into_inner(),
            ["stop", "print run record", "fetch trace files"]
        );
        assert_eq!(ended.timeline, None);
        assert_eq!(ended.failures.len(), 1, "{:?}", ended.failures);
        assert!(
            ended.failures[0].contains(MEMBER)
                && ended.failures[0].contains(SERVER)
                && ended.failures[0].contains(STOP_REFUSAL),
            "{:?}",
            ended.failures
        );
        assert!(
            kept.join(format!("{MEMBER}.{STOP_FAILED_EXTENSION}"))
                .is_file(),
            "no mark of the failed stop beside {}",
            kept.display()
        );
        remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn observability_decision_16_a_failing_test_whose_server_members_kept_directory_cannot_be_made_answers_that_failure_and_still_fetches_its_trace_files_for_the_timeline()
     {
        let scratch = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-server-member-unkept-test-{}", std::process::id()),
        )
        .unwrap();
        let not_a_directory = scratch.join(NOT_A_DIRECTORY);
        fs::write(&not_a_directory, "").unwrap();
        let fetched_into = RefCell::new(Vec::new());
        let steps_run = RefCell::new(Vec::new());
        let stand_in_for_the_recipes = |step: ServerMemberStep<'_>| match step {
            ServerMemberStep::Stop => {
                steps_run.borrow_mut().push("stop");
                Ok(succeeded_run(""))
            }
            ServerMemberStep::PrintRunRecord => {
                steps_run.borrow_mut().push("print run record");
                Ok(succeeded_run("record of amy\n"))
            }
            ServerMemberStep::FetchTraceFiles(into) => {
                steps_run.borrow_mut().push("fetch trace files");
                fetched_into.borrow_mut().push(into.to_path_buf());
                fs::create_dir_all(into).unwrap();
                Ok(succeeded_run(""))
            }
        };

        let ended = ended_server_member(
            MEMBER,
            SERVER,
            Some(&not_a_directory.join(KEPT_BENEATH_NOT_A_DIRECTORY)),
            &secrets_of_the_member(),
            TestSoFar::Failing,
            stand_in_for_the_recipes,
        );

        assert_eq!(steps_run.into_inner(), ["stop", "fetch trace files"]);
        assert_eq!(ended.timeline.as_deref(), Some(NO_EVENT_RECORDED));
        assert_eq!(ended.failures.len(), 1, "{:?}", ended.failures);
        assert!(
            ended.failures[0].contains(MEMBER)
                && ended.failures[0].contains(&not_a_directory.display().to_string()),
            "{:?}",
            ended.failures
        );
        for fetched in fetched_into.into_inner() {
            assert!(
                !fetched.exists(),
                "{} was not removed once read",
                fetched.display()
            );
        }
        remove_scratch(&scratch).unwrap();
    }

    #[test]
    fn observability_decision_16_a_server_members_failed_run_record_read_and_failed_fetch_are_each_marked_beside_what_is_kept_so_a_partial_fetch_cannot_be_read_as_whole()
     {
        let kept_in = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-server-member-marked-test-{}", std::process::id()),
        )
        .unwrap();
        let fetch_refusal = format!("ssh: the stream broke after {PLANTED_TOKEN}");
        let stand_in_for_the_recipes = |step: ServerMemberStep<'_>| match step {
            ServerMemberStep::Stop => Ok(succeeded_run("")),
            ServerMemberStep::PrintRunRecord => Ok(failed_run(RUN_RECORD_REFUSAL)),
            ServerMemberStep::FetchTraceFiles(into) => {
                fs::create_dir_all(into).unwrap();
                fs::write(into.join(FETCHED_TRACE_FILE), "part of a line").unwrap();
                Ok(failed_run(&fetch_refusal))
            }
        };

        let ended = ended_server_member(
            MEMBER,
            SERVER,
            Some(&kept_in),
            &secrets_of_the_member(),
            TestSoFar::NotFailing,
            stand_in_for_the_recipes,
        );

        let kept = kept_in.join(kept_name(SERVER, MEMBER));
        let run_record_mark = kept.join(format!("{MEMBER}.{RUN_RECORD_READ_FAILED_EXTENSION}"));
        let fetch_mark = kept.join(format!(
            "{KEPT_TRACE_FILES_DIRECTORY}.{FETCH_FAILED_EXTENSION}"
        ));
        assert_eq!(ended.failures.len(), 2, "{:?}", ended.failures);
        let kept_files: BTreeSet<PathBuf> = fs::read_dir(&kept)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(
            kept_files,
            BTreeSet::from([
                run_record_mark.clone(),
                kept.join(KEPT_TRACE_FILES_DIRECTORY),
                fetch_mark.clone(),
                kept.join(format!(
                    "{KEPT_TRACE_FILES_DIRECTORY}.{FETCH_FAILED_EXTENSION}.{REDACTED_NOTE_EXTENSION}"
                )),
            ])
        );
        let run_record_marked = fs::read_to_string(&run_record_mark).unwrap();
        assert!(
            run_record_marked.contains(MEMBER) && run_record_marked.contains(RUN_RECORD_REFUSAL),
            "{run_record_marked}"
        );
        let fetch_marked = fs::read_to_string(&fetch_mark).unwrap();
        assert!(
            fetch_marked.contains(MEMBER)
                && fetch_marked.contains(&format!("ssh: the stream broke after {REDACTED}"))
                && !fetch_marked.contains(PLANTED_TOKEN),
            "{fetch_marked}"
        );
        remove_scratch(&kept_in).unwrap();
    }

    #[test]
    fn observability_decision_16_a_server_members_fetched_file_that_cannot_be_read_is_not_kept_but_marked_and_answered_and_the_others_are_kept()
     {
        let kept_in = fresh_directory_in(
            &std::env::temp_dir(),
            &format!(
                "estafette-server-member-unread-fetched-test-{}",
                std::process::id()
            ),
        )
        .unwrap();
        let stand_in_for_the_recipes = |step: ServerMemberStep<'_>| match step {
            ServerMemberStep::Stop => Ok(succeeded_run("")),
            ServerMemberStep::PrintRunRecord => Ok(succeeded_run("record of amy\n")),
            ServerMemberStep::FetchTraceFiles(into) => {
                fs::create_dir_all(into).unwrap();
                fs::write(into.join(FETCHED_TRACE_FILE), "").unwrap();
                let unreadable = into.join(FETCHED_NAMING_THE_TOKEN);
                fs::write(&unreadable, format!("key {PLANTED_TOKEN} seen\n")).unwrap();
                fs::set_permissions(&unreadable, fs::Permissions::from_mode(UNREADABLE)).unwrap();
                Ok(succeeded_run(""))
            }
        };

        let ended = ended_server_member(
            MEMBER,
            SERVER,
            Some(&kept_in),
            &secrets_of_the_member(),
            TestSoFar::NotFailing,
            stand_in_for_the_recipes,
        );

        let kept = kept_in.join(kept_name(SERVER, MEMBER));
        let kept_traces: BTreeSet<PathBuf> = fs::read_dir(kept.join(KEPT_TRACE_FILES_DIRECTORY))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        let fetch_mark = fs::read_to_string(kept.join(format!(
            "{KEPT_TRACE_FILES_DIRECTORY}.{FETCH_FAILED_EXTENSION}"
        )))
        .unwrap_or_default();
        remove_scratch(&kept_in).unwrap();
        assert_eq!(
            kept_traces,
            BTreeSet::from([kept
                .join(KEPT_TRACE_FILES_DIRECTORY)
                .join(FETCHED_TRACE_FILE)])
        );
        assert!(
            fetch_mark.contains(FETCHED_NAMING_THE_TOKEN),
            "the fetch mark names no unread file: {fetch_mark}"
        );
        assert_eq!(ended.failures.len(), 1, "{:?}", ended.failures);
        assert!(
            ended.failures[0].contains(FETCHED_NAMING_THE_TOKEN),
            "{:?}",
            ended.failures
        );
    }

    #[test]
    fn observability_decision_15_a_member_whose_trace_files_were_not_fetched_is_printed_as_not_fetched_not_as_recording_nothing()
     {
        let fetched_into = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("server-member-{}-fetched-into", std::process::id()),
        )
        .unwrap();

        let not_fetched = timeline_of_fetched(None);
        let fetched_with_no_event = timeline_of_fetched(Some(&fetched_into));

        assert_eq!(not_fetched, TRACE_FILES_NOT_FETCHED);
        assert_eq!(fetched_with_no_event, NO_EVENT_RECORDED);
        remove_scratch(&fetched_into).unwrap();
    }

    /// A value that, when dropped, reports a recipe run from its drop that could not run, as a lab fixture's drop does.
    struct ReportingARecipeThatCouldNotRun;

    impl Drop for ReportingARecipeThatCouldNotRun {
        fn drop(&mut self) {
            fail_unless_succeeded(&Err(io::Error::other(RECIPE_UNRUNNABLE)), LEFT_UNDONE);
        }
    }

    #[test]
    fn observability_decision_16_a_drop_whose_recipe_could_not_run_on_a_test_already_failing_leaves_that_failure_standing_and_panics_no_further()
     {
        let unwound = std::panic::catch_unwind(|| {
            let _reporting = ReportingARecipeThatCouldNotRun;
            panic!("{FIRST_FAILURE}");
        });

        let first = unwound.expect_err("the test did not fail first");
        assert_eq!(
            first.downcast_ref::<String>().map(String::as_str),
            Some(FIRST_FAILURE)
        );
    }

    #[test]
    fn observability_decision_16_a_drop_whose_recipe_failed_fails_a_test_that_was_not_failing_saying_what_was_left_undone_and_why()
     {
        let failed = std::panic::catch_unwind(|| {
            fail_unless_succeeded(&Ok(failed_run(STOP_REFUSAL)), LEFT_UNDONE);
        });

        let failure = failed.expect_err("a failed recipe did not fail the test");
        let message = failure.downcast_ref::<String>().unwrap();
        assert!(
            message.contains(LEFT_UNDONE) && message.contains(STOP_REFUSAL),
            "{message}"
        );
    }
}
