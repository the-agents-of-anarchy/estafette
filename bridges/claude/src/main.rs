#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired by the bridge entry unit")
)]
mod discover;

const fn main() {}
