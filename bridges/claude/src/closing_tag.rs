use crate::inject::ENVELOPE_TAG;

const ESCAPED_OPENING_BRACKET: &str = "<\\";
const HYPHEN_IN_TAG_NAME: char = '-';
const BACKSLASH: char = '\\';
const LATIN_SMALL_LONG_S: char = '\u{17f}';
const KELVIN_SIGN: char = '\u{212a}';

const OPENING_BRACKET_LOOKALIKES: [char; 30] = [
    '<',
    '\u{2c2}',
    '\u{1438}',
    '\u{16b2}',
    '\u{2039}',
    '\u{226e}',
    '\u{227a}',
    '\u{2280}',
    '\u{22d6}',
    '\u{2329}',
    '\u{276c}',
    '\u{276e}',
    '\u{2770}',
    '\u{27e8}',
    '\u{2991}',
    '\u{2993}',
    '\u{29fc}',
    '\u{2a79}',
    '\u{2aa6}',
    '\u{3008}',
    '\u{fe3f}',
    '\u{fe64}',
    '\u{ff1c}',
    '\u{1d236}',
    '\u{226a}',
    '\u{22d8}',
    '\u{2aa1}',
    '\u{2af7}',
    '\u{27ea}',
    '\u{fe3d}',
];

const CLOSING_BRACKET_LOOKALIKES: [char; 20] = [
    '>',
    '\u{2c3}',
    '\u{1433}',
    '\u{203a}',
    '\u{226f}',
    '\u{227b}',
    '\u{2281}',
    '\u{22d7}',
    '\u{232a}',
    '\u{276d}',
    '\u{276f}',
    '\u{2771}',
    '\u{27e9}',
    '\u{29fd}',
    '\u{3009}',
    '\u{fe40}',
    '\u{fe65}',
    '\u{ff1e}',
    '\u{16f3f}',
    '\u{1d237}',
];

const SLASH_LOOKALIKES: [char; 4] = ['/', '\u{ff0f}', '\u{2215}', '\u{2044}'];

const FILL_RANGES: &[(u32, u32)] = &[
    (0xad, 0xad),
    (0x34f, 0x34f),
    (0x600, 0x605),
    (0x61c, 0x61c),
    (0x6dd, 0x6dd),
    (0x70f, 0x70f),
    (0x890, 0x891),
    (0x8e2, 0x8e2),
    (0x115f, 0x1160),
    (0x17b4, 0x17b5),
    (0x180b, 0x180f),
    (0x200b, 0x200f),
    (0x202a, 0x202e),
    (0x2060, 0x206f),
    (0x3164, 0x3164),
    (0xfe00, 0xfe0f),
    (0xfeff, 0xfeff),
    (0xffa0, 0xffa0),
    (0xfff0, 0xfffb),
    (0x110bd, 0x110bd),
    (0x110cd, 0x110cd),
    (0x13430, 0x1343f),
    (0x1bca0, 0x1bca3),
    (0x1d173, 0x1d17a),
    (0x16fe4, 0x16fe4),
    (0xe0000, 0xe0fff),
    (0x300, 0x344),
    (0x346, 0x36f),
    (0x483, 0x489),
    (0x591, 0x5bd),
    (0x5bf, 0x5bf),
    (0x5c1, 0x5c2),
    (0x5c4, 0x5c5),
    (0x5c7, 0x5c7),
    (0x610, 0x61a),
    (0x64b, 0x65f),
    (0x670, 0x670),
    (0x6d6, 0x6dc),
    (0x6df, 0x6e4),
    (0x6e7, 0x6e8),
    (0x6ea, 0x6ed),
    (0x1ab0, 0x1aff),
    (0x1dc0, 0x1dff),
    (0x20d0, 0x20ff),
    (0x3099, 0x309a),
    (0xfe20, 0xfe2f),
    (0x0, 0x8),
    (0xb, 0xc),
    (0xe, 0x1f),
    (0x7f, 0x9f),
    (0x2028, 0x2029),
];

const HYPHEN_LOOKALIKE_RANGES: &[(u32, u32)] = &[
    (0x5f, 0x5f),
    (0x203f, 0x2040),
    (0x2054, 0x2054),
    (0xfe33, 0xfe34),
    (0xfe4d, 0xfe4f),
    (0xff3f, 0xff3f),
    (0x2017, 0x2017),
    (0x2cd, 0x2cd),
    (0x7fa, 0x7fa),
    (0x640, 0x640),
    (0x2d, 0x2d),
    (0x58a, 0x58a),
    (0x5be, 0x5be),
    (0x1400, 0x1400),
    (0x1806, 0x1806),
    (0x2010, 0x2015),
    (0x2e17, 0x2e17),
    (0x2e1a, 0x2e1a),
    (0x2e3a, 0x2e3b),
    (0x2e40, 0x2e40),
    (0x2e5d, 0x2e5d),
    (0x301c, 0x301c),
    (0x3030, 0x3030),
    (0x30a0, 0x30a0),
    (0xfe31, 0xfe32),
    (0xfe58, 0xfe58),
    (0xfe63, 0xfe63),
    (0xff0d, 0xff0d),
    (0x10ead, 0x10ead),
    (0x2212, 0x2212),
    (0x207b, 0x207b),
    (0x208b, 0x208b),
    (0x2d7, 0x2d7),
    (0x2796, 0x2796),
    (0x2043, 0x2043),
    (0x30fc, 0x30fc),
    (0xff70, 0xff70),
];

fn in_ranges(character: char, ranges: &[(u32, u32)]) -> bool {
    let code_point = u32::from(character);
    ranges
        .iter()
        .any(|(first, last)| (*first..=*last).contains(&code_point))
}

const fn is_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || character == '_'
        || character == HYPHEN_IN_TAG_NAME
        || character == LATIN_SMALL_LONG_S
        || character == KELVIN_SIGN
}

fn is_filler(character: char) -> bool {
    !is_name_character(character)
        && !OPENING_BRACKET_LOOKALIKES.contains(&character)
        && !CLOSING_BRACKET_LOOKALIKES.contains(&character)
}

const fn is_tag_letter(character: char, expected: char) -> bool {
    character.to_ascii_lowercase() == expected
        || (expected == 's' && character == LATIN_SMALL_LONG_S)
}

fn skip_while(characters: &[char], keeps_going: impl Fn(char) -> bool) -> &[char] {
    let skipped = characters
        .iter()
        .take_while(|character| keeps_going(**character))
        .count();
    characters.get(skipped..).unwrap_or_default()
}

fn matches_tag_name_then_boundary(mut remaining: &[char]) -> bool {
    for (position, expected) in ENVELOPE_TAG.chars().enumerate() {
        if position > 0 {
            remaining = skip_while(remaining, |character| in_ranges(character, FILL_RANGES));
        }
        let Some((first, after)) = remaining.split_first() else {
            return false;
        };
        let matched = if expected == HYPHEN_IN_TAG_NAME {
            in_ranges(*first, HYPHEN_LOOKALIKE_RANGES)
        } else {
            is_tag_letter(*first, expected)
        };
        if !matched {
            return false;
        }
        remaining = after;
    }
    remaining
        .first()
        .is_none_or(|character| !is_name_character(*character))
}

fn is_closing_tag_after_opening_bracket(after_bracket: &[char]) -> bool {
    if after_bracket.first() == Some(&BACKSLASH) {
        return false;
    }
    let after_filler = skip_while(after_bracket, |character| {
        is_filler(character) && !SLASH_LOOKALIKES.contains(&character)
    });
    let Some((slash, after_slash)) = after_filler.split_first() else {
        return false;
    };
    SLASH_LOOKALIKES.contains(slash)
        && matches_tag_name_then_boundary(skip_while(after_slash, is_filler))
}

/// Rewrites each opening bracket of a closing envelope tag to `<\` exactly as the harness's own sender and receiver rebuild do.
#[must_use]
pub fn escape_closing_tags(body: &str) -> String {
    let characters: Vec<char> = body.chars().collect();
    let mut escaped = String::with_capacity(body.len());
    for (index, character) in characters.iter().enumerate() {
        let starts_closing_tag = OPENING_BRACKET_LOOKALIKES.contains(character)
            && characters
                .get(index.saturating_add(1)..)
                .is_some_and(is_closing_tag_after_opening_bracket);
        if starts_closing_tag {
            escaped.push_str(ESCAPED_OPENING_BRACKET);
        } else {
            escaped.push(*character);
        }
    }
    escaped
}
