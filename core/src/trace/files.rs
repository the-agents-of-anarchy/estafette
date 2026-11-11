use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::{TraceAction, TraceBounds, TraceRefused};
use crate::dirs::{TraceFileKind, trace_file_of};

const PRIVATE_FILE_MODE: u32 = 0o600;
const PROGRAM: &str = "estafette";
const MILLISECONDS_PER_SECOND: u64 = 1000;
const SECONDS_PER_DAY: u64 = 86_400;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_MINUTE: u64 = 60;
const UNIX_EPOCH_YEAR: u64 = 1970;
const LEAP_YEAR_INTERVAL: u64 = 4;
const YEARS_PER_CENTURY: u64 = 100;
const LEAP_CENTURY_INTERVAL: u64 = 400;
const DAYS_PER_COMMON_YEAR: u64 = 365;
const DAYS_PER_LEAP_YEAR: u64 = 366;
const FEBRUARY: u64 = 2;
const DAYS_PER_MONTH_OF_A_COMMON_YEAR: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// The name every file of a run starts with: `<start>-<run>-estafette`, the start in UTC basic ISO 8601 to the second.
pub(super) fn run_name(start_wall_ms: u64, run_hex: &str) -> String {
    format!(
        "{}-{run_hex}-{PROGRAM}",
        basic_utc_stamp(start_wall_ms / MILLISECONDS_PER_SECOND)
    )
}

/// The UTC time of `unix_seconds` in basic ISO 8601 to the second, such as `20261111T160600Z`.
fn basic_utc_stamp(unix_seconds: u64) -> String {
    let second_of_day = unix_seconds % SECONDS_PER_DAY;
    let (year, month, day) = civil_date_of(unix_seconds / SECONDS_PER_DAY);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        second_of_day / SECONDS_PER_HOUR,
        second_of_day % SECONDS_PER_HOUR / SECONDS_PER_MINUTE,
        second_of_day % SECONDS_PER_MINUTE
    )
}

/// The Gregorian year, month and day of a count of days since 1970-01-01.
fn civil_date_of(days_since_epoch: u64) -> (u64, u64, u64) {
    let mut days_left = days_since_epoch;
    let mut year = UNIX_EPOCH_YEAR;
    while days_left >= days_in_year(year) {
        days_left = days_left.saturating_sub(days_in_year(year));
        year = year.saturating_add(1);
    }
    let mut month = 1;
    while days_left >= days_in_month(year, month) {
        days_left = days_left.saturating_sub(days_in_month(year, month));
        month = month.saturating_add(1);
    }
    (year, month, days_left.saturating_add(1))
}

/// Whether the Gregorian year has a 29 February.
const fn is_leap_year(year: u64) -> bool {
    year.is_multiple_of(LEAP_YEAR_INTERVAL)
        && (!year.is_multiple_of(YEARS_PER_CENTURY) || year.is_multiple_of(LEAP_CENTURY_INTERVAL))
}

/// The days of the Gregorian year.
const fn days_in_year(year: u64) -> u64 {
    if is_leap_year(year) {
        DAYS_PER_LEAP_YEAR
    } else {
        DAYS_PER_COMMON_YEAR
    }
}

/// The days of month `month`, counted from 1, of the Gregorian year.
fn days_in_month(year: u64, month: u64) -> u64 {
    let common = usize::try_from(month.saturating_sub(1))
        .ok()
        .and_then(|index| DAYS_PER_MONTH_OF_A_COMMON_YEAR.get(index).copied())
        .unwrap_or(u64::MAX);
    common.saturating_add(u64::from(month == FEBRUARY && is_leap_year(year)))
}

/// Creates the file new, readable and writable by its owner alone whatever the umask, for appending.
pub(super) fn create_private_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)?;
    fs::set_permissions(path, Permissions::from_mode(PRIVATE_FILE_MODE))?;
    Ok(file)
}

/// The files of one run in the trace directory, their bytes, and when its events file was last written.
#[derive(Default)]
struct StoredRun {
    files: Vec<PathBuf>,
    bytes: u64,
    events_written_ms: Option<u64>,
}

/// Removes the oldest runs, both files together, until the directory holds at most its bound, sparing every run whose events file was written within the freshness bound of `now_ms`.
pub(super) fn remove_oldest_runs(
    traces: &Path,
    now_ms: u64,
    bounds: &TraceBounds,
) -> Result<(), TraceRefused> {
    let (runs, mut held_bytes) = stored_runs(traces)
        .map_err(|source| TraceRefused::new(TraceAction::ListDirectory, traces, source))?;
    let freshness_ms = u64::try_from(bounds.freshness.as_millis()).unwrap_or(u64::MAX);
    for run in runs.values() {
        if held_bytes <= bounds.directory_bytes {
            break;
        }
        let is_fresh = run
            .events_written_ms
            .is_some_and(|written_ms| now_ms.saturating_sub(written_ms) <= freshness_ms);
        if is_fresh {
            continue;
        }
        for file in &run.files {
            fs::remove_file(file)
                .map_err(|source| TraceRefused::new(TraceAction::RemoveOldFile, file, source))?;
        }
        held_bytes = held_bytes.saturating_sub(run.bytes);
    }
    Ok(())
}

/// Every run in the trace directory, oldest name first, and the bytes of every file the directory holds.
fn stored_runs(traces: &Path) -> io::Result<(BTreeMap<String, StoredRun>, u64)> {
    let mut runs: BTreeMap<String, StoredRun> = BTreeMap::new();
    let mut held_bytes: u64 = 0;
    for entry in fs::read_dir(traces)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        held_bytes = held_bytes.saturating_add(metadata.len());
        let file_name = entry.file_name();
        let Some((name, kind)) = file_name.to_str().and_then(trace_file_of) else {
            continue;
        };
        let run = runs.entry(name.to_owned()).or_default();
        run.files.push(entry.path());
        run.bytes = run.bytes.saturating_add(metadata.len());
        if kind == TraceFileKind::Events {
            let written_ms = metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                });
            run.events_written_ms = Some(
                run.events_written_ms
                    .map_or(written_ms, |earlier| earlier.max(written_ms)),
            );
        }
    }
    Ok((runs, held_bytes))
}
