use std::fmt;
use std::fs::{self, File};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::modes::PERMISSION_BITS_MASK;

/// What a file system entry is, read without following a symbolic link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Directory,
    RegularFile,
    SymbolicLink,
    Other,
}

/// The facts about an entry that decide whether a process may trust it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryFacts {
    pub kind: EntryKind,
    pub owner_user_id: u32,
    pub permission_bits: u32,
}

impl EntryFacts {
    /// The facts of the entry the metadata describes, which must come from a read that does not follow symbolic links.
    #[must_use]
    pub fn of(metadata: &fs::Metadata) -> Self {
        let file_type = metadata.file_type();
        let kind = if file_type.is_symlink() {
            EntryKind::SymbolicLink
        } else if file_type.is_dir() {
            EntryKind::Directory
        } else if file_type.is_file() {
            EntryKind::RegularFile
        } else {
            EntryKind::Other
        };
        Self {
            kind,
            owner_user_id: metadata.uid(),
            permission_bits: metadata.mode() & PERMISSION_BITS_MASK,
        }
    }

    /// The facts of the entry at the path, not following a symbolic link there.
    ///
    /// # Errors
    ///
    /// Fails when the entry cannot be inspected, as when nothing exists at the path.
    pub fn at(path: &Path) -> io::Result<Self> {
        fs::symlink_metadata(path).map(|metadata| Self::of(&metadata))
    }
}

/// What makes an entry of any kind not the user's own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UntrustedEntry {
    SymbolicLink,
    OwnedByAnotherUser { owner: u32, user: u32 },
    ReplacedWhileOpened,
}

impl fmt::Display for UntrustedEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SymbolicLink => write!(formatter, "a symbolic link"),
            Self::OwnedByAnotherUser { owner, user } => {
                write!(formatter, "owned by user {owner}, not by user {user}")
            }
            Self::ReplacedWhileOpened => write!(formatter, "replaced while it was opened"),
        }
    }
}

/// Whether the opened file and the entry its name now reaches are the same entry on the same device.
#[must_use]
pub fn same_entry(opened: &fs::Metadata, named: &fs::Metadata) -> bool {
    (opened.dev(), opened.ino()) == (named.dev(), named.ino())
}

/// The user this process runs as could not be learned.
#[derive(Debug)]
pub enum ProcessUserUnknown {
    PipeNotMade(io::Error),
    PipeOwnerUnreadable(io::Error),
}

impl fmt::Display for ProcessUserUnknown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PipeNotMade(source) => write!(
                formatter,
                "cannot learn the user this process runs as: it could not make a pipe: {source}"
            ),
            Self::PipeOwnerUnreadable(source) => write!(
                formatter,
                "cannot learn the user this process runs as: it could not read the owner of a pipe it made: {source}"
            ),
        }
    }
}

impl std::error::Error for ProcessUserUnknown {}

/// Reads the numeric id of the user this process runs as from the owner the operating system reports for an anonymous pipe the process makes, which is the user the process creates files as.
///
/// # Errors
///
/// Fails when the pipe cannot be made or its owner cannot be read.
pub fn process_user_id() -> Result<u32, ProcessUserUnknown> {
    let (reader, _writer) = io::pipe().map_err(ProcessUserUnknown::PipeNotMade)?;
    File::from(OwnedFd::from(reader))
        .metadata()
        .map(|metadata| metadata.uid())
        .map_err(ProcessUserUnknown::PipeOwnerUnreadable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_user_is_the_owner_of_a_file_the_process_creates_runtime_directory_is_private_or_refused()
     {
        let directory = crate::this_process_directory::fresh_directory_of_this_process_in(
            &std::env::temp_dir(),
            "estafette-private-entry-process-user",
        )
        .unwrap();
        let created = directory.join("created-by-this-process");
        fs::write(&created, b"").unwrap();

        assert_eq!(
            process_user_id().unwrap(),
            fs::metadata(&created).unwrap().uid()
        );
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }
}
