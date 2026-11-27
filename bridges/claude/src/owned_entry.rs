use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use estafette_private_entry::{
    EntryFacts, EntryKind, ExistingDirectoryRefused, UntrustedEntry, create_new_owner_only_file,
    require_private_directory, same_entry,
};

pub use estafette_private_entry::create_private_directory;

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
        Ok(facts) => require_private_directory(user_id, directory, facts)
            .map_err(RuntimeDirectoryProblem::Refused),
    }
}

/// What stops an existing entry from being trusted as one of the bridge's state files.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StateFileFinding {
    Untrusted(UntrustedEntry),
    NotARegularFile,
}

impl fmt::Display for StateFileFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Untrusted(untrusted) => untrusted.fmt(formatter),
            Self::NotARegularFile => write!(formatter, "not a regular file"),
        }
    }
}

/// Says what stops the entry from being a regular file of the user, none when nothing does.
#[must_use]
const fn state_file_finding(facts: EntryFacts, user_id: u32) -> Option<StateFileFinding> {
    match facts.kind {
        EntryKind::SymbolicLink => Some(StateFileFinding::Untrusted(UntrustedEntry::SymbolicLink)),
        EntryKind::Directory | EntryKind::Other => Some(StateFileFinding::NotARegularFile),
        EntryKind::RegularFile if facts.owner_user_id != user_id => Some(
            StateFileFinding::Untrusted(UntrustedEntry::OwnedByAnotherUser {
                owner: facts.owner_user_id,
                user: user_id,
            }),
        ),
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
    if !same_entry(&file.metadata()?, &metadata) {
        return Err(refusal_of(
            path,
            StateFileFinding::Untrusted(UntrustedEntry::ReplacedWhileOpened),
        ));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

/// Writes the text to a new file at the staging path, owner read and write alone, and renames it over the state file unless that is not a regular file of the user.
///
/// # Errors
///
/// Fails when the staging path is taken or the file cannot be written, or with a `StateFileRefused` inside the error when the state file is a symbolic link or another user's; the staging file is then removed, except when its owner-only mode could not be set, which leaves it.
pub fn write_state_file(user_id: u32, path: &Path, staging: &Path, text: &str) -> io::Result<()> {
    let mut staged = create_new_owner_only_file(staging)?;
    let outcome = rename_over_after_writing(&mut staged, user_id, staging, path, text);
    if outcome.is_err() {
        drop(fs::remove_file(staging));
    }
    outcome
}

fn rename_over_after_writing(
    staged: &mut File,
    user_id: u32,
    staging: &Path,
    path: &Path,
    text: &str,
) -> io::Result<()> {
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
    use estafette_private_entry::DirectoryFinding;
    use estafette_private_entry::{OWNER_ONLY_FILE_MODE, PERMISSION_BITS_MASK, process_user_id};
    use std::fs::Permissions;
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

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
        let refusal = require_private_directory(
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
        crate::this_process_directory::fresh_directory_of_this_process_in(
            &std::env::temp_dir(),
            &format!("estafette-bridge-claude-owned-{label}"),
        )
        .unwrap()
    }

    fn this_user() -> u32 {
        process_user_id().unwrap()
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
            fs::metadata(&state).unwrap().permissions().mode() & PERMISSION_BITS_MASK,
            OWNER_ONLY_FILE_MODE
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
            StateFileFinding::Untrusted(UntrustedEntry::SymbolicLink)
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
            StateFileFinding::Untrusted(UntrustedEntry::ReplacedWhileOpened)
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
            StateFileFinding::Untrusted(UntrustedEntry::OwnedByAnotherUser {
                owner: user_id,
                user: another_user_id
            })
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
        fs::metadata(path).unwrap().permissions().mode() & PERMISSION_BITS_MASK
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
            DirectoryFinding::Untrusted(UntrustedEntry::OwnedByAnotherUser {
                owner: this_user(),
                user: another_user_id
            })
        );
        assert_eq!(mode_of(&loose), LOOSE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }
}
