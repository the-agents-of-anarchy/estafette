use std::fmt;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const PERMISSION_BITS: u32 = 0o777;
const GROUP_AND_OTHER_PERMISSION_BITS: u32 = 0o077;

/// What a file system entry is, read without following a symbolic link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    Directory,
    RegularFile,
    SymbolicLink,
    Other,
}

/// The facts about an entry that decide whether the bridge may trust it.
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
            permission_bits: metadata.mode() & PERMISSION_BITS,
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

/// What stops an existing entry from being used as a private directory of the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectoryFinding {
    SymbolicLink,
    NotADirectory,
    OwnedByAnotherUser { owner: u32, user: u32 },
    GrantsAccessBeyondOwner { permission_bits: u32 },
    ReplacedWhileOpened,
}

impl fmt::Display for DirectoryFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SymbolicLink => write!(formatter, "a symbolic link"),
            Self::NotADirectory => write!(formatter, "not a directory"),
            Self::OwnedByAnotherUser { owner, user } => {
                write!(formatter, "owned by user {owner}, not by user {user}")
            }
            Self::GrantsAccessBeyondOwner { permission_bits } => write!(
                formatter,
                "mode {permission_bits:04o}, which grants access to group or other"
            ),
            Self::ReplacedWhileOpened => write!(formatter, "replaced while it was opened"),
        }
    }
}

/// Says what stops the entry from being a directory of the user that grants nothing to group or other, none when nothing does.
#[must_use]
const fn directory_finding(facts: EntryFacts, user_id: u32) -> Option<DirectoryFinding> {
    match facts.kind {
        EntryKind::SymbolicLink => Some(DirectoryFinding::SymbolicLink),
        EntryKind::RegularFile | EntryKind::Other => Some(DirectoryFinding::NotADirectory),
        EntryKind::Directory if facts.owner_user_id != user_id => {
            Some(DirectoryFinding::OwnedByAnotherUser {
                owner: facts.owner_user_id,
                user: user_id,
            })
        }
        EntryKind::Directory if facts.permission_bits & GROUP_AND_OTHER_PERMISSION_BITS != 0 => {
            Some(DirectoryFinding::GrantsAccessBeyondOwner {
                permission_bits: facts.permission_bits,
            })
        }
        EntryKind::Directory => None,
    }
}

/// An existing directory the bridge refuses to use, with the directory and what was found.
#[derive(Debug, Eq, PartialEq)]
pub struct ExistingDirectoryRefused {
    path: PathBuf,
    found: DirectoryFinding,
}

impl fmt::Display for ExistingDirectoryRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} already exists and is {}; it must be a directory of yours with no access for group or other",
            self.path.display(),
            self.found
        )
    }
}

impl std::error::Error for ExistingDirectoryRefused {}

/// Refuses the directory at the path unless it is a directory of the user granting nothing to group or other.
///
/// # Errors
///
/// `ExistingDirectoryRefused` naming the directory and what was found.
pub fn require_private_directory_of(
    user_id: u32,
    path: &Path,
    facts: EntryFacts,
) -> Result<(), ExistingDirectoryRefused> {
    directory_finding(facts, user_id).map_or(Ok(()), |found| {
        Err(ExistingDirectoryRefused {
            path: path.to_path_buf(),
            found,
        })
    })
}

fn directory_refusal_of(refusal: ExistingDirectoryRefused) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, refusal)
}

/// Creates the directory with its missing parents, private to the user, or refuses an existing directory the user may not trust.
///
/// # Errors
///
/// Fails when a directory cannot be created or inspected, or with an `ExistingDirectoryRefused` inside the error when the entry already there is not a private directory of the user.
pub fn create_private_directory(user_id: u32, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::DirBuilder::new()
        .mode(PRIVATE_DIRECTORY_MODE)
        .create(path)
    {
        Ok(()) => make_directory_private_opened_by(user_id, path, |path: &Path| File::open(path)),
        Err(failure) if failure.kind() == io::ErrorKind::AlreadyExists => {
            require_private_directory_of(user_id, path, EntryFacts::at(path)?)
                .map_err(directory_refusal_of)
        }
        Err(failure) => Err(failure),
    }
}

fn make_directory_private_opened_by(
    user_id: u32,
    path: &Path,
    open: impl FnOnce(&Path) -> io::Result<File>,
) -> io::Result<()> {
    let directory = match open(path) {
        Ok(directory) => directory,
        Err(denied) if denied.kind() == io::ErrorKind::PermissionDenied => {
            return make_unreadable_directory_private_by_name(user_id, path);
        }
        Err(failure) => return Err(failure),
    };
    let opened = directory.metadata()?;
    let named = fs::symlink_metadata(path)?;
    require_private_directory_of(user_id, path, EntryFacts::of(&named))
        .map_err(directory_refusal_of)?;
    if (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
        return Err(directory_refusal_of(ExistingDirectoryRefused {
            path: path.to_path_buf(),
            found: DirectoryFinding::ReplacedWhileOpened,
        }));
    }
    directory.set_permissions(Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
}

fn make_unreadable_directory_private_by_name(user_id: u32, path: &Path) -> io::Result<()> {
    let named = fs::symlink_metadata(path)?;
    require_private_directory_of(user_id, path, EntryFacts::of(&named))
        .map_err(directory_refusal_of)?;
    fs::set_permissions(path, Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
}

/// What stops the runtime directory from being used: it cannot be inspected, or it exists and is refused.
#[derive(Debug)]
pub enum RuntimeDirectoryProblem {
    Uninspectable { path: PathBuf, source: io::Error },
    Refused(ExistingDirectoryRefused),
}

impl fmt::Display for RuntimeDirectoryProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uninspectable { path, source } => {
                write!(formatter, "cannot inspect {}: {source}", path.display())
            }
            Self::Refused(refusal) => refusal.fmt(formatter),
        }
    }
}

impl std::error::Error for RuntimeDirectoryProblem {}

/// Judges the runtime directory: a missing one passes, one that cannot be inspected or is not a private directory of the user is a problem.
///
/// # Errors
///
/// `RuntimeDirectoryProblem` naming the directory and what was found.
pub fn check_runtime_directory(
    user_id: u32,
    directory: &Path,
    inspect: impl FnOnce(&Path) -> io::Result<EntryFacts>,
) -> Result<(), RuntimeDirectoryProblem> {
    match inspect(directory) {
        Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(RuntimeDirectoryProblem::Uninspectable {
            path: directory.to_path_buf(),
            source,
        }),
        Ok(facts) => require_private_directory_of(user_id, directory, facts)
            .map_err(RuntimeDirectoryProblem::Refused),
    }
}

/// What stops an existing entry from being trusted as one of the bridge's state files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StateFileFinding {
    SymbolicLink,
    NotARegularFile,
    OwnedByAnotherUser { owner: u32, user: u32 },
    ReplacedWhileOpened,
}

impl fmt::Display for StateFileFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SymbolicLink => write!(formatter, "a symbolic link"),
            Self::NotARegularFile => write!(formatter, "not a regular file"),
            Self::OwnedByAnotherUser { owner, user } => {
                write!(formatter, "owned by user {owner}, not by user {user}")
            }
            Self::ReplacedWhileOpened => write!(formatter, "replaced while it was opened"),
        }
    }
}

/// Says what stops the entry from being a regular file of the user, none when nothing does.
#[must_use]
const fn state_file_finding(facts: EntryFacts, user_id: u32) -> Option<StateFileFinding> {
    match facts.kind {
        EntryKind::SymbolicLink => Some(StateFileFinding::SymbolicLink),
        EntryKind::Directory | EntryKind::Other => Some(StateFileFinding::NotARegularFile),
        EntryKind::RegularFile if facts.owner_user_id != user_id => {
            Some(StateFileFinding::OwnedByAnotherUser {
                owner: facts.owner_user_id,
                user: user_id,
            })
        }
        EntryKind::RegularFile => None,
    }
}

/// A state file the bridge refuses to read or replace, with the file and what was found.
#[derive(Debug, Eq, PartialEq)]
struct StateFileRefused {
    path: PathBuf,
    found: StateFileFinding,
}

impl fmt::Display for StateFileRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} is {}; a state file of the bridge must be a regular file of yours",
            self.path.display(),
            self.found
        )
    }
}

impl std::error::Error for StateFileRefused {}

fn refusal_of(path: &Path, found: StateFileFinding) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        StateFileRefused {
            path: path.to_path_buf(),
            found,
        },
    )
}

/// Reads the state file as text, refusing anything but a regular file of the user, a symbolic link included.
///
/// # Errors
///
/// Fails when the file cannot be read, or with a `StateFileRefused` inside the error when it is not a regular file of the user.
pub fn read_state_file(path: &Path, user_id: u32) -> io::Result<String> {
    read_state_file_opened_by(path, user_id, |path: &Path| File::open(path))
}

fn read_state_file_opened_by(
    path: &Path,
    user_id: u32,
    open: impl FnOnce(&Path) -> io::Result<File>,
) -> io::Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if let Some(found) = state_file_finding(EntryFacts::of(&metadata), user_id) {
        return Err(refusal_of(path, found));
    }
    let mut file = open(path)?;
    let opened = file.metadata()?;
    if (opened.dev(), opened.ino()) != (metadata.dev(), metadata.ino()) {
        return Err(refusal_of(path, StateFileFinding::ReplacedWhileOpened));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

/// Writes the text to a new file at the staging path, owner read and write alone, and renames it over the state file unless that is not a regular file of the user.
///
/// # Errors
///
/// Fails when the staging path is taken or the file cannot be written, or with a `StateFileRefused` inside the error when the state file is a symbolic link or another user's; the staging file is then left unrenamed and removed.
pub fn write_state_file(user_id: u32, path: &Path, staging: &Path, text: &str) -> io::Result<()> {
    let mut staged = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(staging)?;
    let outcome = fill_and_rename_over(&mut staged, user_id, staging, path, text);
    if outcome.is_err() {
        drop(fs::remove_file(staging));
    }
    outcome
}

fn fill_and_rename_over(
    staged: &mut File,
    user_id: u32,
    staging: &Path,
    path: &Path,
    text: &str,
) -> io::Result<()> {
    staged.set_permissions(Permissions::from_mode(PRIVATE_FILE_MODE))?;
    staged.write_all(text.as_bytes())?;
    require_replaceable(path, user_id)?;
    fs::rename(staging, path)
}

fn require_replaceable(path: &Path, user_id: u32) -> io::Result<()> {
    match EntryFacts::at(path) {
        Err(failure) if failure.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(failure) => Err(failure),
        Ok(facts) => {
            state_file_finding(facts, user_id).map_or(Ok(()), |found| Err(refusal_of(path, found)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const THE_USER: u32 = 1000;
    const OWNERS_AROUND_THE_USER: [u32; 4] = [0, 999, 1000, 1001];
    const PERMISSION_BIT_COMBINATIONS: u32 = 0o1000;
    const ENTRY_KINDS: [EntryKind; 4] = [
        EntryKind::Directory,
        EntryKind::RegularFile,
        EntryKind::SymbolicLink,
        EntryKind::Other,
    ];
    const VICTIM_TEXT: &str = "the file a planted link points at";
    const STATE_TEXT: &str = "3";
    const LOOSE_FILE_MODE: u32 = 0o644;
    const LOOSE_DIRECTORY_MODE: u32 = 0o755;

    fn every_entry_around_the_user() -> impl Iterator<Item = EntryFacts> {
        ENTRY_KINDS.into_iter().flat_map(|kind| {
            OWNERS_AROUND_THE_USER.into_iter().flat_map(move |owner| {
                (0..PERMISSION_BIT_COMBINATIONS).map(move |permission_bits| EntryFacts {
                    kind,
                    owner_user_id: owner,
                    permission_bits,
                })
            })
        })
    }

    #[test]
    fn spec_3_3_an_existing_entry_is_a_usable_directory_exactly_when_it_is_the_users_and_grants_nothing_beyond_its_owner_runtime_directory_is_private_or_refused()
     {
        for facts in every_entry_around_the_user() {
            let usable = facts.kind == EntryKind::Directory
                && facts.owner_user_id == THE_USER
                && facts.permission_bits & GROUP_AND_OTHER_PERMISSION_BITS == 0;
            assert_eq!(
                directory_finding(facts, THE_USER).is_none(),
                usable,
                "{facts:?}"
            );
        }
    }

    #[test]
    fn spec_3_12_an_existing_entry_is_a_trusted_state_file_exactly_when_it_is_a_regular_file_of_the_user_runtime_directory_is_private_or_refused()
     {
        for facts in every_entry_around_the_user() {
            let trusted = facts.kind == EntryKind::RegularFile && facts.owner_user_id == THE_USER;
            assert_eq!(
                state_file_finding(facts, THE_USER).is_none(),
                trusted,
                "{facts:?}"
            );
        }
    }

    #[test]
    fn spec_3_3_a_directory_of_another_user_names_both_users_runtime_directory_is_private_or_refused()
     {
        let refusal = require_private_directory_of(
            THE_USER,
            Path::new("/tmp/estafette-1000"),
            EntryFacts {
                kind: EntryKind::Directory,
                owner_user_id: 0,
                permission_bits: 0o700,
            },
        )
        .unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "/tmp/estafette-1000 already exists and is owned by user 0, not by user 1000; it must be a directory of yours with no access for group or other"
        );
    }

    fn scratch_directory(label: &str) -> PathBuf {
        crate::fresh_directory::fresh_directory_in(
            &std::env::temp_dir(),
            &format!(
                "estafette-bridge-claude-owned-{label}-{}",
                std::process::id()
            ),
        )
        .unwrap()
    }

    fn this_user() -> u32 {
        process_user_id().unwrap()
    }

    #[test]
    fn the_process_user_is_the_owner_of_a_file_the_process_creates_runtime_directory_is_private_or_refused()
     {
        let directory = scratch_directory("process-user");
        let created = directory.join("created-by-this-process");
        fs::write(&created, b"").unwrap();

        assert_eq!(this_user(), fs::metadata(&created).unwrap().uid());
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    fn refusal_inside(failure: &io::Error) -> &StateFileRefused {
        failure
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<StateFileRefused>())
            .unwrap()
    }

    #[test]
    fn spec_3_12_a_write_replaces_the_users_own_state_file_with_mode_0600_and_leaves_no_staging_file()
     {
        let directory = scratch_directory("replace");
        let state = directory.join("count");
        let staging = directory.join("count.0.staging");
        fs::write(&state, "older").unwrap();
        fs::set_permissions(&state, Permissions::from_mode(LOOSE_FILE_MODE)).unwrap();

        write_state_file(this_user(), &state, &staging, STATE_TEXT).unwrap();

        assert_eq!(fs::read_to_string(&state).unwrap(), STATE_TEXT);
        assert_eq!(
            fs::metadata(&state).unwrap().permissions().mode() & PERMISSION_BITS,
            PRIVATE_FILE_MODE
        );
        assert!(!staging.exists());
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn spec_3_12_a_write_never_follows_a_symbolic_link_at_the_staging_name_runtime_directory_is_private_or_refused()
     {
        let directory = scratch_directory("staging-link");
        let victim = directory.join("victim");
        let staging = directory.join("count.0.staging");
        fs::write(&victim, VICTIM_TEXT).unwrap();
        symlink(&victim, &staging).unwrap();

        let failure = write_state_file(this_user(), &directory.join("count"), &staging, STATE_TEXT)
            .unwrap_err();

        assert_eq!(failure.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&victim).unwrap(), VICTIM_TEXT);
        assert!(
            fs::symlink_metadata(&staging)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!directory.join("count").exists());
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn spec_3_12_a_write_never_writes_through_a_regular_file_planted_at_the_staging_name_runtime_directory_is_private_or_refused()
     {
        let directory = scratch_directory("staging-file");
        let planted = directory.join("count.0.staging");
        fs::write(&planted, VICTIM_TEXT).unwrap();

        let failure = write_state_file(this_user(), &directory.join("count"), &planted, STATE_TEXT)
            .unwrap_err();

        assert_eq!(failure.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&planted).unwrap(), VICTIM_TEXT);
        assert!(!directory.join("count").exists());
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn spec_3_12_a_write_refuses_a_symbolic_link_at_the_final_name_and_leaves_it_and_its_target_alone_runtime_directory_is_private_or_refused()
     {
        let directory = scratch_directory("final-link");
        let victim = directory.join("victim");
        let state = directory.join("count");
        let staging = directory.join("count.0.staging");
        fs::write(&victim, VICTIM_TEXT).unwrap();
        symlink(&victim, &state).unwrap();

        let failure = write_state_file(this_user(), &state, &staging, STATE_TEXT).unwrap_err();

        assert_eq!(
            refusal_inside(&failure).to_string(),
            format!(
                "{} is a symbolic link; a state file of the bridge must be a regular file of yours",
                state.display()
            )
        );
        assert_eq!(fs::read_to_string(&victim).unwrap(), VICTIM_TEXT);
        assert!(
            fs::symlink_metadata(&state)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!staging.exists());
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn spec_3_12_a_write_refuses_a_directory_at_the_final_name_runtime_directory_is_private_or_refused()
     {
        let directory = scratch_directory("final-directory");
        let state = directory.join("count");
        let staging = directory.join("count.0.staging");
        fs::create_dir(&state).unwrap();

        let failure = write_state_file(this_user(), &state, &staging, STATE_TEXT).unwrap_err();

        assert_eq!(
            refusal_inside(&failure).found,
            StateFileFinding::NotARegularFile
        );
        assert!(state.is_dir());
        assert!(!staging.exists());
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn spec_3_12_a_read_takes_the_users_regular_file_and_refuses_a_symbolic_link_to_one_runtime_directory_is_private_or_refused()
     {
        let directory = scratch_directory("read");
        let regular = directory.join("count");
        let linked = directory.join("linked-count");
        fs::write(&regular, STATE_TEXT).unwrap();
        symlink(&regular, &linked).unwrap();
        let user_id = fs::metadata(&regular).unwrap().uid();

        assert_eq!(read_state_file(&regular, user_id).unwrap(), STATE_TEXT);
        let failure = read_state_file(&linked, user_id).unwrap_err();

        assert_eq!(
            refusal_inside(&failure).found,
            StateFileFinding::SymbolicLink
        );
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn spec_3_12_a_read_refuses_a_file_swapped_for_another_between_its_inspection_and_its_opening_runtime_directory_is_private_or_refused()
     {
        let directory = scratch_directory("read-swapped");
        let inspected = directory.join("count");
        let swapped_in = directory.join("planted");
        fs::write(&inspected, STATE_TEXT).unwrap();
        fs::write(&swapped_in, VICTIM_TEXT).unwrap();
        let user_id = fs::metadata(&inspected).unwrap().uid();

        let failure = read_state_file_opened_by(&inspected, user_id, |_inspected: &Path| {
            File::open(&swapped_in)
        })
        .unwrap_err();

        assert_eq!(
            refusal_inside(&failure).found,
            StateFileFinding::ReplacedWhileOpened
        );
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn spec_3_12_a_read_refuses_a_file_of_another_user_and_a_directory_runtime_directory_is_private_or_refused()
     {
        let directory = scratch_directory("read-foreign");
        let regular = directory.join("count");
        fs::write(&regular, STATE_TEXT).unwrap();
        let user_id = fs::metadata(&regular).unwrap().uid();
        let another_user_id = user_id.wrapping_add(1);

        let foreign = read_state_file(&regular, another_user_id).unwrap_err();
        let not_a_file = read_state_file(&directory, user_id).unwrap_err();

        assert_eq!(
            refusal_inside(&foreign).found,
            StateFileFinding::OwnedByAnotherUser {
                owner: user_id,
                user: another_user_id
            }
        );
        assert_eq!(
            refusal_inside(&not_a_file).found,
            StateFileFinding::NotARegularFile
        );
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    fn directory_refusal_inside(failure: &io::Error) -> &ExistingDirectoryRefused {
        failure
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<ExistingDirectoryRefused>())
            .unwrap()
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & PERMISSION_BITS
    }

    #[test]
    fn bridge_directories_are_private_or_refused_a_directory_made_with_its_missing_parents_is_private_to_the_user()
     {
        let directory = scratch_directory("create-private");
        let created = directory.join("missing").join("created");

        create_private_directory(this_user(), &created).unwrap();

        assert_eq!(mode_of(&created), PRIVATE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn bridge_directories_are_private_or_refused_an_existing_private_directory_is_used_as_it_stands()
     {
        let directory = scratch_directory("create-existing-private");
        let existing = directory.join("existing");
        fs::create_dir(&existing).unwrap();
        fs::set_permissions(&existing, Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();

        create_private_directory(this_user(), &existing).unwrap();

        assert_eq!(mode_of(&existing), PRIVATE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn bridge_directories_are_private_or_refused_an_existing_loose_or_foreign_directory_is_refused_and_left_as_it_stands()
     {
        let directory = scratch_directory("create-existing-loose");
        let loose = directory.join("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, Permissions::from_mode(LOOSE_DIRECTORY_MODE)).unwrap();
        let another_user_id = this_user().wrapping_add(1);

        let loose_failure = create_private_directory(this_user(), &loose).unwrap_err();
        let foreign_failure = create_private_directory(another_user_id, &loose).unwrap_err();

        assert_eq!(
            directory_refusal_inside(&loose_failure).found,
            DirectoryFinding::GrantsAccessBeyondOwner {
                permission_bits: LOOSE_DIRECTORY_MODE
            }
        );
        assert_eq!(
            directory_refusal_inside(&foreign_failure).found,
            DirectoryFinding::OwnedByAnotherUser {
                owner: this_user(),
                user: another_user_id
            }
        );
        assert_eq!(mode_of(&loose), LOOSE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn bridge_directories_are_private_or_refused_a_mode_is_never_set_through_a_symbolic_link() {
        let directory = scratch_directory("mode-through-link");
        let target = directory.join("target");
        let named = directory.join("named");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, Permissions::from_mode(LOOSE_DIRECTORY_MODE)).unwrap();
        symlink(&target, &named).unwrap();

        let failure =
            make_directory_private_opened_by(this_user(), &named, |path: &Path| File::open(path))
                .unwrap_err();

        assert_eq!(
            directory_refusal_inside(&failure).found,
            DirectoryFinding::SymbolicLink
        );
        assert_eq!(mode_of(&target), LOOSE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn bridge_directories_are_private_or_refused_a_mode_is_never_set_on_a_directory_swapped_in_after_the_inspection()
     {
        let directory = scratch_directory("mode-on-swapped");
        let inspected = directory.join("inspected");
        let swapped_in = directory.join("swapped-in");
        fs::create_dir(&inspected).unwrap();
        fs::create_dir(&swapped_in).unwrap();
        fs::set_permissions(&inspected, Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
        fs::set_permissions(&swapped_in, Permissions::from_mode(LOOSE_DIRECTORY_MODE)).unwrap();

        let failure =
            make_directory_private_opened_by(this_user(), &inspected, |_inspected: &Path| {
                File::open(&swapped_in)
            })
            .unwrap_err();

        assert_eq!(
            directory_refusal_inside(&failure).found,
            DirectoryFinding::ReplacedWhileOpened
        );
        assert_eq!(mode_of(&swapped_in), LOOSE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    const OWNER_WRITE_AND_SEARCH_ONLY_MODE: u32 = 0o300;

    #[test]
    fn bridge_directories_are_private_or_refused_a_directory_made_unreadable_by_the_umask_is_still_made_private()
     {
        let directory = scratch_directory("unreadable-made-private");
        let unreadable = directory.join("unreadable");
        fs::create_dir(&unreadable).unwrap();
        fs::set_permissions(
            &unreadable,
            Permissions::from_mode(OWNER_WRITE_AND_SEARCH_ONLY_MODE),
        )
        .unwrap();

        make_directory_private_opened_by(this_user(), &unreadable, |path: &Path| File::open(path))
            .unwrap();

        assert_eq!(mode_of(&unreadable), PRIVATE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn bridge_directories_are_private_or_refused_a_mode_is_never_set_through_a_symbolic_link_when_the_open_is_denied()
     {
        let directory = scratch_directory("denied-open-link");
        let target = directory.join("target");
        let named = directory.join("named");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, Permissions::from_mode(LOOSE_DIRECTORY_MODE)).unwrap();
        symlink(&target, &named).unwrap();

        let failure = make_directory_private_opened_by(this_user(), &named, |_named: &Path| {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();

        assert_eq!(
            directory_refusal_inside(&failure).found,
            DirectoryFinding::SymbolicLink
        );
        assert_eq!(mode_of(&target), LOOSE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }
}
