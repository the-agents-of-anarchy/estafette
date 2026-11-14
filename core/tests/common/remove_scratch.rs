use std::fs;
use std::io;
use std::path::Path;

/// Removes the scratch directory or file with everything beneath it, failing with the path and the cause.
pub fn remove_scratch(path: &Path) -> io::Result<()> {
    let removed = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    removed.map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}
