use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// How many numbered names follow the plain name before the directory is refused: chosen, not measured, far above what the runs of one machine leave.
pub const NUMBERED_NAMES_TRIED: u32 = 100;

/// Creates a directory under `parent` that did not exist before the call, named `name` or, where that name is taken, `name` followed by the first free number, failing with the path and the cause.
pub fn fresh_directory_in(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let candidates = std::iter::once(name.to_owned())
        .chain((1..=NUMBERED_NAMES_TRIED).map(|number| format!("{name}-{number}")));
    for candidate in candidates {
        let path = parent.join(candidate);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("{}: {error}", path.display()),
                ));
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "{}: every numbered name of {name} is taken",
            parent.display()
        ),
    ))
}
