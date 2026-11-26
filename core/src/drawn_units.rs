//! The units a property test draws beside named inputs, over every byte or every character.

use crate::seeded_draws::Draws;

const LONGEST_DRAWN_UNITS: u64 = 24;
const SIDES_TO_PAD: u64 = 2;
const BYTE_VALUES: u64 = 256;
const ASCII_WHITESPACE: &[u8] = b" \t\n\x0c\r";
const FIRST_SURROGATE: u32 = 0xD800;
const SURROGATES: u32 = 0x800;
const SCALAR_VALUES: u32 = 0x11_0000 - SURROGATES;
pub const CATCH_ALL_CASES: u64 = 2000;

/// An index below the length, drawn from the sequence.
fn drawn_index(draws: &mut Draws, length: usize) -> usize {
    usize::try_from(draws.below(u64::try_from(length).unwrap())).unwrap()
}

/// One unit of an input a property test draws: a byte or a character.
pub trait DrawnUnit: Copy {
    /// Any unit, drawn from the sequence.
    fn drawn(draws: &mut Draws) -> Self;
    /// An ASCII whitespace unit, drawn from the sequence.
    fn drawn_whitespace(draws: &mut Draws) -> Self;
    /// The unit with an ASCII letter's case swapped, any other unit unchanged.
    fn case_swapped(self) -> Self;
}

impl DrawnUnit for u8 {
    fn drawn(draws: &mut Draws) -> Self {
        Self::try_from(draws.below(BYTE_VALUES)).unwrap()
    }

    fn drawn_whitespace(draws: &mut Draws) -> Self {
        ASCII_WHITESPACE[drawn_index(draws, ASCII_WHITESPACE.len())]
    }

    fn case_swapped(self) -> Self {
        if self.is_ascii_lowercase() {
            self.to_ascii_uppercase()
        } else {
            self.to_ascii_lowercase()
        }
    }
}

impl DrawnUnit for char {
    fn drawn(draws: &mut Draws) -> Self {
        let below_the_gap = u32::try_from(draws.below(u64::from(SCALAR_VALUES))).unwrap();
        let scalar = if below_the_gap < FIRST_SURROGATE {
            below_the_gap
        } else {
            below_the_gap.saturating_add(SURROGATES)
        };
        Self::from_u32(scalar).unwrap()
    }

    fn drawn_whitespace(draws: &mut Draws) -> Self {
        Self::from(u8::drawn_whitespace(draws))
    }

    fn case_swapped(self) -> Self {
        u8::try_from(self).map_or(self, |byte| Self::from(byte.case_swapped()))
    }
}

/// One way to draw units beside the named units.
type WayBeside<Unit> = fn(Vec<Unit>, &mut Draws) -> Vec<Unit>;

/// Every way to draw beside the named units, so the count of ways is the count of arms.
fn ways_beside<Unit: DrawnUnit>() -> [WayBeside<Unit>; 7] {
    [
        lengthened_by_a_unit,
        shortened_by_a_unit,
        with_one_unit_changed,
        with_case_swapped,
        padded_with_whitespace,
        with_a_unit_before,
        any_units,
    ]
}

/// The named units with a drawn unit after them.
fn lengthened_by_a_unit<Unit: DrawnUnit>(mut named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
    named.push(Unit::drawn(draws));
    named
}

/// The named units without their last.
fn shortened_by_a_unit<Unit: DrawnUnit>(mut named: Vec<Unit>, _draws: &mut Draws) -> Vec<Unit> {
    named.truncate(named.len().saturating_sub(1));
    named
}

/// The named units with one of them replaced by a drawn unit.
fn with_one_unit_changed<Unit: DrawnUnit>(mut named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
    let position = drawn_index(draws, named.len());
    named[position] = Unit::drawn(draws);
    named
}

/// The named units with every ASCII letter's case swapped.
fn with_case_swapped<Unit: DrawnUnit>(named: Vec<Unit>, _draws: &mut Draws) -> Vec<Unit> {
    named.into_iter().map(Unit::case_swapped).collect()
}

/// The named units with a drawn whitespace unit before or after them.
fn padded_with_whitespace<Unit: DrawnUnit>(mut named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
    let whitespace = Unit::drawn_whitespace(draws);
    if draws.below(SIDES_TO_PAD) == 0 {
        named.insert(0, whitespace);
    } else {
        named.push(whitespace);
    }
    named
}

/// The named units with a drawn unit before them.
fn with_a_unit_before<Unit: DrawnUnit>(mut named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
    named.insert(0, Unit::drawn(draws));
    named
}

/// Any drawn units, the named ones aside.
fn any_units<Unit: DrawnUnit>(_named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
    (0..draws.below(LONGEST_DRAWN_UNITS))
        .map(|_| Unit::drawn(draws))
        .collect()
}

/// The units drawn beside the named units by one of the ways beside, drawn alike often.
fn drawn_beside<Unit: DrawnUnit>(named: Vec<Unit>, draws: &mut Draws) -> Vec<Unit> {
    let ways = ways_beside::<Unit>();
    ways[drawn_index(draws, ways.len())](named, draws)
}

/// Bytes drawn beside one of the named byte strings, over all 256 byte values.
pub fn bytes_beside(named: &[&[u8]], draws: &mut Draws) -> Vec<u8> {
    let picked = named[drawn_index(draws, named.len())].to_vec();
    drawn_beside(picked, draws)
}

/// Text drawn beside one of the named strings, over every character.
pub fn text_beside(named: &[&str], draws: &mut Draws) -> String {
    let picked = named[drawn_index(draws, named.len())].chars().collect();
    drawn_beside(picked, draws).into_iter().collect()
}
