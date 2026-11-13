use std::fs::{self, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::UNIX_EPOCH;

use super::{StoredFile, TraceClocks, TraceEffects, TraceFile};

const PRIVATE_FILE_MODE: u32 = 0o600;

impl TraceEffects {
    /// The effects on this machine's filesystem and threads, every line reading `clocks`.
    #[must_use]
    pub fn of_the_operating_system(clocks: TraceClocks) -> Self {
        Self {
            clocks,
            create_file: Arc::new(create_private_file),
            rename_file: Arc::new(|from: &Path, to: &Path| fs::rename(from, to)),
            list_files: Arc::new(stored_files_in),
            remove_file: Arc::new(|path: &Path| fs::remove_file(path)),
            spawn_thread: Arc::new(spawn_named_thread),
        }
    }
}

impl TraceFile for File {
    fn set_length(&mut self, length: u64) -> io::Result<()> {
        self.set_len(length)
    }
}

/// Creates the file new, readable and writable by its owner alone whatever the umask, written from its start.
fn create_private_file(path: &Path) -> io::Result<Box<dyn TraceFile>> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)?;
    fs::set_permissions(path, Permissions::from_mode(PRIVATE_FILE_MODE))?;
    Ok(Box::new(file))
}

/// Every entry of the directory with its size and when it was last written, a time before the Unix epoch read as the epoch, and an entry removed while the directory is read left out.
fn stored_files_in(directory: &Path) -> io::Result<Vec<StoredFile>> {
    fs::read_dir(directory)?
        .map(|entry| -> io::Result<Option<StoredFile>> {
            let entry = entry?;
            let metadata = match entry.metadata() {
                Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
                read => read?,
            };
            let written_ms = metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                });
            Ok(Some(StoredFile {
                name: entry.file_name(),
                bytes: metadata.len(),
                written_ms,
            }))
        })
        .filter_map(Result::transpose)
        .collect()
}

/// Runs the work on a new thread of that name.
fn spawn_named_thread(name: &str, work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .map(drop)
}
