const LCG_MULTIPLIER: u64 = 6_364_136_223_846_793_005;
const LCG_INCREMENT: u64 = 1_442_695_040_888_963_407;
const RANDOM_HIGH_BITS_SHIFT: u32 = 33;

/// Draws the next number below the bound from a generator state seeded by the test's loop index.
pub fn next_below(state: &mut u64, bound: usize) -> usize {
    *state = state
        .wrapping_mul(LCG_MULTIPLIER)
        .wrapping_add(LCG_INCREMENT);
    usize::try_from(*state >> RANDOM_HIGH_BITS_SHIFT)
        .unwrap_or(0)
        .checked_rem(bound)
        .unwrap_or(0)
}
