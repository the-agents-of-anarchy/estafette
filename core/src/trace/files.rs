use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{ListFiles, RemoveFile, StoredFile, TraceAction, TraceBounds, TraceRefused};
use crate::dirs::{TraceFileKind, trace_file_of};

const PROGRAM: &str = "estafette";
const START_PLACEHOLDER: &str = "<start>";
const RUN_PLACEHOLDER: &str = "<run>";
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
    run_name_of(
        &basic_utc_stamp(start_wall_ms / MILLISECONDS_PER_SECOND),
        run_hex,
    )
}

/// The form of every run's name as the help states it: `<start>-<run>-estafette`.
#[must_use]
pub fn run_name_pattern() -> String {
    run_name_of(START_PLACEHOLDER, RUN_PLACEHOLDER)
}

/// The name of the run that started at `start` with the run id `run`, both as written.
fn run_name_of(start: &str, run: &str) -> String {
    format!("{start}-{run}-{PROGRAM}")
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
    list_files: &ListFiles,
    remove_file: &RemoveFile,
) -> Result<(), TraceRefused> {
    let stored = list_files(traces)
        .map_err(|source| TraceRefused::new(TraceAction::ListDirectory, traces, source))?;
    let (runs, mut held_bytes) = stored_runs(traces, &stored);
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
            remove_file(file)
                .map_err(|source| TraceRefused::new(TraceAction::RemoveOldFile, file, source))?;
        }
        held_bytes = held_bytes.saturating_sub(run.bytes);
    }
    Ok(())
}

/// Every run among the files stored in the trace directory, oldest name first, and the bytes of every stored file.
fn stored_runs(traces: &Path, stored: &[StoredFile]) -> (BTreeMap<String, StoredRun>, u64) {
    let mut runs: BTreeMap<String, StoredRun> = BTreeMap::new();
    let mut held_bytes: u64 = 0;
    for file in stored {
        held_bytes = held_bytes.saturating_add(file.bytes);
        let Some((name, kind)) = file.name.to_str().and_then(trace_file_of) else {
            continue;
        };
        let run = runs.entry(name.to_owned()).or_default();
        run.files.push(traces.join(&file.name));
        run.bytes = run.bytes.saturating_add(file.bytes);
        if kind == TraceFileKind::Events {
            run.events_written_ms = Some(
                run.events_written_ms
                    .map_or(file.written_ms, |earlier| earlier.max(file.written_ms)),
            );
        }
    }
    (runs, held_bytes)
}
