use std::fmt;
use std::fs::{self, File, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::entry::{EntryFacts, EntryKind, UntrustedEntry, same_entry};
use crate::modes::{GROUP_AND_OTHER_PERMISSION_BITS, OWNER_ONLY_DIRECTORY_MODE};

/// What stops an existing entry from being used as a private directory of the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryFinding {
    Untrusted(UntrustedEntry),
    NotADirectory,
    GrantsAccessBeyondOwner { permission_bits: u32 },
}

impl fmt::Display for DirectoryFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Untrusted(untrusted) => untrusted.fmt(formatter),
            Self::NotADirectory => write!(formatter, "not a directory"),
            Self::GrantsAccessBeyondOwner { permission_bits } => write!(
                formatter,
                "mode {permission_bits:04o}, which grants access to group or other"
            ),
        }
    }
}

/// Says what stops the entry from being a directory of the user that grants nothing to group or other, none when nothing does.
#[must_use]
const fn directory_finding(facts: EntryFacts, user_id: u32) -> Option<DirectoryFinding> {
    match facts.kind {
        EntryKind::SymbolicLink => Some(DirectoryFinding::Untrusted(UntrustedEntry::SymbolicLink)),
        EntryKind::RegularFile | EntryKind::Other => Some(DirectoryFinding::NotADirectory),
        EntryKind::Directory if facts.owner_user_id != user_id => Some(
            DirectoryFinding::Untrusted(UntrustedEntry::OwnedByAnotherUser {
                owner: facts.owner_user_id,
                user: user_id,
            }),
        ),
        EntryKind::Directory if facts.permission_bits & GROUP_AND_OTHER_PERMISSION_BITS != 0 => {
            Some(DirectoryFinding::GrantsAccessBeyondOwner {
                permission_bits: facts.permission_bits,
            })
        }
        EntryKind::Directory => None,
    }
}

/// An existing directory that is refused, with the directory and what was found.
#[derive(Debug, Eq, PartialEq)]
pub struct ExistingDirectoryRefused {
    pub path: PathBuf,
    pub found: DirectoryFinding,
}

impl ExistingDirectoryRefused {
    fn of(path: &Path, found: DirectoryFinding) -> Self {
        Self {
            path: path.to_path_buf(),
            found,
        }
    }
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

impl From<ExistingDirectoryRefused> for io::Error {
    fn from(refusal: ExistingDirectoryRefused) -> Self {
        Self::new(io::ErrorKind::PermissionDenied, refusal)
    }
}

/// Refuses the entry the facts describe unless it is a private directory of the user, naming the path.
///
/// # Errors
///
/// `ExistingDirectoryRefused` naming the path and what was found.
pub fn require_private_directory(
    user_id: u32,
    path: &Path,
    facts: EntryFacts,
) -> Result<(), ExistingDirectoryRefused> {
    directory_finding(facts, user_id).map_or(Ok(()), |found| {
        Err(ExistingDirectoryRefused::of(path, found))
    })
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
        .mode(OWNER_ONLY_DIRECTORY_MODE)
        .create(path)
    {
        Ok(()) => make_directory_private_opened_by(user_id, path, |path: &Path| File::open(path)),
        Err(failure) if failure.kind() == io::ErrorKind::AlreadyExists => Ok(
            require_private_directory(user_id, path, EntryFacts::at(path)?)?,
        ),
        Err(failure) => Err(failure),
    }
}

/// Sets the directory private through the descriptor the open gave, refusing an entry that is not the user's private directory or was swapped after the open.
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
    require_private_directory(user_id, path, EntryFacts::of(&named))?;
    if !same_entry(&opened, &named) {
        return Err(ExistingDirectoryRefused::of(
            path,
            DirectoryFinding::Untrusted(UntrustedEntry::ReplacedWhileOpened),
        )
        .into());
    }
    directory.set_permissions(Permissions::from_mode(OWNER_ONLY_DIRECTORY_MODE))
}

/// Sets the directory private by its name when the open was denied, refusing an entry that is not a private directory of the user.
fn make_unreadable_directory_private_by_name(user_id: u32, path: &Path) -> io::Result<()> {
    let named = fs::symlink_metadata(path)?;
    require_private_directory(user_id, path, EntryFacts::of(&named))?;
    fs::set_permissions(path, Permissions::from_mode(OWNER_ONLY_DIRECTORY_MODE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::process_user_id;
    use crate::modes::PERMISSION_BITS_MASK;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    const THE_USER: u32 = 1000;
    const OWNERS_AROUND_THE_USER: [u32; 4] = [0, 999, 1000, 1001];
    const PERMISSION_BIT_COMBINATIONS: u32 = 0o1000;
    const ENTRY_KINDS: [EntryKind; 4] = [
        EntryKind::Directory,
        EntryKind::RegularFile,
        EntryKind::SymbolicLink,
        EntryKind::Other,
    ];
    const LOOSE_DIRECTORY_MODE: u32 = 0o755;
    const OWNER_WRITE_AND_SEARCH_ONLY_MODE: u32 = 0o300;
    const MODES_OPEN_TO_GROUP_ALONE: [u32; 3] = [0o070, 0o750, 0o040];
    const MODES_OPEN_TO_OTHER_ALONE: [u32; 3] = [0o007, 0o705, 0o004];
    const MODES_CLOSED_TO_GROUP_AND_OTHER: [u32; 3] = [0o700, 0o500, 0o000];

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

    fn scratch_directory(label: &str) -> PathBuf {
        crate::this_process_directory::fresh_directory_of_this_process_in(
            &std::env::temp_dir(),
            &format!("estafette-private-entry-{label}"),
        )
        .unwrap()
    }

    fn this_user() -> u32 {
        process_user_id().unwrap()
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & PERMISSION_BITS_MASK
    }

    fn refusal_inside(failure: &io::Error) -> &ExistingDirectoryRefused {
        failure
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<ExistingDirectoryRefused>())
            .unwrap()
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
    fn spec_3_3_a_directory_open_to_group_alone_or_to_other_alone_is_refused_and_one_closed_to_both_is_usable_runtime_directory_is_private_or_refused()
     {
        let directory_with_mode = |permission_bits| EntryFacts {
            kind: EntryKind::Directory,
            owner_user_id: THE_USER,
            permission_bits,
        };
        for mode in MODES_OPEN_TO_GROUP_ALONE
            .into_iter()
            .chain(MODES_OPEN_TO_OTHER_ALONE)
        {
            assert_eq!(
                directory_finding(directory_with_mode(mode), THE_USER),
                Some(DirectoryFinding::GrantsAccessBeyondOwner {
                    permission_bits: mode
                }),
                "mode {mode:04o}"
            );
        }
        for mode in MODES_CLOSED_TO_GROUP_AND_OTHER {
            assert_eq!(
                directory_finding(directory_with_mode(mode), THE_USER),
                None,
                "mode {mode:04o}"
            );
        }
    }

    #[test]
    fn spec_3_3_a_foreign_owner_is_named_with_the_user_and_ranks_below_the_kind_and_above_the_mode_runtime_directory_is_private_or_refused()
     {
        let foreign_and_loose = EntryFacts {
            kind: EntryKind::Directory,
            owner_user_id: 0,
            permission_bits: 0o777,
        };

        assert_eq!(
            directory_finding(foreign_and_loose, THE_USER),
            Some(DirectoryFinding::Untrusted(
                UntrustedEntry::OwnedByAnotherUser {
                    owner: 0,
                    user: THE_USER
                }
            ))
        );
        assert_eq!(
            directory_finding(
                EntryFacts {
                    kind: EntryKind::SymbolicLink,
                    ..foreign_and_loose
                },
                THE_USER
            ),
            Some(DirectoryFinding::Untrusted(UntrustedEntry::SymbolicLink))
        );
        assert_eq!(
            DirectoryFinding::Untrusted(UntrustedEntry::OwnedByAnotherUser {
                owner: 0,
                user: 1000
            })
            .to_string(),
            "owned by user 0, not by user 1000"
        );
    }

    #[test]
    fn private_directories_are_private_or_refused_a_directory_made_with_its_missing_parents_is_private_to_the_user()
     {
        let directory = scratch_directory("create-private");
        let created = directory.join("missing").join("created");

        create_private_directory(this_user(), &created).unwrap();

        assert_eq!(mode_of(&created), OWNER_ONLY_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn private_directories_are_private_or_refused_an_existing_private_directory_is_used_as_it_stands()
     {
        let directory = scratch_directory("create-existing-private");
        let existing = directory.join("existing");
        fs::create_dir(&existing).unwrap();
        fs::set_permissions(&existing, Permissions::from_mode(OWNER_ONLY_DIRECTORY_MODE)).unwrap();

        create_private_directory(this_user(), &existing).unwrap();

        assert_eq!(mode_of(&existing), OWNER_ONLY_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn private_directories_are_private_or_refused_an_existing_loose_or_foreign_directory_is_refused_and_left_as_it_stands()
     {
        let directory = scratch_directory("create-existing-loose");
        let loose = directory.join("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, Permissions::from_mode(LOOSE_DIRECTORY_MODE)).unwrap();
        let another_user_id = this_user().wrapping_add(1);

        let loose_failure = create_private_directory(this_user(), &loose).unwrap_err();
        let foreign_failure = create_private_directory(another_user_id, &loose).unwrap_err();

        assert_eq!(
            refusal_inside(&loose_failure).found,
            DirectoryFinding::GrantsAccessBeyondOwner {
                permission_bits: LOOSE_DIRECTORY_MODE
            }
        );
        assert_eq!(
            refusal_inside(&foreign_failure).found,
            DirectoryFinding::Untrusted(UntrustedEntry::OwnedByAnotherUser {
                owner: this_user(),
                user: another_user_id
            })
        );
        assert_eq!(mode_of(&loose), LOOSE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn private_directories_are_private_or_refused_a_mode_is_never_set_through_a_symbolic_link() {
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
            refusal_inside(&failure).found,
            DirectoryFinding::Untrusted(UntrustedEntry::SymbolicLink)
        );
        assert_eq!(mode_of(&target), LOOSE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn private_directories_are_private_or_refused_a_mode_is_never_set_on_a_directory_swapped_in_after_the_inspection()
     {
        let directory = scratch_directory("mode-on-swapped");
        let inspected = directory.join("inspected");
        let swapped_in = directory.join("swapped-in");
        fs::create_dir(&inspected).unwrap();
        fs::create_dir(&swapped_in).unwrap();
        fs::set_permissions(
            &inspected,
            Permissions::from_mode(OWNER_ONLY_DIRECTORY_MODE),
        )
        .unwrap();
        fs::set_permissions(&swapped_in, Permissions::from_mode(LOOSE_DIRECTORY_MODE)).unwrap();

        let failure =
            make_directory_private_opened_by(this_user(), &inspected, |_inspected: &Path| {
                File::open(&swapped_in)
            })
            .unwrap_err();

        assert_eq!(
            refusal_inside(&failure).found,
            DirectoryFinding::Untrusted(UntrustedEntry::ReplacedWhileOpened)
        );
        assert_eq!(mode_of(&swapped_in), LOOSE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn private_directories_are_private_or_refused_a_directory_made_unreadable_by_the_umask_is_still_made_private()
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

        assert_eq!(mode_of(&unreadable), OWNER_ONLY_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    #[test]
    fn private_directories_are_private_or_refused_a_mode_is_never_set_through_a_symbolic_link_when_the_open_is_denied()
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
            refusal_inside(&failure).found,
            DirectoryFinding::Untrusted(UntrustedEntry::SymbolicLink)
        );
        assert_eq!(mode_of(&target), LOOSE_DIRECTORY_MODE);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }
}
