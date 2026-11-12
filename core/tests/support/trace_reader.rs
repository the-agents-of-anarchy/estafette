use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use estafette::dirs::{TraceFileKind, trace_file_of};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;

/// The format version of every trace line this reader understands (spec/observability.md decision 5).
const KNOWN_FORMAT_VERSION: u64 = 1;
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

/// Why a trace file could not be read into lines.
#[derive(Debug, PartialEq, Eq)]
pub enum TraceReadError {
    UnknownFormatVersion {
        line: usize,
        found: u64,
        known: u64,
    },
    NotATraceLine {
        line: usize,
    },
    Unreadable {
        path: PathBuf,
        kind: std::io::ErrorKind,
    },
}

impl fmt::Display for TraceReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFormatVersion { line, found, known } => write!(
                formatter,
                "line {line} has format version {found}; this reader knows version {known}"
            ),
            Self::NotATraceLine { line } => write!(
                formatter,
                "line {line} is not a trace line: a JSON object with the header and an event name"
            ),
            Self::Unreadable { path, kind } => {
                write!(formatter, "{} cannot be read: {kind}", path.display())
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
fn trace_line_of(members: Vec<(String, Value)>, line: usize) -> Result<TraceLine, TraceReadError> {
    let member = |key: &str| {
        members
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    };
    let found = member(VERSION_KEY)
        .and_then(Value::as_u64)
        .ok_or(TraceReadError::NotATraceLine { line })?;
    if found != KNOWN_FORMAT_VERSION {
        return Err(TraceReadError::UnknownFormatVersion {
            line,
            found,
            known: KNOWN_FORMAT_VERSION,
        });
    }
    let number = |key: &str| {
        member(key)
            .and_then(Value::as_u64)
            .ok_or(TraceReadError::NotATraceLine { line })
    };
    let text = |key: &str| {
        member(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(TraceReadError::NotATraceLine { line })
    };
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

/// Every whole line of the bytes, leaving out the unended last line a killed process may leave.
///
/// # Errors
///
/// Fails on a whole line that is no trace line, and on one with a format version this reader does not know, naming it and the version it knows.
pub fn parse_trace_lines(bytes: &[u8]) -> Result<Vec<TraceLine>, TraceReadError> {
    let mut segments: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    segments.pop();
    segments
        .into_iter()
        .enumerate()
        .map(|(index, segment)| {
            let line = index.saturating_add(1);
            let OrderedMembers(members) = serde_json::from_slice(segment)
                .map_err(|_failure| TraceReadError::NotATraceLine { line })?;
            trace_line_of(members, line)
        })
        .collect()
}

/// Every whole line of the trace file.
///
/// # Errors
///
/// Fails when the file cannot be read, and as `parse_trace_lines` does.
pub fn read_trace_file(path: &Path) -> Result<Vec<TraceLine>, TraceReadError> {
    let bytes = fs::read(path).map_err(|failure| TraceReadError::Unreadable {
        path: path.to_path_buf(),
        kind: failure.kind(),
    })?;
    parse_trace_lines(&bytes)
}

/// Every whole line of the trace file, failing the test that reads a file it cannot read.
#[track_caller]
pub fn trace_lines_of(path: &Path) -> Vec<TraceLine> {
    read_trace_file(path).unwrap_or_else(|failure| panic!("{}: {failure}", path.display()))
}

/// The events files in the trace directory, oldest run first; none when the directory does not exist.
pub fn events_files_in(traces_directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(traces_directory) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(trace_file_of)
                .is_some_and(|(_run, kind)| kind == TraceFileKind::Events)
        })
        .collect();
    files.sort();
    files
}

/// Every whole line of every events file in the trace directory, oldest run first.
///
/// # Errors
///
/// Fails as `read_trace_file` does for the first file it cannot read.
pub fn events_lines_in(traces_directory: &Path) -> Result<Vec<TraceLine>, TraceReadError> {
    let mut lines = Vec::new();
    for file in events_files_in(traces_directory) {
        lines.extend(read_trace_file(&file)?);
    }
    Ok(lines)
}

/// The events lines of the trace directory one to a row, or the reason they cannot be read, for a test that prints a member's timeline as it fails.
pub fn timeline_in(traces_directory: &Path) -> String {
    match events_lines_in(traces_directory) {
        Ok(lines) if lines.is_empty() => "no event recorded".to_owned(),
        Ok(lines) => lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        Err(failure) => format!("the timeline cannot be read: {failure}"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const MEMBER_LINE: &str = "{\"v\":1,\"run\":\"6c935f82c118f7b3\",\"n\":4,\"mono_ms\":17,\"wall_ms\":1794524353720,\"node\":\"8f43abc249\",\"event\":\"process.command_failed\",\"code\":\"unreachable\",\"handle\":\"bob\"}\n";

    #[test]
    fn observability_decision_11_a_line_reads_into_its_header_its_event_name_and_its_fields_in_written_order()
     {
        let lines = parse_trace_lines(MEMBER_LINE.as_bytes()).unwrap();

        let [line] = lines.as_slice() else {
            panic!("one line: {lines:?}");
        };
        assert_eq!(line.run, "6c935f82c118f7b3");
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
        let future_line = MEMBER_LINE.replace("{\"v\":1", "{\"v\":2");
        let bytes = format!("{MEMBER_LINE}{future_line}");

        let refusal = parse_trace_lines(bytes.as_bytes()).unwrap_err();

        assert_eq!(
            refusal,
            TraceReadError::UnknownFormatVersion {
                line: 2,
                found: 2,
                known: 1
            }
        );
        assert_eq!(
            refusal.to_string(),
            "line 2 has format version 2; this reader knows version 1"
        );
    }

    #[test]
    fn observability_decision_12_the_unended_last_line_of_a_killed_process_and_the_zeros_after_it_are_left_out()
     {
        let mut bytes = MEMBER_LINE.as_bytes().to_vec();
        bytes.extend_from_slice(&MEMBER_LINE.as_bytes()[..40]);
        bytes.extend_from_slice(&[0; 64]);

        let lines = parse_trace_lines(&bytes).unwrap();

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].n, 4);
    }

    #[test]
    fn observability_decision_4_a_trace_directory_reads_as_its_events_files_in_run_order_leaving_out_raw_files()
     {
        let directory =
            std::env::temp_dir().join(format!("trace-reader-{}-directory", std::process::id()));
        drop(fs::remove_dir_all(&directory));
        fs::create_dir_all(&directory).unwrap();
        let later = MEMBER_LINE.replace("\"n\":4", "\"n\":9");
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
            trace_lines_of(
                &directory.join("20261112T225914Z-aaaaaaaaaaaaaaaa-estafette.events.jsonl")
            )[0]
            .n,
            9
        );
        assert_eq!(
            timeline_in(&directory),
            "        17 ms  #4    process.command_failed code=\"unreachable\" handle=\"bob\"\n        17 ms  #9    process.command_failed code=\"unreachable\" handle=\"bob\""
        );
        assert_eq!(timeline_in(&directory.join("absent")), "no event recorded");
        drop(fs::remove_dir_all(&directory));
    }
}
