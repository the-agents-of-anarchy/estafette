use std::fmt;

/// The frames the bridge did not inject into the session, counted by the reason each was dropped.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DroppedFrames {
    pub rate_limit: u64,
    pub sender_name_refused: u64,
    pub notice_not_written: u64,
}

impl DroppedFrames {
    /// Counts the frames dropped for any reason.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.rate_limit
            .saturating_add(self.sender_name_refused)
            .saturating_add(self.notice_not_written)
    }

    /// Writes the counts as the text of the file the doctor reads: the three counts in a fixed order, one space between.
    #[must_use]
    pub fn as_state_file_text(self) -> String {
        format!(
            "{} {} {}",
            self.rate_limit, self.sender_name_refused, self.notice_not_written
        )
    }

    /// Reads the counts back from the text of the file the doctor reads, none for any other text.
    #[must_use]
    pub fn from_state_file_text(text: &str) -> Option<Self> {
        let mut counts = text.split_whitespace().map(|word| word.parse().ok());
        let dropped = Self {
            rate_limit: counts.next()??,
            sender_name_refused: counts.next()??,
            notice_not_written: counts.next()??,
        };
        counts.next().is_none().then_some(dropped)
    }
}

impl fmt::Display for DroppedFrames {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} (rate limit {}, sender name refused {}, notice not written {})",
            self.total(),
            self.rate_limit,
            self.sender_name_refused,
            self.notice_not_written
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seeded::next_below;

    const GENERATED_CASES: u64 = 200;
    const LARGEST_GENERATED_COUNT: usize = 100_000;

    fn count_from(state: &mut u64) -> u64 {
        u64::try_from(next_below(state, LARGEST_GENERATED_COUNT)).unwrap_or_default()
    }

    #[test]
    fn spec_3_12_the_counts_the_bridge_writes_for_the_doctor_are_the_counts_the_doctor_reads() {
        for seed in 0..GENERATED_CASES {
            let mut state = seed;
            let dropped = DroppedFrames {
                rate_limit: count_from(&mut state),
                sender_name_refused: count_from(&mut state),
                notice_not_written: count_from(&mut state),
            };
            assert_eq!(
                DroppedFrames::from_state_file_text(&dropped.as_state_file_text()),
                Some(dropped),
                "{dropped:?}"
            );
        }
    }

    #[test]
    fn spec_3_12_the_doctors_total_is_the_sum_of_the_three_reasons() {
        let dropped = DroppedFrames {
            rate_limit: 3,
            sender_name_refused: 10,
            notice_not_written: 200,
        };
        assert_eq!(dropped.total(), 213);
        assert_eq!(
            dropped.to_string(),
            "213 (rate limit 3, sender name refused 10, notice not written 200)"
        );
    }
}
