use std::io;
use std::path::PathBuf;

pub mod fresh_directory;

/// A directory of the temp directory that did not exist before the call, named for the prefix, the test and the process.
pub fn scratch_root(prefix: &str, test_name: &str) -> io::Result<PathBuf> {
    fresh_directory::fresh_directory_in(
        &std::env::temp_dir(),
        &format!("{prefix}-{test_name}-{}", std::process::id()),
    )
}
