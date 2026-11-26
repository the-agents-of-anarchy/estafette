use std::io;
use std::process::Output;

use super::{KeptAs, KeptSecrets, fail_unless_panicking};

/// Why a recipe run did not succeed: it ran and exited in failure, or it could not run.
pub enum RecipeFailure<'ran> {
    Failed(&'ran Output),
    Unrunnable(&'ran io::Error),
}

impl RecipeFailure<'_> {
    /// What a printed line says of it: a failed recipe's line made by `recipe_failure` with the secrets, or the error that kept it from running.
    pub fn printed(&self, secrets: Option<&KeptSecrets>) -> String {
        match self {
            Self::Failed(output) => recipe_failure(output, secrets),
            Self::Unrunnable(failure) => failure.to_string(),
        }
    }

    /// What a kept mark says of it, a failed recipe's standard error whole, so the writer of kept files searches it and notes what it redacts.
    pub(super) fn marked(&self) -> String {
        match self {
            Self::Failed(output) => format!(
                "{}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
            Self::Unrunnable(failure) => failure.to_string(),
        }
    }
}

/// The output of a recipe run that succeeded, or why it did not: it failed, or it could not run.
pub fn succeeded(ran: &io::Result<Output>) -> Result<&Output, RecipeFailure<'_>> {
    match ran {
        Ok(output) if output.status.success() => Ok(output),
        Ok(output) => Err(RecipeFailure::Failed(output)),
        Err(failure) => Err(RecipeFailure::Unrunnable(failure)),
    }
}

/// What a printed line says of a recipe that ran and failed: its exit status and its standard error searched for the secrets, or, when no secrets stand, that it failed, its standard error left out.
pub fn recipe_failure(output: &Output, secrets: Option<&KeptSecrets>) -> String {
    secrets.map_or_else(
        || {
            format!(
                "{}: the recipe failed; its standard error is left out, since no secrets stand to search it for",
                output.status
            )
        },
        |secrets| {
            let (searched, _redacted_lines) =
                secrets.redacted(&String::from_utf8_lossy(&output.stderr), KeptAs::Evidence);
            format!("{}: {searched}", output.status)
        },
    )
}

/// Fails a test that was not failing, saying what was left undone and how the recipe failed as `recipe_failure` prints it with the secrets, when a recipe run from a drop failed or could not run, and prints that instead on a test already failing, so no drop panics on a thread already panicking.
pub fn fail_unless_succeeded(
    ran: &io::Result<Output>,
    left_undone: &str,
    secrets: Option<&KeptSecrets>,
) {
    if let Err(failure) = succeeded(ran) {
        fail_unless_panicking(&format!("{left_undone}: {}", failure.printed(secrets)));
    }
}
