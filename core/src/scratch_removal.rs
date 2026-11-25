use std::fmt::Display;
use std::path::Path;

/// Fails the test with the failure unless the test is already failing, in which case the failure goes to standard error.
pub fn fail_the_test_unless_already_failing(failure: &impl Display) {
    assert!(std::thread::panicking(), "{failure}");
    eprintln!("{failure}");
}

/// Removes the scratch path when a test ends, failing the test with the path and the cause unless it is already failing, in which case the cause goes to standard error.
pub fn remove_when_the_test_ends(path: &Path) {
    if let Err(failure) = super::remove_scratch::remove_scratch(path) {
        fail_the_test_unless_already_failing(&failure);
    }
}
