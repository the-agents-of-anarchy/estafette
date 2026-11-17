use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;

use super::fresh_directory::fresh_directory_in;
use super::remove_scratch::remove_scratch;
use super::trace_reader::timeline_in;
use super::{
    fail_unless_panicking, fresh_kept_directory, keep_mark, keep_run_record, keep_stop_failure,
    redact_files_beneath,
};

/// The directory a member's fetched trace files are kept in, inside the directory its records are kept in.
const KEPT_TRACE_FILES_DIRECTORY: &str = "traces";
/// The extension of the file kept beside a member's records when its run record could not be read, so a reader knows why none is kept.
const RUN_RECORD_READ_FAILED_EXTENSION: &str = "run-record-read-failed";
/// The extension of the file kept beside the trace files' directory, named as it is, when the fetch into it failed, so an absent, empty or partial directory is not read as a whole fetch.
const FETCH_FAILED_EXTENSION: &str = "fetch-failed";
/// What begins the name of the scratch directory a failing test fetches a member's trace files into for its timeline alone.
const FETCHED_FOR_THE_PRINT_PREFIX: &str = "ea-timeline";
/// The name, inside that scratch directory, of the directory the recipe fetches into.
const FETCHED_FOR_THE_PRINT_DIRECTORY: &str = "fetched";
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

/// Where the end of a member left its trace files for a failing test's timeline.
enum TraceFilesKept {
    /// Fetched whole into the directory its records are kept in.
    Fetched(PathBuf),
    /// Not fetched whole into that directory.
    NotFetched,
    /// No directory to keep them in: the test keeps no records, or the directory could not be made.
    NowhereToKeep,
}

/// Stops the member on the server through the runner and, when the test keeps records, keeps a mark of a failed stop, its run record and its trace files, each fetched file holding one of its secrets kept redacted with a note beside it, in a directory of its own named as given, then, when the test is failing, reads the member's timeline, from what was kept or else from a fetch of its own; each step runs whatever an earlier one did, and every failure, a failed stop included, is answered once all have run.
pub fn ended_server_member(
    member: &str,
    server: &str,
    kept_as: &str,
    kept_in: Option<&Path>,
    secrets: &[String],
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
    let made = kept_in.map(|directory| (directory, fresh_kept_directory(directory, kept_as)));
    let trace_files = match made {
        None => TraceFilesKept::NowhereToKeep,
        Some((_directory, Ok(kept))) => {
            kept_records(member, server, &kept, &stop, secrets, &run, &mut failures)
        }
        Some((directory, Err(failure))) => {
            failures.push(format!(
                "records of {member} not kept in {}: {failure}",
                directory.display()
            ));
            TraceFilesKept::NowhereToKeep
        }
    };
    let timeline = (test == TestSoFar::Failing)
        .then(|| timeline_for_the_print(member, server, trace_files, &run, &mut failures));
    ServerMemberEnd { timeline, failures }
}

/// Keeps in the directory a mark of a failed stop, the member's run record and its trace files, each fetched file holding one of its secrets kept redacted with a note beside it, adding each failure, and answers where the trace files were left.
fn kept_records(
    member: &str,
    server: &str,
    kept: &Path,
    stop: &io::Result<Output>,
    secrets: &[String],
    run: &impl Fn(ServerMemberStep<'_>) -> io::Result<Output>,
    failures: &mut Vec<String>,
) -> TraceFilesKept {
    if let Err(failure) = keep_stop_failure(kept, member, stop, secrets) {
        failures.push(format!("failed stop of {member} not marked: {failure}"));
    }
    let printed = run(ServerMemberStep::PrintRunRecord);
    match succeeded(&printed) {
        Ok(output) => {
            let record = String::from_utf8_lossy(&output.stdout);
            if let Err(failure) = keep_run_record(kept, member, &record, secrets) {
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
    let kept_trace_files = kept.join(KEPT_TRACE_FILES_DIRECTORY);
    let fetched = run(ServerMemberStep::FetchTraceFiles(&kept_trace_files));
    let fetch_failure = succeeded(&fetched).err();
    if let Some(failure) = &fetch_failure {
        failures.push(format!(
            "trace files of {member} on {server} not fetched: {failure}"
        ));
        let mark = format!(
            "the trace files of {member} on {server} were not fetched whole, so {KEPT_TRACE_FILES_DIRECTORY}/ beside this file may be absent, empty or partial; the fetch ended with {failure}"
        );
        let mark_name = format!("{KEPT_TRACE_FILES_DIRECTORY}.{FETCH_FAILED_EXTENSION}");
        if let Err(unmarked) = keep_mark(kept, &mark_name, &mark, secrets) {
            failures.push(format!(
                "failed fetch of the trace files of {member} not marked: {unmarked}"
            ));
        }
    }
    for failure in redact_files_beneath(&kept_trace_files, secrets) {
        failures.push(format!(
            "a trace file of {member} not searched for secrets: {failure}"
        ));
    }
    if fetch_failure.is_some() {
        return TraceFilesKept::NotFetched;
    }
    eprintln!(
        "trace files of {member} kept at {}",
        kept_trace_files.display()
    );
    TraceFilesKept::Fetched(kept_trace_files)
}

/// The member's timeline for a failing test's print: read from the trace files kept, unknown when they were not fetched, or read from a fetch into a scratch directory when there was nowhere to keep them, adding each failure.
fn timeline_for_the_print(
    member: &str,
    server: &str,
    trace_files: TraceFilesKept,
    run: &impl Fn(ServerMemberStep<'_>) -> io::Result<Output>,
    failures: &mut Vec<String>,
) -> String {
    match trace_files {
        TraceFilesKept::Fetched(directory) => timeline_of_fetched(Some(&directory)),
        TraceFilesKept::NotFetched => timeline_of_fetched(None),
        TraceFilesKept::NowhereToKeep => {
            timeline_fetched_for_the_print(member, server, run, failures)
        }
    }
}

/// The member's timeline read from a copy of its trace files fetched into a scratch directory, which is removed once read, adding each failure.
fn timeline_fetched_for_the_print(
    member: &str,
    server: &str,
    run: &impl Fn(ServerMemberStep<'_>) -> io::Result<Output>,
    failures: &mut Vec<String>,
) -> String {
    let holder = match fresh_directory_in(
        &std::env::temp_dir(),
        &format!(
            "{FETCHED_FOR_THE_PRINT_PREFIX}-{}-{server}-{member}",
            std::process::id()
        ),
    ) {
        Ok(holder) => holder,
        Err(failure) => {
            failures.push(format!(
                "trace files of {member} not fetched for its timeline: {failure}"
            ));
            return timeline_of_fetched(None);
        }
    };
    let fetched_for_the_print = holder.join(FETCHED_FOR_THE_PRINT_DIRECTORY);
    let fetched = run(ServerMemberStep::FetchTraceFiles(&fetched_for_the_print));
    let timeline = match succeeded(&fetched) {
        Ok(_fetched) => timeline_of_fetched(Some(&fetched_for_the_print)),
        Err(failure) => {
            failures.push(format!(
                "trace files of {member} on {server} not fetched for its timeline: {failure}"
            ));
            timeline_of_fetched(None)
        }
    };
    if let Err(failure) = remove_scratch(&holder) {
        failures.push(failure.to_string());
    }
    timeline
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
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::*;
    use crate::scratch_machine::fresh_directory::fresh_directory_in;
    use crate::scratch_machine::remove_scratch::remove_scratch;
    use crate::scratch_machine::trace_reader::NO_EVENT_RECORDED;
    use crate::scratch_machine::{REDACTED, REDACTED_NOTE_EXTENSION, STOP_FAILED_EXTENSION};

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
            &format!("{SERVER}-{MEMBER}"),
            Some(&kept_in),
            &[PLANTED_INVITATION.to_owned(), PLANTED_TOKEN.to_owned()],
            TestSoFar::NotFailing,
            stand_in_for_the_recipes,
        );

        let kept_traces = kept_in
            .join(format!("{SERVER}-{MEMBER}"))
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
            &format!("{SERVER}-{MEMBER}"),
            Some(&kept_in),
            &[],
            TestSoFar::Failing,
            stand_in_for_the_recipes,
        );

        let kept = kept_in.join(format!("{SERVER}-{MEMBER}"));
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
            &format!("{SERVER}-{MEMBER}"),
            Some(&kept_in),
            &[],
            TestSoFar::NotFailing,
            stand_in_for_the_recipes,
        );

        let kept = kept_in.join(format!("{SERVER}-{MEMBER}"));
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
            &format!("{SERVER}-{MEMBER}"),
            Some(&not_a_directory.join(KEPT_BENEATH_NOT_A_DIRECTORY)),
            &[],
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
            &format!("{SERVER}-{MEMBER}"),
            Some(&kept_in),
            &[PLANTED_TOKEN.to_owned()],
            TestSoFar::NotFailing,
            stand_in_for_the_recipes,
        );

        let kept = kept_in.join(format!("{SERVER}-{MEMBER}"));
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
