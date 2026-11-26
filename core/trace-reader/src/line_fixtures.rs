use crate::SPEC_TRACE_FILE_SIZED_AHEAD_BYTES;

pub const MEMBER_LINE: &str = "{\"v\":2,\"run\":\"6c935f82c118f7b3\",\"n\":4,\"mono_ms\":17,\"wall_ms\":1794524353720,\"node\":\"8f43abc249\",\"event\":\"process.command_failed\",\"code\":\"unreachable\",\"handle\":\"bob\"}\n";

/// The run of every line `MEMBER_LINE` is built on.
pub const MEMBER_RUN: &str = "6c935f82c118f7b3";

/// How many leading bytes of a line a test leaves zero to show it still landing.
const LEADING_BYTES_STILL_ZERO: usize = 64;

/// The refusal of a line too short to have the span a test zeroes at its start.
#[derive(Debug, PartialEq, Eq)]
pub struct LineShorterThanZeroedSpan {
    pub line_bytes: usize,
}

/// The bytes of the line as a read shows it while its write is landing: its first 64 bytes still zero.
///
/// # Errors
///
/// Fails with `LineShorterThanZeroedSpan` when the line is shorter than the 64 bytes to zero.
pub(crate) fn line_still_zero_at_its_start(
    line: &str,
) -> Result<Vec<u8>, LineShorterThanZeroedSpan> {
    let mut bytes = line.as_bytes().to_vec();
    bytes
        .get_mut(..LEADING_BYTES_STILL_ZERO)
        .ok_or(LineShorterThanZeroedSpan {
            line_bytes: line.len(),
        })?
        .fill(0);
    Ok(bytes)
}

/// `MEMBER_LINE` with the number it is written as replaced.
pub fn member_line_numbered(number: impl std::fmt::Display) -> String {
    MEMBER_LINE.replace("\"n\":4", &format!("\"n\":{number}"))
}

/// The bytes a read of a live trace file sized ahead to 64 KiB gives while its second line is landing: `MEMBER_LINE` whole, then the same line numbered 5 still zero in its first 64 bytes.
///
/// # Errors
///
/// Fails with `LineShorterThanZeroedSpan` when the second line is shorter than the 64 bytes to zero.
pub fn second_line_still_zero_at_its_start_sized_ahead()
-> Result<Vec<u8>, LineShorterThanZeroedSpan> {
    let second_line = line_still_zero_at_its_start(&member_line_numbered(5))?;
    let mut bytes = MEMBER_LINE.as_bytes().to_vec();
    bytes.extend_from_slice(&second_line);
    Ok(sized_ahead(&bytes))
}

/// The bytes a live trace file holds when `landed` is all that has been written to it: `landed` zero-filled to the 64 KiB the file was sized ahead to.
#[must_use]
pub fn sized_ahead(landed: &[u8]) -> Vec<u8> {
    let mut bytes = landed.to_vec();
    bytes.resize(SPEC_TRACE_FILE_SIZED_AHEAD_BYTES, 0);
    bytes
}
