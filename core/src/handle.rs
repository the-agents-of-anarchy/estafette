use std::fmt;
use std::str::FromStr;

const MAX_HANDLE_LENGTH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleError {
    NotAHandle,
}

impl fmt::Display for HandleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAHandle => formatter.write_str("handle must match ^[a-z][a-z0-9-]{0,31}$"),
        }
    }
}

impl std::error::Error for HandleError {}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Handle(String);

impl FromStr for Handle {
    type Err = HandleError;

    fn from_str(text: &str) -> Result<Self, HandleError> {
        if matches_handle_grammar(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(HandleError::NotAHandle)
        }
    }
}

impl Handle {
    /// Returns the handle text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn matches_handle_grammar(text: &str) -> bool {
    let mut characters = text.chars();
    text.len() <= MAX_HANDLE_LENGTH
        && characters
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|rest| rest.is_ascii_lowercase() || rest.is_ascii_digit() || rest == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_grammar_accepts_lowercase_start_then_lowercase_digits_hyphens_up_to_32_spec_3_1() {
        let longest = format!("a{}", "9-z".repeat(10) + "0");
        assert_eq!(longest.len(), MAX_HANDLE_LENGTH);
        for accepted in ["a", "z9", "a-", "a--b-0", "h-02", longest.as_str()] {
            assert_eq!(
                accepted.parse::<Handle>().as_ref().map(Handle::as_str),
                Ok(accepted)
            );
        }
    }

    #[test]
    fn handle_grammar_refuses_everything_else_spec_3_1() {
        let too_long = "a".repeat(MAX_HANDLE_LENGTH + 1);
        for refused in [
            "",
            "9a",
            "-a",
            "A",
            "aB",
            "a_b",
            "a b",
            "a.b",
            "\u{e9}a",
            "a\u{e9}",
            too_long.as_str(),
        ] {
            assert_eq!(
                refused.parse::<Handle>(),
                Err(HandleError::NotAHandle),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn refusal_names_the_accepted_set() {
        assert_eq!(
            HandleError::NotAHandle.to_string(),
            "handle must match ^[a-z][a-z0-9-]{0,31}$"
        );
    }
}
