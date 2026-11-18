use std::ffi::OsStr;
use std::fs;
use std::io::{self, PipeReader, Read};
use std::ops::{Deref, DerefMut};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
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

/// A spawned process owned from its spawn and ended when dropped, killed and waited for, so no failure after its spawn leaves it running unmarked.
pub struct OwnedProcess {
    child: Child,
    label: String,
    marked_in: PathBuf,
}

impl OwnedProcess {
    /// Spawns the command, owning the process in the value it returns, a failed end of it to be marked in the directory under the label.
    #[track_caller]
    pub fn spawned(command: &mut Command, marked_in: &Path, label: &str) -> Self {
        Self {
            child: command.spawn().unwrap(),
            label: label.to_owned(),
            marked_in: marked_in.to_path_buf(),
        }
    }

    /// Kills the process and waits for it, never panicking: a kill or a wait that fails is written to standard error and marked in its directory; a no-op once the process has ended.
    pub fn end(&mut self) {
        self.end_through(|process| process.kill().and_then(|()| process.wait()));
    }

    /// Ends the process through the kill and wait handed in, never panicking: one that fails is written to standard error and marked in its directory.
    pub fn end_through(
        &mut self,
        kill_then_wait: impl FnOnce(&mut Child) -> io::Result<ExitStatus>,
    ) {
        if let Err(failure) = kill_then_wait(&mut self.child) {
            mark_end_failure(&self.marked_in, &self.label, self.child.id(), &failure);
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

/// Writes to standard error, and to a file of the directory named for the process, that its end failed and why, a mark that cannot be written reported on standard error alone.
fn mark_end_failure(marked_in: &Path, label: &str, process_id: u32, failure: &io::Error) {
    let mark = format!(
        "the end of {label}, process {process_id}, failed, so it may have run on while its machine's data was kept: {failure}\n"
    );
    print_to_standard_error(mark.trim_end());
    let marked = marked_in.join(format!("{label}-{process_id}.{END_FAILED_EXTENSION}"));
    if let Err(unwritten) = fs::write(&marked, mark) {
        print_to_standard_error(&format!("{}: {unwritten}", marked.display()));
    }
}

/// Runs an ignored fixture of the running test binary as a child process, the fixture variable naming it beside the variables handed in and its standard error sent where handed, requires the child to end within the bound, ending it and failing the test when it does not, and returns how it exited.
#[track_caller]
pub fn fixture_exit_within(
    fixture: &str,
    variables: &[(&str, &OsStr)],
    standard_error: Stdio,
    within: Duration,
) -> ExitStatus {
    let (only_the_child_writes, child_output) = io::pipe().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", fixture, "--ignored", "--nocapture"])
        .env(FIXTURE_VARIABLE, fixture)
        .envs(variables.iter().copied())
        .stdin(Stdio::null())
        .stdout(child_output)
        .stderr(standard_error)
        .spawn()
        .unwrap();
    let after = format!("the fixture {fixture} started");
    let ended = catch_unwind(AssertUnwindSafe(|| {
        require_stand_in_ended_within(only_the_child_writes, within, &after);
    }));
    if let Err(still_running) = ended {
        if let Err(unended) = child.kill().and_then(|()| child.wait()) {
            print_to_standard_error(&format!(
                "the fixture {fixture}, process {}, could not be ended: {unended}",
                child.id()
            ));
        }
        resume_unwind(still_running);
    }
    child.wait().unwrap()
}

/// Requires that the pipe only a test's stand-in holds open closes within the bound, the sign the stand-in ended, failing the test when it stays open or cannot be read; read on a thread of its own so no signal is sent to any process.
#[track_caller]
pub fn require_stand_in_ended_within(
    only_the_stand_in_writes: PipeReader,
    within: Duration,
    after: &str,
) {
    let (closed, close_seen) = mpsc::channel();
    std::thread::spawn(move || {
        let mut written = Vec::new();
        drop(closed.send((&only_the_stand_in_writes).read_to_end(&mut written)));
    });
    match close_seen.recv_timeout(within) {
        Ok(Ok(_bytes_read)) => {}
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
