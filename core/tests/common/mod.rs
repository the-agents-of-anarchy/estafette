use std::fs;
use std::io;
use std::path::PathBuf;

pub fn scratch_root(prefix: &str, test_name: &str) -> io::Result<PathBuf> {
    let root = std::env::temp_dir().join(format!("{prefix}-{test_name}-{}", std::process::id()));
    drop(fs::remove_dir_all(&root));
    fs::create_dir_all(&root)?;
    Ok(root)
}
