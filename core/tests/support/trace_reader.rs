use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use estafette::dirs::{TraceFileKind, events_trace_file_name, raw_trace_file_name, trace_file_of};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;

/// The format version of every trace line this reader understands (spec/observability.md decision 5).
const KNOWN_FORMAT_VERSION: u64 = estafette::trace::FORMAT_VERSION;
/// The header key holding the format version.
const VERSION_KEY: &str = "v";
/// The header key holding the run id.
const RUN_KEY: &str = "run";
/// The header key holding the line number.
const NUMBER_KEY: &str = "n";
/// The header key holding the monotonic clock reading.
const MONOTONIC_KEY: &str = "mono_ms";
/// The header key holding the wall clock reading.
const WALL_KEY: &str = "wall_ms";
/// The header key holding the node, absent while the process has no endpoint.
const NODE_KEY: &str = "node";
/// The key holding the event's name.
const EVENT_KEY: &str = "event";
/// What a timeline says of a trace directory that holds no event.
pub const NO_EVENT_RECORDED: &str = "no event recorded";
/// The byte a trace file holds where it was sized ahead and no line has landed yet, which no line of JSON text holds (spec/observability.md decision 12).
const SIZED_AHEAD_FILL: u8 = 0;
/// The size a trace file is sized ahead to, so its length is a multiple of it (spec/observability.md decision 12).
pub const SPEC_TRACE_FILE_SIZED_AHEAD_BYTES: usize = 65_536;
/// The byte that ends every trace line.
const LINE_END: u8 = b'\n';

/// Whether the process writing a trace file was still writing it when the test read it, as the caller states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WritingProcess {
    Running,
    Ended,
}

impl WritingProcess {
    /// The bytes before the first line not yet landed whole: all of them once the process ended, and while it runs those before the first line that still shows the zeros the file was sized ahead with.
    pub fn landed_part(self, bytes: &[u8]) -> &[u8] {
        match self {
            Self::Running => {
                let landed_end = bytes
                    .iter()
                    .position(|byte| *byte == SIZED_AHEAD_FILL)
                    .map_or(bytes.len(), |first_fill| {
                        bytes[..first_fill]
                            .iter()
                            .rposition(|byte| *byte == LINE_END)
                            .map_or(0, |line_end| line_end.saturating_add(1))
                    });
                &bytes[..landed_end]
            }
            Self::Ended => bytes,
        }
    }
}

/// One line of a trace file: the header every line begins with, the event's name and the event's own fields, in the order they were written.
#[derive(Debug)]
pub struct TraceLine {
    pub run: String,
    pub n: u64,
    pub mono_ms: u64,
    pub wall_ms: u64,
    pub node: Option<String>,
    pub event: String,
    members: Vec<(String, Value)>,
}

/// Why a whole line of trace text could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum TraceLineError {
    UnknownFormatVersion { line: usize, found: u64, known: u64 },
    NotATraceLine { line: usize, cause: String },
}

/// Why a trace file could not be read into lines.
#[derive(Debug, PartialEq, Eq)]
pub enum TraceReadError {
    Unreadable {
        path: PathBuf,
        kind: std::io::ErrorKind,
    },
    UnreadableLine {
        path: PathBuf,
        failure: TraceLineError,
    },
}

impl fmt::Display for TraceLineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFormatVersion { line, found, known } => write!(
                formatter,
                "line {line} has format version {found}; this reader knows version {known}"
            ),
            Self::NotATraceLine { line, cause } => {
                write!(formatter, "line {line} is not a trace line: {cause}")
            }
        }
    }
}

impl fmt::Display for TraceReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { path, kind } => {
                write!(formatter, "{} cannot be read: {kind}", path.display())
            }
            Self::UnreadableLine { path, failure } => {
                write!(formatter, "{}: {failure}", path.display())
            }
        }
    }
}

impl fmt::Display for TraceLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:>10} ms  #{:<4} {}",
            self.mono_ms, self.n, self.event
        )?;
        self.fields()
            .iter()
            .try_for_each(|(key, value)| write!(formatter, " {key}={value}"))
    }
}

impl TraceLine {
    /// The keys of the line in the order it wrote them.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.members.iter().map(|(key, _)| key.as_str())
    }

    /// The value the line holds under the key, header keys included.
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.members
            .iter()
            .find(|(member, _)| member == key)
            .map(|(_, value)| value)
    }

    /// The fields of the event, the members after its name, in the order they were written.
    pub fn fields(&self) -> &[(String, Value)] {
        let name_position = self
            .members
            .iter()
            .position(|(key, _)| key == EVENT_KEY)
            .map_or(self.members.len(), |position| position.saturating_add(1));
        self.members.get(name_position..).unwrap_or_default()
    }
}

/// The members of a JSON object in the order the text writes them.
struct OrderedMembers(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for OrderedMembers {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MembersVisitor;

        impl<'de> Visitor<'de> for MembersVisitor {
            type Value = OrderedMembers;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OrderedMembers, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry::<String, Value>()? {
                    members.push(member);
                }
                Ok(OrderedMembers(members))
            }
        }

        deserializer.deserialize_map(MembersVisitor)
    }
}

/// The line read from the members of a JSON object, refusing a format version it does not know and any header it lacks.
fn trace_line_of(members: Vec<(String, Value)>, line: usize) -> Result<TraceLine, TraceLineError> {
    let member = |key: &str| {
        members
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };
    let number = |key: &str| {
        member(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| TraceLineError::NotATraceLine {
                line,
                cause: format!("{key} is missing or not a number"),
            })
    };
    let text = |key: &str| {
        member(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| TraceLineError::NotATraceLine {
                line,
                cause: format!("{key} is missing or not a string"),
            })
    };
    let found = number(VERSION_KEY)?;
    if found != KNOWN_FORMAT_VERSION {
        return Err(TraceLineError::UnknownFormatVersion {
            line,
            found,
            known: KNOWN_FORMAT_VERSION,
        });
    }
    Ok(TraceLine {
        run: text(RUN_KEY)?,
        n: number(NUMBER_KEY)?,
        mono_ms: number(MONOTONIC_KEY)?,
        wall_ms: number(WALL_KEY)?,
        node: member(NODE_KEY).and_then(Value::as_str).map(str::to_owned),
        event: text(EVENT_KEY)?,
        members,
    })
}

/// Every whole line of the bytes, leaving out the unended last line a killed process may leave and, while the process runs, every line from the first one that a read, made while its write was landing, still shows partly as the zeros the file was sized ahead with.
///
/// # Errors
///
/// Fails on a whole line that is no trace line, naming why, and on one with a format version this reader does not know, naming it and the version it knows.
pub fn parse_trace_lines(
    bytes: &[u8],
    writing: WritingProcess,
) -> Result<Vec<TraceLine>, TraceLineError> {
    let mut segments: Vec<&[u8]> = writing
        .landed_part(bytes)
        .split(|byte| *byte == LINE_END)
        .collect();
    segments.pop();
    segments
        .into_iter()
        .enumerate()
        .map(|(index, segment)| {
            let line = index.saturating_add(1);
            let OrderedMembers(members) = serde_json::from_slice(segment).map_err(|failure| {
                TraceLineError::NotATraceLine {
                    line,
                    cause: failure.to_string(),
                }
            })?;
            trace_line_of(members, line)
        })
        .collect()
}

/// Every whole line of the trace file, read as the caller states its writing process: running or ended.
///
/// # Errors
///
/// Fails when the file cannot be read, and as `parse_trace_lines` does, naming the file.
pub fn read_trace_file(
    path: &Path,
    writing: WritingProcess,
) -> Result<Vec<TraceLine>, TraceReadError> {
    let bytes = fs::read(path).map_err(|failure| TraceReadError::Unreadable {
        path: path.to_path_buf(),
        kind: failure.kind(),
    })?;
    parse_trace_lines(&bytes, writing).map_err(|failure| TraceReadError::UnreadableLine {
        path: path.to_path_buf(),
        failure,
    })
}

/// Every line of the trace file of a stopped process, failing the test when the file cannot be read or keeps any byte after its last newline.
#[track_caller]
pub fn stopped_trace_lines_of(path: &Path) -> Vec<TraceLine> {
    let bytes = fs::read(path).unwrap_or_else(|failure| panic!("{}: {failure}", path.display()));
    let unended = bytes
        .rsplit(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    assert!(
        unended.is_empty(),
        "{}: {} bytes after its last whole line",
        path.display(),
        unended.len()
    );
    parse_trace_lines(&bytes, WritingProcess::Ended)
        .unwrap_or_else(|failure| panic!("{}: {failure}", path.display()))
}

/// The events files in the trace directory, oldest run first and each run's earlier lines first; none when the directory does not exist.
///
/// # Errors
///
/// Fails with `TraceReadError::Unreadable` when the directory exists but cannot be listed.
pub fn events_files_in(traces_directory: &Path) -> Result<Vec<PathBuf>, TraceReadError> {
    trace_files_in(traces_directory, TraceFileKind::Events)
}

/// The trace files of the kind in the trace directory, oldest run first and each run's earlier lines first; none when the directory does not exist.
///
/// # Errors
///
/// Fails with `TraceReadError::Unreadable` when the directory exists but cannot be listed.
pub fn trace_files_in(
    traces_directory: &Path,
    kind: TraceFileKind,
) -> Result<Vec<PathBuf>, TraceReadError> {
    let unreadable = |failure: std::io::Error| TraceReadError::Unreadable {
        path: traces_directory.to_path_buf(),
        kind: failure.kind(),
    };
    let entries = match fs::read_dir(traces_directory) {
        Ok(entries) => entries,
        Err(failure) if failure.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(failure) => return Err(unreadable(failure)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(unreadable)?.path();
        let is_of_the_kind = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(trace_file_of)
            .is_some_and(|(_run, file_kind)| file_kind == kind);
        if is_of_the_kind {
            files.push(path);
        }
    }
    files.sort_by_key(|file| reading_order(file));
    Ok(files)
}

/// Where a trace file is read among the others of its kind: by its run, and within its run the file it rolled over into before the file written after the rollover.
fn reading_order(trace_file: &Path) -> (String, bool) {
    let name = trace_file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let (run_name, written_after_the_rollover) =
        trace_file_of(name).map_or((name, false), |(run_name, kind)| {
            let unrolled_name = match kind {
                TraceFileKind::Events => events_trace_file_name(run_name),
                TraceFileKind::Raw => raw_trace_file_name(run_name),
            };
            (run_name, name == unrolled_name)
        });
    (run_name.to_owned(), written_after_the_rollover)
}

/// Every whole line of every events file in the trace directory, oldest run first, each read as a file whose process ended.
///
/// # Errors
///
/// Fails as `events_files_in` does, and as `read_trace_file` does for the first file it cannot read.
pub fn events_lines_in(traces_directory: &Path) -> Result<Vec<TraceLine>, TraceReadError> {
    events_lines_read_as(traces_directory, WritingProcess::Ended)
}

/// Every whole line of every events file in the trace directory, oldest run first, each read as the caller states its writing process.
fn events_lines_read_as(
    traces_directory: &Path,
    writing: WritingProcess,
) -> Result<Vec<TraceLine>, TraceReadError> {
    let mut lines = Vec::new();
    for file in events_files_in(traces_directory)? {
        lines.extend(read_trace_file(&file, writing)?);
    }
    Ok(lines)
}

/// Every whole line of the listed trace files of a running process, passing over a file removed since the listing named it, as an events file a wait listed may be when its run rolls over.
///
/// # Errors
///
/// Fails as `read_trace_file` does for the first listed file it cannot read for any reason but its absence.
pub fn lines_of_files_still_present(files: &[PathBuf]) -> Result<Vec<TraceLine>, TraceReadError> {
    let mut lines = Vec::new();
    for file in files {
        match read_trace_file(file, WritingProcess::Running) {
            Ok(read) => lines.extend(read),
            Err(TraceReadError::Unreadable {
                kind: std::io::ErrorKind::NotFound,
                ..
            }) => {}
            Err(
                failure @ (TraceReadError::Unreadable { .. }
                | TraceReadError::UnreadableLine { .. }),
            ) => return Err(failure),
        }
    }
    Ok(lines)
}

/// The events lines of the trace directory one to a row, read as the caller states its writing process, or the reason they cannot be read, for a test that prints a member's timeline as it fails.
pub fn timeline_read_as(traces_directory: &Path, writing: WritingProcess) -> String {
    match events_lines_read_as(traces_directory, writing) {
        Ok(lines) if lines.is_empty() => NO_EVENT_RECORDED.to_owned(),
        Ok(lines) => lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        Err(failure) => format!("the timeline cannot be read: {failure}"),
    }
}

#[cfg(test)]
pub mod tests {
    use serde_json::json;

    use super::super::fresh_directory::fresh_directory_in;
    use super::super::remove_scratch::remove_scratch;
    use super::*;

    fn fresh_scratch_directory(label: &str) -> PathBuf {
        fresh_directory_in(
            &std::env::temp_dir(),
            &format!("trace-reader-{}-{label}", std::process::id()),
        )
        .unwrap()
    }

    pub const MEMBER_LINE: &str = "{\"v\":2,\"run\":\"6c935f82c118f7b3\",\"n\":4,\"mono_ms\":17,\"wall_ms\":1794524353720,\"node\":\"8f43abc249\",\"event\":\"process.command_failed\",\"code\":\"unreachable\",\"handle\":\"bob\"}\n";

    /// The run of every line `MEMBER_LINE` is built on.
    pub const MEMBER_RUN: &str = "6c935f82c118f7b3";

    /// How many leading bytes of a line a test leaves zero to show it still landing.
    const LEADING_BYTES_STILL_ZERO: usize = 64;

    /// The bytes of the line as a read shows it while its write is landing: its first 64 bytes still zero.
    pub fn line_still_zero_at_its_start(line: &str) -> Vec<u8> {
        let mut bytes = line.as_bytes().to_vec();
        bytes[..LEADING_BYTES_STILL_ZERO].fill(0);
        bytes
    }

    /// `MEMBER_LINE` with the number it is written as replaced.
    pub fn member_line_numbered(number: impl std::fmt::Display) -> String {
        MEMBER_LINE.replace("\"n\":4", &format!("\"n\":{number}"))
    }

    /// The bytes a read of a live trace file sized ahead to 64 KiB gives while its second line is landing: `MEMBER_LINE` whole, then the same line numbered 5 still zero in its first 64 bytes.
    pub fn second_line_still_zero_at_its_start_sized_ahead() -> Vec<u8> {
        let second_line = line_still_zero_at_its_start(&member_line_numbered(5));
        let mut bytes = MEMBER_LINE.as_bytes().to_vec();
        bytes.extend_from_slice(&second_line);
        sized_ahead(&bytes)
    }

    /// The bytes a live trace file holds when `landed` is all that has been written to it: `landed` zero-filled to the 64 KiB the file was sized ahead to.
    pub fn sized_ahead(landed: &[u8]) -> Vec<u8> {
        let mut bytes = landed.to_vec();
        bytes.resize(SPEC_TRACE_FILE_SIZED_AHEAD_BYTES, 0);
        bytes
    }

    #[test]
    fn observability_decision_11_a_line_reads_into_its_header_its_event_name_and_its_fields_in_written_order()
     {
        let lines = parse_trace_lines(MEMBER_LINE.as_bytes(), WritingProcess::Ended).unwrap();

        let [line] = lines.as_slice() else {
            panic!("one line: {lines:?}");
        };
        assert_eq!(line.run, MEMBER_RUN);
        assert_eq!(line.n, 4);
        assert_eq!(line.mono_ms, 17);
        assert_eq!(line.wall_ms, 1_794_524_353_720);
        assert_eq!(line.node.as_deref(), Some("8f43abc249"));
        assert_eq!(line.event, "process.command_failed");
        assert_eq!(
            line.fields(),
            [
                ("code".to_owned(), json!("unreachable")),
                ("handle".to_owned(), json!("bob")),
            ]
        );
        assert_eq!(
            line.keys().collect::<Vec<_>>(),
            [
                "v", "run", "n", "mono_ms", "wall_ms", "node", "event", "code", "handle"
            ]
        );
        assert_eq!(line.value("handle"), Some(&json!("bob")));
    }

    #[test]
    fn observability_decision_11_a_line_of_a_format_version_the_reader_does_not_know_is_refused_naming_both_versions()
     {
        let future_line = MEMBER_LINE.replace("{\"v\":2", "{\"v\":3");
        let bytes = format!("{MEMBER_LINE}{future_line}");

        let refusal = parse_trace_lines(bytes.as_bytes(), WritingProcess::Ended).unwrap_err();

        assert_eq!(
            refusal,
            TraceLineError::UnknownFormatVersion {
                line: 2,
                found: 3,
                known: 2
            }
        );
        assert_eq!(
            refusal.to_string(),
            "line 2 has format version 3; this reader knows version 2"
        );
    }

    #[test]
    fn observability_decision_11_a_line_lacking_a_header_is_refused_naming_its_file_its_line_and_the_header()
     {
        let directory = fresh_scratch_directory("headerless");
        let headerless = MEMBER_LINE.replace("\"n\":4,", "");
        let bad_file = directory.join("20261112T225914Z-aaaaaaaaaaaaaaaa-estafette.events.jsonl");
        fs::write(&bad_file, format!("{MEMBER_LINE}{headerless}")).unwrap();
        fs::write(
            directory.join("20261112T225913Z-bbbbbbbbbbbbbbbb-estafette.events.jsonl"),
            MEMBER_LINE,
        )
        .unwrap();

        let refusal = events_lines_in(&directory).unwrap_err();

        assert_eq!(
            refusal.to_string(),
            format!(
                "{}: line 2 is not a trace line: n is missing or not a number",
                bad_file.display()
            )
        );
        remove_scratch(&directory).unwrap();
    }

    #[test]
    fn observability_decision_11_a_line_that_is_no_json_object_is_refused_with_the_parsers_reason()
    {
        let unended_object = "{\"v\":2,\"run\":\n";

        let refusal = parse_trace_lines(
            format!("{MEMBER_LINE}{unended_object}").as_bytes(),
            WritingProcess::Ended,
        )
        .unwrap_err();

        assert_eq!(
            refusal.to_string(),
            "line 2 is not a trace line: EOF while parsing a value at line 1 column 13"
        );
    }

    #[test]
    fn observability_decision_12_the_unended_last_line_of_a_killed_process_and_the_zeros_after_it_are_left_out()
     {
        let mut bytes = MEMBER_LINE.as_bytes().to_vec();
        bytes.extend_from_slice(&MEMBER_LINE.as_bytes()[..40]);
        bytes.extend_from_slice(&[0; 64]);

        let lines = parse_trace_lines(&bytes, WritingProcess::Ended).unwrap();

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].n, 4);
    }

    #[test]
    fn observability_decision_12_a_first_line_read_while_it_still_shows_the_zeros_the_file_was_sized_ahead_with_is_no_line_until_it_is_whole()
     {
        let directory = fresh_scratch_directory("first-line-still-zero");
        let events_file =
            directory.join("20261116T210036Z-e8e6b01fdb8dbfa0-estafette.events.jsonl");
        let first_line_still_zero_at_its_start = line_still_zero_at_its_start(MEMBER_LINE);
        let lines_numbered_in = |file: &Path| {
            read_trace_file(file, WritingProcess::Running)
                .map(|lines| lines.iter().map(|line| line.n).collect::<Vec<_>>())
        };

        fs::write(
            &events_file,
            sized_ahead(&first_line_still_zero_at_its_start),
        )
        .unwrap();
        let while_written = lines_numbered_in(&events_file);
        fs::write(&events_file, sized_ahead(MEMBER_LINE.as_bytes())).unwrap();
        let once_whole = lines_numbered_in(&events_file);

        assert_eq!(while_written, Ok(Vec::new()));
        assert_eq!(once_whole, Ok(vec![4]));
        remove_scratch(&directory).unwrap();
    }

    #[test]
    fn observability_decision_12_a_stopped_process_file_keeping_bytes_after_its_last_line_fails_the_test_reading_it()
     {
        const UNENDED_BYTES: usize = 40;
        const STOPPED_FILE_NAME: &str = "stopped.events.jsonl";
        let stopped_directory = fresh_scratch_directory("stopped");
        let stopped_file = stopped_directory.join(STOPPED_FILE_NAME);
        let mut bytes = MEMBER_LINE.as_bytes().to_vec();
        bytes.extend_from_slice(&MEMBER_LINE.as_bytes()[..UNENDED_BYTES]);
        fs::write(&stopped_file, bytes).unwrap();

        let failure = std::panic::catch_unwind(|| stopped_trace_lines_of(&stopped_file))
            .map(|lines| lines.len());

        assert_eq!(
            failure.map_err(|payload| payload.downcast_ref::<String>().cloned()),
            Err(Some(format!(
                "{}: {UNENDED_BYTES} bytes after its last whole line",
                stopped_file.display()
            )))
        );
        remove_scratch(&stopped_directory).unwrap();
    }

    #[test]
    fn observability_decision_12_a_stopped_process_file_whose_line_holds_a_zero_byte_before_a_whole_line_fails_the_test_reading_it()
     {
        let stopped_directory = fresh_scratch_directory("stopped-zero-line");
        let stopped_file = stopped_directory.join("stopped.events.jsonl");
        let mut bytes = line_still_zero_at_its_start(MEMBER_LINE);
        bytes.extend_from_slice(member_line_numbered(5).as_bytes());
        fs::write(&stopped_file, bytes).unwrap();

        let failure = std::panic::catch_unwind(|| stopped_trace_lines_of(&stopped_file))
            .map(|lines| lines.len());

        assert_eq!(
            failure.map_err(|payload| payload.downcast_ref::<String>().cloned()),
            Err(Some(format!(
                "{}: line 1 is not a trace line: expected value at line 1 column 1",
                stopped_file.display()
            )))
        );
        remove_scratch(&stopped_directory).unwrap();
    }

    #[test]
    fn observability_decision_4_a_trace_directory_reads_as_its_events_files_in_run_order_leaving_out_raw_files()
     {
        let directory = fresh_scratch_directory("directory");
        let later = member_line_numbered(9);
        fs::write(
            directory.join("20261112T225914Z-aaaaaaaaaaaaaaaa-estafette.events.jsonl"),
            &later,
        )
        .unwrap();
        fs::write(
            directory.join("20261112T225913Z-bbbbbbbbbbbbbbbb-estafette.events.jsonl"),
            MEMBER_LINE,
        )
        .unwrap();
        fs::write(
            directory.join("20261112T225913Z-bbbbbbbbbbbbbbbb-estafette.raw.jsonl"),
            "not an events line\n",
        )
        .unwrap();

        let numbers: Vec<u64> = events_lines_in(&directory)
            .unwrap()
            .iter()
            .map(|line| line.n)
            .collect();

        assert_eq!(numbers, [4, 9]);
        assert_eq!(
            stopped_trace_lines_of(
                &directory.join("20261112T225914Z-aaaaaaaaaaaaaaaa-estafette.events.jsonl")
            )[0]
            .n,
            9
        );
        assert_eq!(
            timeline_read_as(&directory, WritingProcess::Ended),
            "        17 ms  #4    process.command_failed code=\"unreachable\" handle=\"bob\"\n        17 ms  #9    process.command_failed code=\"unreachable\" handle=\"bob\""
        );
        assert_eq!(
            timeline_read_as(&directory.join("absent"), WritingProcess::Ended),
            NO_EVENT_RECORDED
        );
        remove_scratch(&directory).unwrap();
    }

    #[test]
    fn observability_decision_15_a_trace_directory_that_cannot_be_listed_is_reported_not_read_as_holding_no_event()
     {
        const NOT_A_DIRECTORY_NAME: &str = "not-a-directory";
        let holder = fresh_scratch_directory("not-a-directory");
        let not_a_directory = holder.join(NOT_A_DIRECTORY_NAME);
        fs::write(&not_a_directory, MEMBER_LINE).unwrap();

        let read = events_lines_in(&not_a_directory).map(|lines| lines.len());
        let timeline = timeline_read_as(&not_a_directory, WritingProcess::Ended);

        assert_eq!(
            read,
            Err(TraceReadError::Unreadable {
                path: not_a_directory.clone(),
                kind: std::io::ErrorKind::NotADirectory
            })
        );
        assert_eq!(
            timeline,
            format!(
                "the timeline cannot be read: {} cannot be read: not a directory",
                not_a_directory.display()
            )
        );
        remove_scratch(&holder).unwrap();
    }

    #[test]
    fn observability_decision_13_the_lines_a_run_rolled_over_are_read_before_the_lines_written_after_the_rollover()
     {
        let directory = fresh_scratch_directory("rolled-over");
        let run = "20261112T225913Z-bbbbbbbbbbbbbbbb-estafette.events.jsonl";
        let after_rollover = member_line_numbered(7);
        fs::write(directory.join(format!("{run}.1")), MEMBER_LINE).unwrap();
        fs::write(directory.join(run), &after_rollover).unwrap();

        let numbers: Vec<u64> = events_lines_in(&directory)
            .unwrap()
            .iter()
            .map(|line| line.n)
            .collect();

        assert_eq!(numbers, [4, 7]);
        remove_scratch(&directory).unwrap();
    }

    #[test]
    fn observability_decision_7_a_trace_directory_reads_as_its_raw_files_in_run_order_the_rolled_over_lines_first_leaving_out_events_files()
     {
        let directory = fresh_scratch_directory("raw-files");
        let earlier_run = "20261112T225913Z-bbbbbbbbbbbbbbbb-estafette.raw.jsonl";
        fs::write(directory.join(format!("{earlier_run}.1")), MEMBER_LINE).unwrap();
        fs::write(directory.join(earlier_run), member_line_numbered(7)).unwrap();
        fs::write(
            directory.join("20261112T225914Z-aaaaaaaaaaaaaaaa-estafette.raw.jsonl"),
            member_line_numbered(9),
        )
        .unwrap();
        fs::write(
            directory.join("20261112T225913Z-bbbbbbbbbbbbbbbb-estafette.events.jsonl"),
            member_line_numbered(2),
        )
        .unwrap();

        let numbers: Vec<u64> = trace_files_in(&directory, TraceFileKind::Raw)
            .and_then(|files| lines_of_files_still_present(&files))
            .unwrap()
            .iter()
            .map(|line| line.n)
            .collect();

        assert_eq!(numbers, [4, 7, 9]);
        remove_scratch(&directory).unwrap();
    }

    #[test]
    fn observability_decision_14_a_listed_events_file_removed_before_its_read_is_passed_over_and_every_other_read_failure_still_fails()
     {
        const FILES: u32 = 4;
        const REMOVED_SETS: u32 = 1 << FILES;
        let root = fresh_scratch_directory("removed");
        let is_removed = |removed_set: u32, index: u32| removed_set & (1 << index) != 0;

        for removed_set in 0..REMOVED_SETS {
            let directory = root.join(format!("removed-set-{removed_set}"));
            fs::create_dir(&directory).unwrap();
            for index in 0..FILES {
                fs::write(
                    directory.join(format!(
                        "20261112T22591{index}Z-aaaaaaaaaaaaaaaa-estafette.events.jsonl"
                    )),
                    member_line_numbered(index),
                )
                .unwrap();
            }
            let listed = events_files_in(&directory).unwrap();
            for (index, file) in (0..FILES).zip(&listed) {
                if is_removed(removed_set, index) {
                    fs::remove_file(file).unwrap();
                }
            }

            let numbers: Vec<u64> = lines_of_files_still_present(&listed)
                .unwrap()
                .iter()
                .map(|line| line.n)
                .collect();

            let kept: Vec<u64> = (0..FILES)
                .filter(|index| !is_removed(removed_set, *index))
                .map(u64::from)
                .collect();
            assert_eq!(numbers, kept, "removed set {removed_set:#06b}");
        }
        let not_a_file = root.join("20261112T225919Z-cccccccccccccccc-estafette.events.jsonl");
        fs::create_dir(&not_a_file).unwrap();
        let headerless = root.join("20261112T225918Z-dddddddddddddddd-estafette.events.jsonl");
        fs::write(&headerless, MEMBER_LINE.replace("\"n\":4,", "")).unwrap();

        let directory_read = lines_of_files_still_present(std::slice::from_ref(&not_a_file))
            .map(|lines| lines.len());
        let headerless_read = lines_of_files_still_present(std::slice::from_ref(&headerless))
            .map(|lines| lines.len());

        assert_eq!(
            directory_read,
            Err(TraceReadError::Unreadable {
                path: not_a_file,
                kind: std::io::ErrorKind::IsADirectory
            })
        );
        assert_eq!(
            headerless_read,
            Err(TraceReadError::UnreadableLine {
                path: headerless,
                failure: TraceLineError::NotATraceLine {
                    line: 1,
                    cause: "n is missing or not a number".to_owned()
                }
            })
        );
        remove_scratch(&root).unwrap();
    }
}
