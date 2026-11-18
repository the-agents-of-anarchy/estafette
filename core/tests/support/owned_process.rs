use std::ffi::OsStr;
use std::fs;
use std::io::{self, PipeReader, Read};
use std::ops::{Deref, DerefMut};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::print_to_standard_error;

/// The extension of the file that marks a process whose kill or wait failed, beside the run records its machine keeps.
pub const END_FAILED_EXTENSION: &str = "end-failed";
/// How long the end of a test's stand-in may take to reach the pipe only it holds once its owner ended it: chosen, not measured, far above a kill and a wait.
pub const STAND_IN_ENDED_WITHIN: Duration = Duration::from_secs(10);
/// The variable a parent test sets, naming the fixture it runs in a child process, so the fixture refuses to run anywhere else.
pub const FIXTURE_VARIABLE: &str = "ESTAFETTE_FIXTURE_OF_A_PARENT_TEST";
/// How long a fixture a parent test runs as a child test binary may take to end: chosen, not measured, far above a fixture that starts one machine and fails.
pub const FIXTURE_ENDED_WITHIN: Duration = Duration::from_secs(60);
/// The file a test sends a fixture's standard error to, in a scratch directory of its own.
pub const FIXTURE_STANDARD_ERROR: &str = "fixture.stderr";
/// The exit code of a test binary whose test failed, as against a signal that ends one that aborts.
pub const FAILED_TEST_EXIT_CODE: i32 = 101;
/// What a test is failed with when a child it runs closed its standard output and outlived the bound on its exit.
const OUTLIVED_ITS_OUTPUT: &str = "closed its standard output and had not exited";
/// The command that sends a process a signal by its id.
const KILL_COMMAND: &str = "kill";
/// The flag that makes that signal one no process can catch.
const KILL_SIGNAL_FLAG: &str = "-KILL";

/// A spawned process owned from its spawn and ended when dropped, killed and waited for, so no failure after its spawn leaves it running unmarked.
pub struct OwnedProcess {
    child: Child,
    label: String,
    end_marks: EndMarks,
}

/// Where the processes of one owner mark a failed end, and every failed end they met, held whether or not its mark could be written.
#[derive(Clone)]
pub struct EndMarks {
    directory: PathBuf,
    failed: Arc<Mutex<Vec<FailedEnd>>>,
}

/// A process whose kill or wait failed: the label it was spawned under and its process id.
#[derive(Clone)]
pub struct FailedEnd {
    pub(super) label: String,
    pub(super) process_id: u32,
}

impl EndMarks {
    /// Marks written in the directory, no failed end met yet.
    pub fn in_directory(directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
            failed: Arc::default(),
        }
    }

    /// Every failed end met so far, in the order met.
    pub(super) fn failed(&self) -> Vec<FailedEnd> {
        self.failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Holds the failed end, then writes to standard error, and to its mark in the directory, that it failed and why, a mark that cannot be written reported on standard error alone.
    fn mark(&self, failed: FailedEnd, failure: &io::Error) {
        let mark = format!(
            "the end of {}, process {}, failed, so it may have run on while its machine's data was kept: {failure}\n",
            failed.label, failed.process_id
        );
        let marked = self.directory.join(failed.mark_name());
        self.failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(failed);
        print_to_standard_error(mark.trim_end());
        if let Err(unwritten) = fs::write(&marked, mark) {
            print_to_standard_error(&format!("{}: {unwritten}", marked.display()));
        }
    }
}

impl FailedEnd {
    /// The name of the file that marks this failed end.
    pub(super) fn mark_name(&self) -> String {
        format!("{}-{}.{END_FAILED_EXTENSION}", self.label, self.process_id)
    }
}

impl OwnedProcess {
    /// Spawns the command, owning the process in the value it returns, a failed end of it to be held and marked by the end marks under the label.
    #[track_caller]
    pub fn spawned(command: &mut Command, end_marks: &EndMarks, label: &str) -> Self {
        Self {
            child: command.spawn().unwrap(),
            label: label.to_owned(),
            end_marks: end_marks.clone(),
        }
    }

    /// Kills the process and waits for it, never panicking: a kill or a wait that fails is held, written to standard error and marked by its end marks; a no-op once the process has ended.
    pub fn end(&mut self) {
        self.end_through(|process| process.kill().and_then(|()| process.wait()));
    }

    /// Ends the process through the kill and wait handed in, never panicking: one that fails is held, written to standard error and marked by its end marks.
    pub fn end_through(
        &mut self,
        kill_then_wait: impl FnOnce(&mut Child) -> io::Result<ExitStatus>,
    ) {
        if let Err(failure) = kill_then_wait(&mut self.child) {
            let failed = FailedEnd {
                label: self.label.clone(),
                process_id: self.child.id(),
            };
            self.end_marks.mark(failed, &failure);
        }
    }
}

impl Deref for OwnedProcess {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for OwnedProcess {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        self.end();
    }
}

/// How a fixture run as a child process ended: its exit and every byte it wrote to standard output.
pub struct FixtureRun {
    pub exit: ExitStatus,
    pub standard_output: Vec<u8>,
}

/// Runs an ignored fixture of the running test binary as a child process, the fixture variable naming it beside the variables handed in and its standard error sent where handed, requires the child to close its standard output within the bound and then to exit within the bound, ending it and failing the test when it does not, and returns how it ended.
#[track_caller]
pub fn fixture_run_within(
    fixture: &str,
    variables: &[(&str, &OsStr)],
    standard_error: Stdio,
    within: Duration,
) -> FixtureRun {
    let mut fixture_run = Command::new(std::env::current_exe().unwrap());
    fixture_run
        .args(["--exact", fixture, "--ignored", "--nocapture"])
        .env(FIXTURE_VARIABLE, fixture)
        .envs(variables.iter().copied())
        .stdin(Stdio::null())
        .stderr(standard_error);
    ran_within(fixture_run, &format!("the fixture {fixture}"), within)
}

/// Spawns the command, its standard output on a pipe only the child holds, requires the child to close that pipe within the bound and then to exit within the bound, ending it and failing the test when it does not, and returns how it ended.
#[track_caller]
fn ran_within(mut command: Command, named: &str, within: Duration) -> FixtureRun {
    let (only_the_child_writes, child_output) = io::pipe().unwrap();
    let mut child = command.stdout(child_output).spawn().unwrap();
    drop(command);
    let after = format!("{named} started");
    let ended = catch_unwind(AssertUnwindSafe(|| {
        written_until_the_stand_in_ended_within(only_the_child_writes, within, &after)
    }));
    let standard_output = ended.unwrap_or_else(|still_running| {
        if let Err(unended) = child.kill().and_then(|()| child.wait()) {
            print_unended(named, child.id(), &unended);
        }
        resume_unwind(still_running)
    });
    FixtureRun {
        exit: exited_within(child, named, within),
        standard_output,
    }
}

/// How the child exited, waited for on a thread of its own within the bound, failing the test when its exit cannot be waited for, or, once it is killed, when it outlives the bound.
#[track_caller]
fn exited_within(mut child: Child, named: &str, within: Duration) -> ExitStatus {
    let process_id = child.id();
    let (exited, exit) = mpsc::channel();
    std::thread::spawn(move || drop(exited.send(child.wait())));
    match exit.recv_timeout(within) {
        Ok(Ok(status)) => status,
        Ok(Err(unwaited)) => {
            panic!("the exit of {named}, process {process_id}, could not be waited for: {unwaited}")
        }
        Err(RecvTimeoutError::Timeout) => {
            if let Err(unended) = killed_within(process_id, &exit, within) {
                print_unended(named, process_id, &unended);
            }
            panic!("{named} {OUTLIVED_ITS_OUTPUT} {within:?} later")
        }
        Err(RecvTimeoutError::Disconnected) => {
            panic!("the thread waiting for the exit of {named} ended without an answer")
        }
    }
}

/// Kills the process by its id, still unreaped while the thread waiting for it holds no answer, and waits within the bound for that thread to see it exit, or why it could not be ended.
fn killed_within(
    process_id: u32,
    exit: &mpsc::Receiver<io::Result<ExitStatus>>,
    within: Duration,
) -> io::Result<()> {
    let killed = Command::new(KILL_COMMAND)
        .args([KILL_SIGNAL_FLAG, &process_id.to_string()])
        .status()?;
    if !killed.success() {
        return Err(io::Error::other(format!(
            "{KILL_COMMAND} {KILL_SIGNAL_FLAG} {process_id}: {killed}"
        )));
    }
    exit.recv_timeout(within)
        .map_err(|unanswered| io::Error::other(format!("no exit within {within:?}: {unanswered}")))?
        .map(drop)
}

/// Writes to standard error that the named process could not be ended, and why.
fn print_unended(named: &str, process_id: u32, unended: &io::Error) {
    print_to_standard_error(&format!(
        "{named}, process {process_id}, could not be ended: {unended}"
    ));
}

/// Requires that the pipe only a test's stand-in holds open closes within the bound, the sign the stand-in ended, failing the test when it stays open or cannot be read; read on a thread of its own so no signal is sent to any process.
#[track_caller]
pub fn require_stand_in_ended_within(
    only_the_stand_in_writes: PipeReader,
    within: Duration,
    after: &str,
) {
    drop(written_until_the_stand_in_ended_within(
        only_the_stand_in_writes,
        within,
        after,
    ));
}

/// Every byte written to the pipe only a test's stand-in holds open, read until it closes within the bound, the sign the stand-in ended, failing the test when it stays open or cannot be read; read on a thread of its own so no signal is sent to any process.
#[track_caller]
fn written_until_the_stand_in_ended_within(
    only_the_stand_in_writes: PipeReader,
    within: Duration,
    after: &str,
) -> Vec<u8> {
    let (closed, close_seen) = mpsc::channel();
    std::thread::spawn(move || {
        let mut written = Vec::new();
        let read = (&only_the_stand_in_writes).read_to_end(&mut written);
        drop(closed.send(read.map(|_bytes_read| written)));
    });
    match close_seen.recv_timeout(within) {
        Ok(Ok(written)) => written,
        Ok(Err(failure)) => panic!(
            "the stand-in's pipe could not be read after {after}, so whether the stand-in ended is unknown: {failure}"
        ),
        Err(RecvTimeoutError::Timeout) => {
            panic!("the stand-in still ran {within:?} after {after}")
        }
        Err(RecvTimeoutError::Disconnected) => {
            panic!("the thread reading the stand-in's pipe ended without an answer after {after}")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    /// The shell a test's outliving child runs in.
    const SHELL: &str = "sh";
    /// A script that closes its standard output, then reads a standard input the test holds open, so the child outlives any bound with no sleep.
    const CLOSE_OUTPUT_THEN_READ_INPUT: &str = "exec >&-; read -r line";
    /// What the test calls its outliving child.
    const OUTLIVING_CHILD: &str = "the child that outlives its output";
    /// The bound the test hands the run: chosen, not measured, far above a shell's start.
    const EXIT_BOUND: Duration = Duration::from_secs(2);

    #[test]
    fn observability_decision_16_a_child_that_closes_its_standard_output_and_does_not_exit_fails_the_test_within_the_bound_and_is_ended()
     {
        let (child_input, only_the_test_writes) = io::pipe().unwrap();
        let mut outliving = Command::new(SHELL);
        outliving
            .args(["-c", CLOSE_OUTPUT_THEN_READ_INPUT])
            .stdin(child_input)
            .stderr(Stdio::null());
        let (answered, answer) = mpsc::channel();
        std::thread::spawn(move || {
            let ran = catch_unwind(AssertUnwindSafe(|| {
                ran_within(outliving, OUTLIVING_CHILD, EXIT_BOUND)
            }));
            drop(answered.send(ran.err().map(|panicked| {
                panicked
                    .downcast::<String>()
                    .map_or_else(|_unworded| String::new(), |worded| *worded)
            })));
        });

        let failure = answer
            .recv_timeout(EXIT_BOUND + STAND_IN_ENDED_WITHIN)
            .unwrap_or_else(|unanswered| {
                panic!(
                    "the run of {OUTLIVING_CHILD} did not end within {:?}: {unanswered}",
                    EXIT_BOUND + STAND_IN_ENDED_WITHIN
                )
            })
            .unwrap_or_else(|| panic!("the run of {OUTLIVING_CHILD} did not fail the test"));

        assert!(
            failure.contains(&format!("{OUTLIVING_CHILD} {OUTLIVED_ITS_OUTPUT}")),
            "{failure}"
        );
        assert_eq!(
            (&only_the_test_writes)
                .write_all(b"\n")
                .map_err(|unwritten| unwritten.kind()),
            Err(io::ErrorKind::BrokenPipe),
            "{OUTLIVING_CHILD} still reads its standard input"
        );
    }
}
