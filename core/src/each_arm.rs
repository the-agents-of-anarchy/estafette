//! The assertion that a mapping from inputs to outputs names every arm of an enumeration.

use std::fmt;

/// Asserts that the map gives each case's input the case's output, and that its outputs hold as many distinct values as named.
pub fn assert_each_arm<Input, Output: PartialEq + fmt::Debug>(
    cases: &[(Input, Output)],
    map: impl Fn(&Input) -> Output,
    distinct_outputs: usize,
) {
    let written: Vec<Output> = cases.iter().map(|(input, _)| map(input)).collect();
    let expected: Vec<&Output> = cases.iter().map(|(_, output)| output).collect();
    assert_eq!(written.iter().collect::<Vec<_>>(), expected);
    let distinct = written
        .iter()
        .enumerate()
        .filter(|(index, output)| !written[..*index].contains(output))
        .count();
    assert_eq!(
        distinct, distinct_outputs,
        "distinct outputs of {written:?}"
    );
}
