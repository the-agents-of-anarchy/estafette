//! A seeded draw of numbers for property tests: the same seed gives the same sequence on every run.

const SPLITMIX_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
const SPLITMIX_FIRST_MULTIPLIER: u64 = 0xBF58_476D_1CE4_E5B9;
const SPLITMIX_SECOND_MULTIPLIER: u64 = 0x94D0_49BB_1331_11EB;
const SPLITMIX_FIRST_SHIFT: u32 = 30;
const SPLITMIX_SECOND_SHIFT: u32 = 27;
const SPLITMIX_THIRD_SHIFT: u32 = 31;

/// A `SplitMix64` sequence, seeded by a test's case index.
pub struct Draws(u64);

impl Draws {
    /// The sequence that starts from `seed`.
    pub const fn seeded(seed: u64) -> Self {
        Self(seed)
    }

    /// The next number of the sequence, below `bound`.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.0 = self.0.wrapping_add(SPLITMIX_GAMMA);
        let mut mixed = self.0;
        mixed = (mixed ^ (mixed >> SPLITMIX_FIRST_SHIFT)).wrapping_mul(SPLITMIX_FIRST_MULTIPLIER);
        mixed = (mixed ^ (mixed >> SPLITMIX_SECOND_SHIFT)).wrapping_mul(SPLITMIX_SECOND_MULTIPLIER);
        (mixed ^ (mixed >> SPLITMIX_THIRD_SHIFT))
            .checked_rem(bound)
            .unwrap()
    }
}
