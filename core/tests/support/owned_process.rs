use std::ops::{Deref, DerefMut};
use std::process::{Child, Command};

/// A spawned process owned from its spawn and ended when dropped, killed and waited for, so no failure after its spawn leaves it running.
pub struct OwnedProcess {
    child: Child,
}

impl OwnedProcess {
    /// Spawns the command, owning the process in the value it returns.
    #[track_caller]
    pub fn spawned(command: &mut Command) -> Self {
        Self {
            child: command.spawn().unwrap(),
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
        drop(self.child.kill());
        drop(self.child.wait());
    }
}
