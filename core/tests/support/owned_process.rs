use std::fs;
use std::io;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// The extension of the file that marks a process whose kill or wait failed, beside the run records its machine keeps.
pub const END_FAILED_EXTENSION: &str = "end-failed";

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
        let ended = self.child.kill().and_then(|()| self.child.wait());
        if let Err(failure) = ended {
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
pub fn mark_end_failure(marked_in: &Path, label: &str, process_id: u32, failure: &io::Error) {
    let mark = format!(
        "the end of {label}, process {process_id}, failed, so it may have run on while its machine's data was kept: {failure}\n"
    );
    eprint!("{mark}");
    let marked = marked_in.join(format!("{label}-{process_id}.{END_FAILED_EXTENSION}"));
    if let Err(unwritten) = fs::write(&marked, mark) {
        eprintln!("{}: {unwritten}", marked.display());
    }
}
