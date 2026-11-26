use std::io;
use std::path::{Path, PathBuf};

use super::fresh_directory::fresh_directory_in;

/// Creates a directory under `parent` that did not exist before the call, named `name` and the id of this process, so a test's scratch root never meets another process's.
pub fn fresh_directory_of_this_process_in(parent: &Path, name: &str) -> io::Result<PathBuf> {
    fresh_directory_in(parent, &format!("{name}-{}", std::process::id()))
}
