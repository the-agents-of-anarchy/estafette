use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;

use super::{fresh_kept_directory, keep_run_record, keep_stop_failure};

/// The directory a member's fetched trace files are kept in, inside the directory its records are kept in.
const KEPT_TRACE_FILES_DIRECTORY: &str = "traces";

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

/// Where the end of a member on a server kept its trace files, when it fetched them, and each failure of its steps in the order they ran.
pub struct ServerMemberEnd {
    pub kept_trace_files: Option<PathBuf>,
    pub failures: Vec<String>,
}

/// Stops the member on the server through the runner and, when the test keeps records, keeps a mark of a failed stop, its run record and its trace files in a directory of its own named as given, each step run whatever an earlier one did.
pub fn ended_server_member(
    member: &str,
    server: &str,
    kept_as: &str,
    kept_in: Option<&Path>,
    secrets: &[String],
    run: impl Fn(ServerMemberStep<'_>) -> io::Result<Output>,
) -> ServerMemberEnd {
    let stop = run(ServerMemberStep::Stop);
    if let Err(failure) = succeeded(&stop) {
        eprintln!("{member} on {server} did not stop cleanly: {failure}");
    }
    let mut failures = Vec::new();
    let Some(directory) = kept_in else {
        return ServerMemberEnd {
            kept_trace_files: None,
            failures,
        };
    };
    let kept = match fresh_kept_directory(directory, kept_as) {
        Ok(kept) => kept,
        Err(failure) => {
            failures.push(format!("records of {member} not kept: {failure}"));
            return ServerMemberEnd {
                kept_trace_files: None,
                failures,
            };
        }
    };
    if let Err(failure) = keep_stop_failure(&kept, member, &stop, secrets) {
        failures.push(format!("failed stop of {member} not marked: {failure}"));
    }
    let printed = run(ServerMemberStep::PrintRunRecord);
    match succeeded(&printed) {
        Ok(output) => {
            let record = String::from_utf8_lossy(&output.stdout);
            if let Err(failure) = keep_run_record(&kept, member, &record, secrets) {
                failures.push(format!("run record of {member} not kept: {failure}"));
            }
        }
        Err(failure) => {
            failures.push(format!(
                "run record of {member} on {server} not read: {failure}"
            ));
        }
    }
    let kept_trace_files = kept.join(KEPT_TRACE_FILES_DIRECTORY);
    let fetched = run(ServerMemberStep::FetchTraceFiles(&kept_trace_files));
    if let Err(failure) = succeeded(&fetched) {
        failures.push(format!(
            "trace files of {member} on {server} not fetched: {failure}"
        ));
        return ServerMemberEnd {
            kept_trace_files: None,
            failures,
        };
    }
    eprintln!(
        "trace files of {member} kept at {}",
        kept_trace_files.display()
    );
    ServerMemberEnd {
        kept_trace_files: Some(kept_trace_files),
        failures,
    }
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    use super::*;
    use crate::scratch_machine::STOP_FAILED_EXTENSION;
    use crate::scratch_machine::fresh_directory::fresh_directory_in;
    use crate::scratch_machine::remove_scratch::remove_scratch;

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

    /// A recipe run that exited with code 1 having printed the text on standard error.
    fn failed_run(printed: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(EXITED_WITH_ONE),
            stdout: Vec::new(),
            stderr: printed.as_bytes().to_vec(),
        }
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
            stand_in_for_the_recipes,
        );

        let kept = kept_in.join(format!("{SERVER}-{MEMBER}"));
        let kept_traces = kept.join(KEPT_TRACE_FILES_DIRECTORY);
        assert_eq!(
            steps_run.into_inner(),
            ["stop", "print run record", "fetch trace files"]
        );
        assert_eq!(ended.kept_trace_files, Some(kept_traces.clone()));
        assert_eq!(ended.failures.len(), 1, "{:?}", ended.failures);
        assert!(
            ended.failures[0].contains(MEMBER)
                && ended.failures[0].contains(SERVER)
                && ended.failures[0].contains(RUN_RECORD_REFUSAL),
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
                kept_traces
            ])
        );
        remove_scratch(&kept_in).unwrap();
    }
}
