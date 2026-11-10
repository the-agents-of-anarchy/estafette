/// Draws the next number below the bound from a generator state seeded by the test's loop index.
pub fn next_below(state: &mut u64, bound: usize) -> usize {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    usize::try_from(*state >> 33)
        .unwrap_or(0)
        .checked_rem(bound)
        .unwrap_or(0)
}
