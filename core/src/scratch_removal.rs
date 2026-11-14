use std::path::Path;

/// Removes the scratch path when a test ends, failing the test with the path and the cause unless it is already failing, in which case the cause goes to standard error.
pub fn remove_when_the_test_ends(path: &Path) {
    if let Err(failure) = crate::remove_scratch::remove_scratch(path) {
        assert!(std::thread::panicking(), "{failure}");
        eprintln!("{failure}");
    }
}
