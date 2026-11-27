use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::modes::OWNER_ONLY_FILE_MODE;

const TEMPORARY_FILE_SUFFIX: &str = "tmp";

/// Writes the contents to a new uniquely named file readable by its owner alone, syncs it, and renames it over the path, removing it when that fails.
///
/// # Errors
///
/// Fails with the operating system's error when the file cannot be written or renamed, naming the temporary file when its removal fails too.
pub fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let (file, temporary_path) = create_unique_temporary_file(path, OWNER_ONLY_FILE_MODE)?;
    rename_over_after_syncing(file, &temporary_path, path, contents).map_err(|failure| {
        with_cleanup_outcome(
            failure,
            &temporary_path,
            remove_file_if_present(&temporary_path),
        )
    })
}

/// Writes the contents whole to a new file of the given mode at the path, never replacing a file that exists.
///
/// # Errors
///
/// Fails with `AlreadyExists` when the path exists, with the operating system's error when the file cannot be written or linked, and naming the temporary file when `remove_temporary` fails to remove it.
pub fn write_new_file_atomically(
    path: &Path,
    contents: &str,
    mode: u32,
    remove_temporary: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let (file, temporary_path) = create_unique_temporary_file(path, mode)?;
    let linked = link_after_syncing(file, &temporary_path, path, contents);
    let removal = remove_temporary(&temporary_path);
    match linked {
        Ok(()) => removal.map_err(|cleanup| {
            with_cleanup_outcome(
                io::Error::other(format!("{} was written", path.display())),
                &temporary_path,
                Err(cleanup),
            )
        }),
        Err(failure) => Err(with_cleanup_outcome(failure, &temporary_path, removal)),
    }
}

/// The failure, naming the file left behind and why its removal failed when the cleanup failed too, and keeping the failure's kind.
#[must_use]
pub fn with_cleanup_outcome(
    failure: io::Error,
    left_behind: &Path,
    cleanup: io::Result<()>,
) -> io::Error {
    match cleanup {
        Ok(()) => failure,
        Err(cleanup_failure) => io::Error::new(
            failure.kind(),
            format!(
                "{failure}; {} was left behind: {cleanup_failure}",
                left_behind.display()
            ),
        ),
    }
}

/// Creates a new file whose mode is exactly the given one whatever the umask, failing with `AlreadyExists` when the path exists.
fn create_new_file_with_mode(path: &Path, mode: u32) -> io::Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.set_permissions(Permissions::from_mode(mode))?;
    Ok(file)
}

/// Creates a new file readable and writable by its owner alone whatever the umask, failing with `AlreadyExists` when the path exists.
///
/// # Errors
///
/// Fails with the operating system's error when the file cannot be created or its mode set, `AlreadyExists` when the path exists.
pub fn create_new_owner_only_file(path: &Path) -> io::Result<File> {
    create_new_file_with_mode(path, OWNER_ONLY_FILE_MODE)
}

/// Creates a new file of the given mode at the first free numbered temporary name beside the path.
fn create_unique_temporary_file(path: &Path, mode: u32) -> io::Result<(File, PathBuf)> {
    for attempt in 0..=u32::MAX {
        let temporary_path =
            path.with_added_extension(format!("{attempt}.{TEMPORARY_FILE_SUFFIX}"));
        match create_new_file_with_mode(&temporary_path, mode) {
            Ok(file) => return Ok((file, temporary_path)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(source),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

/// Writes the contents to the file, syncs it, and renames it over the path.
fn rename_over_after_syncing(
    mut file: File,
    temporary_path: &Path,
    path: &Path,
    contents: &str,
) -> io::Result<()> {
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary_path, path)
}

/// Writes the contents to the file, syncs it, and links it to the path.
fn link_after_syncing(
    mut file: File,
    temporary_path: &Path,
    path: &Path,
    contents: &str,
) -> io::Result<()> {
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::hard_link(temporary_path, path)
}

/// Removes the file, counting a file that is already gone as removed.
///
/// # Errors
///
/// Fails with the operating system's error when the file exists and cannot be removed.
pub fn remove_file_if_present(path: &Path) -> io::Result<()> {
    fs::remove_file(path).or_else(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(source)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::PERMISSION_BITS_MASK;
    use std::ffi::OsString;

    const GROUP_READABLE_FILE_MODE: u32 = 0o644;

    fn scratch_directory(label: &str) -> PathBuf {
        crate::this_process_directory::fresh_directory_of_this_process_in(
            &std::env::temp_dir(),
            &format!("estafette-private-entry-{label}"),
        )
        .unwrap()
    }

    fn names_in(directory: &Path) -> Vec<OsString> {
        let mut names: Vec<_> = directory
            .read_dir()
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    }

    /// Spec 3.3: a write that fails after the temporary file exists leaves no temporary file behind.
    #[test]
    fn failed_write_removes_its_temporary_file() {
        let directory = scratch_directory("debris");
        let blocked_final_path = directory.join("net.toml");
        fs::create_dir(&blocked_final_path).unwrap();

        let outcome = write_atomically(&blocked_final_path, "secret = \"x\"\n");

        assert!(outcome.is_err());
        assert_eq!(names_in(&directory), ["net.toml"]);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    /// Spec 3.3: a failed cleanup names the temporary file it left and keeps the kind of the failure that caused it.
    #[test]
    fn a_failed_cleanup_names_the_file_left_behind_and_keeps_the_failure_kind() {
        let left_behind = Path::new("/state/net.toml.0.tmp");
        let failure = io::Error::from(io::ErrorKind::PermissionDenied);
        let cleanup_failure = io::Error::from(io::ErrorKind::ReadOnlyFilesystem);
        let cleanup_failure_text = cleanup_failure.to_string();

        let outcome = with_cleanup_outcome(failure, left_behind, Err(cleanup_failure));

        assert_eq!(outcome.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            outcome.to_string().ends_with(&format!(
                "/state/net.toml.0.tmp was left behind: {cleanup_failure_text}"
            )),
            "{outcome}"
        );
    }

    /// Spec 3.3: a cleanup that worked leaves the failure as it was.
    #[test]
    fn a_successful_cleanup_leaves_the_failure_unchanged() {
        let failure = io::Error::from(io::ErrorKind::PermissionDenied);
        let failure_text = failure.to_string();

        let outcome = with_cleanup_outcome(failure, Path::new("/state/x.tmp"), Ok(()));

        assert_eq!(outcome.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(outcome.to_string(), failure_text);
    }

    /// Spec 3.5: a new file appears whole at its path with exactly the given mode and no temporary file stays beside it.
    #[test]
    fn a_new_file_is_linked_into_place_whole_with_its_mode_and_no_debris() {
        let directory = scratch_directory("link-new");
        let path = directory.join("card.yaml");

        write_new_file_atomically(
            &path,
            "handle: a\n",
            GROUP_READABLE_FILE_MODE,
            remove_file_if_present,
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "handle: a\n");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & PERMISSION_BITS_MASK,
            GROUP_READABLE_FILE_MODE
        );
        assert_eq!(names_in(&directory), ["card.yaml"]);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    /// Spec 3.5: a temporary file that cannot be removed once the link is made is named in the failure, and the file at the path stays whole.
    #[test]
    fn a_temporary_file_left_after_the_link_is_named_in_the_failure() {
        let directory = scratch_directory("link-leftover");
        let path = directory.join("card.yaml");
        let failing_removal =
            |_leftover: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));

        let failure = write_new_file_atomically(
            &path,
            "handle: a\n",
            GROUP_READABLE_FILE_MODE,
            failing_removal,
        )
        .unwrap_err();

        assert_eq!(fs::read_to_string(&path).unwrap(), "handle: a\n");
        let names = names_in(&directory);
        assert_eq!(names.len(), 2);
        let leftover = directory.join(names.iter().find(|name| *name != "card.yaml").unwrap());
        assert!(
            failure
                .to_string()
                .contains(&format!("{} was left behind", leftover.display())),
            "{failure}"
        );
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }

    /// Spec 3.5: a path that exists is never replaced and no temporary file stays beside it.
    #[test]
    fn an_existing_file_is_never_replaced_by_a_new_file_write() {
        let directory = scratch_directory("link-existing");
        let path = directory.join("card.yaml");
        fs::write(&path, "old\n").unwrap();

        let failure = write_new_file_atomically(
            &path,
            "new\n",
            GROUP_READABLE_FILE_MODE,
            remove_file_if_present,
        )
        .unwrap_err();

        assert_eq!(failure.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&path).unwrap(), "old\n");
        assert_eq!(names_in(&directory), ["card.yaml"]);
        crate::remove_scratch::remove_scratch(&directory).unwrap();
    }
}
