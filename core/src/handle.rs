use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

const MAX_HANDLE_LENGTH: usize = 32;
const MAX_SUFFIX_DIGITS: usize = 20;
const FIRST_ASSIGNED_SUFFIX: u64 = 2;
const SUFFIX_SEPARATOR: char = '-';
const SUFFIX_SEPARATOR_LENGTH: usize = 1;
const MAX_ANNOUNCED_LENGTH: usize = MAX_HANDLE_LENGTH + SUFFIX_SEPARATOR_LENGTH + MAX_SUFFIX_DIGITS;
const MAX_LOCAL_LENGTH: usize = MAX_ANNOUNCED_LENGTH + SUFFIX_SEPARATOR_LENGTH + MAX_SUFFIX_DIGITS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleError {
    NotAHandle,
    NotAnAnnouncedHandle,
    NotALocalHandle,
}

impl fmt::Display for HandleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAHandle => write!(
                formatter,
                "handle must match ^[a-z][a-z0-9-]*$, at most {MAX_HANDLE_LENGTH} characters"
            ),
            Self::NotAnAnnouncedHandle => write!(
                formatter,
                "announced handle must be a handle, or a handle followed by - and an integer of at least {FIRST_ASSIGNED_SUFFIX} with no leading zeros, at most {MAX_ANNOUNCED_LENGTH} characters"
            ),
            Self::NotALocalHandle => write!(
                formatter,
                "local handle must be an announced handle, or an announced handle followed by - and an integer of at least {FIRST_ASSIGNED_SUFFIX} with no leading zeros, at most {MAX_LOCAL_LENGTH} characters"
            ),
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AnnouncedHandle(String);

impl FromStr for AnnouncedHandle {
    type Err = HandleError;

    fn from_str(text: &str) -> Result<Self, HandleError> {
        if matches_announced_grammar(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(HandleError::NotAnAnnouncedHandle)
        }
    }
}

impl AnnouncedHandle {
    /// Returns the announced handle text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocalHandle(String);

impl FromStr for LocalHandle {
    type Err = HandleError;

    fn from_str(text: &str) -> Result<Self, HandleError> {
        if matches_local_grammar(text) {
            Ok(Self(text.to_owned()))
        } else {
            Err(HandleError::NotALocalHandle)
        }
    }
}

impl LocalHandle {
    /// Names an announced handle at this node by the smallest free `h-n`, n of at least 2, when `h` is taken.
    #[must_use]
    pub fn assign(announced: &AnnouncedHandle, taken: &HashSet<Self>) -> Self {
        let mut candidate = Self(announced.0.clone());
        let mut suffix = FIRST_ASSIGNED_SUFFIX;
        while taken.contains(&candidate) {
            candidate = Self(format!("{}{SUFFIX_SEPARATOR}{suffix}", announced.0));
            suffix = suffix.saturating_add(1);
        }
        candidate
    }

    /// Returns the local handle text.
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

fn matches_announced_grammar(text: &str) -> bool {
    text.len() <= MAX_ANNOUNCED_LENGTH
        && (matches_handle_grammar(text) || ends_in_suffix_after(text, matches_handle_grammar))
}

fn matches_local_grammar(text: &str) -> bool {
    text.len() <= MAX_LOCAL_LENGTH
        && (matches_announced_grammar(text)
            || ends_in_suffix_after(text, matches_announced_grammar))
}

fn ends_in_suffix_after(text: &str, matches_base: fn(&str) -> bool) -> bool {
    text.rsplit_once(SUFFIX_SEPARATOR)
        .is_some_and(|(base, suffix)| matches_base(base) && is_suffix_of_at_least_2(suffix))
}

fn is_suffix_of_at_least_2(text: &str) -> bool {
    text.chars().all(|digit| digit.is_ascii_digit())
        && !text.starts_with('0')
        && !text.is_empty()
        && text != "1"
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
    fn each_refusal_names_its_accepted_set_and_length_limit_spec_3_1() {
        for (error, limit) in [
            (HandleError::NotAHandle, MAX_HANDLE_LENGTH),
            (HandleError::NotAnAnnouncedHandle, MAX_ANNOUNCED_LENGTH),
            (HandleError::NotALocalHandle, MAX_LOCAL_LENGTH),
        ] {
            assert!(
                error
                    .to_string()
                    .ends_with(&format!("at most {limit} characters")),
                "{error}"
            );
        }
        assert!(
            HandleError::NotAHandle
                .to_string()
                .contains("^[a-z][a-z0-9-]*$")
        );
        assert!(
            HandleError::NotAnAnnouncedHandle
                .to_string()
                .starts_with("announced handle must be a handle")
        );
        assert!(
            HandleError::NotALocalHandle
                .to_string()
                .starts_with("local handle must be an announced handle")
        );
    }

    #[test]
    fn length_limits_are_the_spec_figures_32_53_and_74_spec_3_1() {
        assert_eq!(MAX_HANDLE_LENGTH, 32);
        assert_eq!(MAX_ANNOUNCED_LENGTH, 53);
        assert_eq!(MAX_LOCAL_LENGTH, 74);
    }

    fn base_handle() -> String {
        "b".repeat(MAX_HANDLE_LENGTH)
    }

    fn longest_local_handle() -> String {
        format!(
            "{}-{}-{}",
            base_handle(),
            twenty_digit_suffix(),
            twenty_digit_suffix()
        )
    }

    fn twenty_digit_suffix() -> String {
        format!("2{}", "0".repeat(MAX_SUFFIX_DIGITS - 1))
    }

    #[test]
    fn announced_handle_is_a_handle_or_a_handle_with_one_suffix_of_at_least_2_up_to_53_spec_3_1() {
        let longest = format!("{}-{}", base_handle(), twenty_digit_suffix());
        assert_eq!(longest.len(), MAX_ANNOUNCED_LENGTH);
        let long_base_with_suffix = format!("{}-2", base_handle());
        for accepted in [
            "h",
            "h-2",
            "h-9",
            "h-10",
            "h-0",
            "h-02",
            "h-1",
            "h-2x",
            "h-2-3",
            base_handle().as_str(),
            long_base_with_suffix.as_str(),
            longest.as_str(),
        ] {
            assert_eq!(
                accepted
                    .parse::<AnnouncedHandle>()
                    .as_ref()
                    .map(AnnouncedHandle::as_str),
                Ok(accepted)
            );
        }
    }

    #[test]
    fn announced_handle_refuses_what_neither_form_covers_spec_3_1() {
        let too_long = format!("{}-{}0", base_handle(), twenty_digit_suffix());
        let leading_zero_after_long_base = format!("{}-02", base_handle());
        let suffix_of_one_after_long_base = format!("{}-1", base_handle());
        let two_suffixes_after_long_base = format!("{}-2-2", base_handle());
        for refused in [
            "",
            "H",
            "9-2",
            "h_2",
            "\u{e9}-2",
            leading_zero_after_long_base.as_str(),
            suffix_of_one_after_long_base.as_str(),
            two_suffixes_after_long_base.as_str(),
            too_long.as_str(),
        ] {
            assert_eq!(
                refused.parse::<AnnouncedHandle>(),
                Err(HandleError::NotAnAnnouncedHandle),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn local_handle_is_an_announced_handle_or_one_more_suffix_up_to_74_spec_3_1() {
        let longest = longest_local_handle();
        assert_eq!(longest.len(), MAX_LOCAL_LENGTH);
        let long_base_with_two_suffixes = format!("{}-2-2", base_handle());
        for accepted in [
            "h",
            "h-2",
            "h-2-2",
            "h-10-11",
            "h-02-2",
            "h-2-3",
            long_base_with_two_suffixes.as_str(),
            longest.as_str(),
        ] {
            assert_eq!(
                accepted
                    .parse::<LocalHandle>()
                    .as_ref()
                    .map(LocalHandle::as_str),
                Ok(accepted)
            );
        }
    }

    #[test]
    fn local_handle_refuses_a_third_suffix_a_bad_suffix_and_more_than_74_characters_spec_3_1() {
        let too_long = format!("{}0", longest_local_handle());
        let bad_inner_suffix_after_long_base = format!("{}-02-2", base_handle());
        let three_suffixes_after_long_base = format!("{}-2-2-2", base_handle());
        let bad_outer_suffix = format!("{}-2-02", base_handle());
        for refused in [
            "",
            "H-2-2",
            three_suffixes_after_long_base.as_str(),
            bad_inner_suffix_after_long_base.as_str(),
            bad_outer_suffix.as_str(),
            too_long.as_str(),
        ] {
            assert_eq!(
                refused.parse::<LocalHandle>(),
                Err(HandleError::NotALocalHandle),
                "{refused:?}"
            );
        }
    }

    fn local(text: &str) -> LocalHandle {
        text.parse().unwrap()
    }

    fn assigned_beside(announced: &str, taken: &[&str]) -> String {
        let taken = taken.iter().map(|text| local(text)).collect();
        LocalHandle::assign(&announced.parse().unwrap(), &taken)
            .as_str()
            .to_owned()
    }

    #[test]
    fn assignment_keeps_the_announced_handle_when_free_spec_3_8() {
        assert_eq!(assigned_beside("h", &[]), "h");
        assert_eq!(assigned_beside("h", &["g", "h-2", "hh", "i-3"]), "h");
    }

    #[test]
    fn assignment_takes_the_smallest_free_suffix_of_at_least_2_spec_3_8() {
        assert_eq!(assigned_beside("h", &["h"]), "h-2");
        assert_eq!(assigned_beside("h", &["h", "h-2"]), "h-3");
        assert_eq!(assigned_beside("h", &["h", "h-2", "h-4"]), "h-3");
        assert_eq!(assigned_beside("h", &["h", "h-3", "h-4"]), "h-2");
        assert_eq!(assigned_beside("h", &["h", "h-3", "g-2"]), "h-2");
    }

    #[test]
    fn assignment_compares_suffixes_as_numbers_past_nine_spec_3_8() {
        let taken: Vec<String> = std::iter::once("h".to_owned())
            .chain((2..=10).map(|suffix| format!("h-{suffix}")))
            .collect();
        let taken: Vec<&str> = taken.iter().map(String::as_str).collect();
        assert_eq!(assigned_beside("h", &taken), "h-11");
    }

    #[test]
    fn assignment_suffixes_an_announced_suffix_again_spec_3_8() {
        assert_eq!(assigned_beside("h-2", &["h-2"]), "h-2-2");
        assert_eq!(assigned_beside("h-2", &["h", "h-2", "h-2-2"]), "h-2-3");
    }

    const PROPERTY_CASES: u64 = 2000;
    const HANDLE_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789-";
    const FIRST_CHARACTER_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
    const SUFFIX_FIRST_DIGITS: &[u8] = b"23456789";
    const DIGITS: &[u8] = b"0123456789";
    const FORBIDDEN_CHARACTERS: &[char] = &['A', 'Z', '_', ' ', '.', '/', '\u{e9}'];
    const TAKEN_ODDS_AGAINST: usize = 3;
    const ASSIGNMENT_CANDIDATE_SUFFIX_LIMIT: usize = 14;

    fn below(case: u64, stream: [usize; 2], bound: usize) -> usize {
        let mixed = stream.iter().fold(case, |mixed, part| {
            mixed
                .wrapping_add(u64::try_from(*part).unwrap())
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .rotate_left(29)
                .wrapping_mul(0xBF58_476D_1CE4_E5B9)
        });
        usize::try_from(mixed >> 16)
            .unwrap()
            .checked_rem(bound)
            .unwrap()
    }

    fn generated_handle(case: u64) -> String {
        let length = below(case, [0, 0], MAX_HANDLE_LENGTH).saturating_add(1);
        let first = FIRST_CHARACTER_ALPHABET[below(case, [1, 0], FIRST_CHARACTER_ALPHABET.len())];
        let rest = (1..length)
            .map(|position| HANDLE_ALPHABET[below(case, [2, position], HANDLE_ALPHABET.len())]);
        std::iter::once(first).chain(rest).map(char::from).collect()
    }

    fn generated_valid_suffix(case: u64) -> String {
        let digit_count = below(case, [3, 0], MAX_SUFFIX_DIGITS).saturating_add(1);
        let first = SUFFIX_FIRST_DIGITS[below(case, [4, 0], SUFFIX_FIRST_DIGITS.len())];
        let rest =
            (1..digit_count).map(|position| DIGITS[below(case, [5, position], DIGITS.len())]);
        std::iter::once(first).chain(rest).map(char::from).collect()
    }

    #[test]
    fn every_generated_valid_handle_parses_as_each_of_the_three_forms_spec_3_1() {
        for case in 0..PROPERTY_CASES {
            let text = generated_handle(case);
            assert_eq!(text.parse::<Handle>().unwrap().as_str(), text);
            assert_eq!(text.parse::<AnnouncedHandle>().unwrap().as_str(), text);
            assert_eq!(text.parse::<LocalHandle>().unwrap().as_str(), text);
        }
    }

    #[test]
    fn a_forbidden_character_anywhere_refuses_all_three_forms_spec_3_1() {
        for case in 0..PROPERTY_CASES {
            let text = generated_handle(case);
            let forbidden = FORBIDDEN_CHARACTERS[below(case, [6, 0], FORBIDDEN_CHARACTERS.len())];
            let at = below(case, [7, 0], text.len().saturating_add(1));
            let mut spoiled = text.clone();
            spoiled.insert(at, forbidden);
            assert_eq!(spoiled.parse::<Handle>(), Err(HandleError::NotAHandle));
            assert_eq!(
                spoiled.parse::<AnnouncedHandle>(),
                Err(HandleError::NotAnAnnouncedHandle)
            );
            assert_eq!(
                spoiled.parse::<LocalHandle>(),
                Err(HandleError::NotALocalHandle)
            );
        }
    }

    #[test]
    fn a_suffix_after_a_full_length_handle_is_valid_only_from_2_without_leading_zero_spec_3_1() {
        for case in 0..PROPERTY_CASES {
            let base = format!(
                "{}{}",
                char::from(
                    FIRST_CHARACTER_ALPHABET[below(case, [8, 0], FIRST_CHARACTER_ALPHABET.len())]
                ),
                "x".repeat(MAX_HANDLE_LENGTH - 1)
            );
            let valid_suffix = generated_valid_suffix(case);
            let valid = format!("{base}-{valid_suffix}");
            assert_eq!(valid.parse::<AnnouncedHandle>().unwrap().as_str(), valid);
            assert_eq!(valid.parse::<LocalHandle>().unwrap().as_str(), valid);
            for refused_suffix in [format!("0{valid_suffix}"), "1".to_owned(), "0".to_owned()] {
                let refused = format!("{base}-{refused_suffix}");
                assert_eq!(
                    refused.parse::<AnnouncedHandle>(),
                    Err(HandleError::NotAnAnnouncedHandle)
                );
                assert_eq!(
                    refused.parse::<LocalHandle>(),
                    Err(HandleError::NotALocalHandle)
                );
            }
        }
    }

    #[test]
    fn assignment_is_free_valid_and_smallest_against_any_taken_set_spec_3_8() {
        for case in 0..PROPERTY_CASES {
            let announced = generated_handle(case);
            let announced_handle: AnnouncedHandle = announced.parse().unwrap();
            let candidates: Vec<LocalHandle> = std::iter::once(announced.clone())
                .chain(
                    (FIRST_ASSIGNED_SUFFIX..)
                        .take(ASSIGNMENT_CANDIDATE_SUFFIX_LIMIT)
                        .map(|suffix| format!("{announced}-{suffix}")),
                )
                .map(|text| local(&text))
                .collect();
            let taken: HashSet<LocalHandle> = candidates
                .iter()
                .enumerate()
                .filter(|(position, _)| below(case, [9, *position], TAKEN_ODDS_AGAINST) != 0)
                .map(|(_, candidate)| candidate.clone())
                .collect();
            let assigned = LocalHandle::assign(&announced_handle, &taken);
            assert!(!taken.contains(&assigned), "{announced} {assigned:?}");
            assert_eq!(
                assigned.as_str().parse::<LocalHandle>().as_ref(),
                Ok(&assigned)
            );
            let rank = candidates
                .iter()
                .position(|candidate| *candidate == assigned)
                .unwrap_or(candidates.len());
            assert!(
                candidates
                    .iter()
                    .take(rank)
                    .all(|earlier| taken.contains(earlier))
            );
        }
    }
}
