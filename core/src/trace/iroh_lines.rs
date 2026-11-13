use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::subscriber::SetGlobalDefaultError;
use tracing::{Event, Subscriber};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use super::Trace;

/// The four targets of iroh's own whose lines the raw file holds, each with the most detailed level it takes (`spec/observability.md` decision 7).
const IROH_TARGETS: [(&str, LevelFilter); 4] = [
    ("iroh::_events", LevelFilter::DEBUG),
    ("iroh::socket::remote_map", LevelFilter::TRACE),
    ("iroh_gossip::proto::hyparview", LevelFilter::DEBUG),
    ("iroh_gossip::net", LevelFilter::DEBUG),
];
/// The name `tracing` gives the field that holds a line's message.
const TRACING_MESSAGE_FIELD: &str = "message";
const TARGET_KEY: &str = "target";
const LEVEL_KEY: &str = "level";
const MESSAGE_KEY: &str = "message";
const FIELDS_KEY: &str = "fields";

/// Sends every line iroh writes to the four targets of decision 7 into the trace's raw file for the rest of the process.
///
/// # Errors
/// `IrohLinesRefused` when the process already has a global tracing subscriber.
pub fn install(trace: Arc<Trace>) -> Result<(), IrohLinesRefused> {
    tracing::subscriber::set_global_default(raw_file_subscriber(trace)).map_err(IrohLinesRefused)
}

/// A subscriber that hands each line of the four targets to the trace's raw file and drops every other line.
fn raw_file_subscriber(trace: Arc<Trace>) -> impl Subscriber + Send + Sync {
    Registry::default()
        .with(Targets::new().with_targets(IROH_TARGETS))
        .with(RawFileLines { trace })
}

/// The layer that queues each line it is given on the trace's raw file.
struct RawFileLines {
    trace: Arc<Trace>,
}

impl<S: Subscriber> Layer<S> for RawFileLines {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        self.trace.record_iroh_line(&iroh_line(event));
    }
}

/// What a raw line holds after its header: the line's target, its level, its message when it has one, and its other fields, each as iroh words it.
fn iroh_line(event: &Event<'_>) -> Vec<(&'static str, Value)> {
    let metadata = event.metadata();
    let mut worded = WordedFields::default();
    event.record(&mut worded);
    let mut line = vec![
        (TARGET_KEY, Value::from(metadata.target())),
        (LEVEL_KEY, Value::from(metadata.level().as_str())),
    ];
    line.extend(
        worded
            .message
            .map(|message| (MESSAGE_KEY, Value::from(message))),
    );
    line.push((FIELDS_KEY, Value::Object(worded.fields)));
    line
}

/// A line's message and its other fields, each as iroh words it.
#[derive(Default)]
struct WordedFields {
    message: Option<String>,
    fields: Map<String, Value>,
}

impl WordedFields {
    /// Keeps the field's text as the message when it is the message, among the other fields when it is not.
    fn keep(&mut self, field: &Field, text: String) {
        if field.name() == TRACING_MESSAGE_FIELD {
            self.message = Some(text);
        } else {
            self.fields
                .insert(field.name().to_owned(), Value::from(text));
        }
    }
}

impl Visit for WordedFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.keep(field, value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.keep(field, format!("{value:?}"));
    }
}

/// The raw file cannot take iroh's lines: the process already has a global tracing subscriber.
#[derive(Debug)]
pub struct IrohLinesRefused(SetGlobalDefaultError);

impl fmt::Display for IrohLinesRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot write iroh's own lines into the raw trace file: {}; run with --no-debug to write no trace",
            self.0
        )
    }
}

impl std::error::Error for IrohLinesRefused {}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use iroh::SecretKey;
    use serde_json::Value;
    use tracing::subscriber::with_default;

    use super::raw_file_subscriber;
    use crate::dirs::{OperatingSystemFilesystem, TraceDirectory, rolled_over_trace_file};
    use crate::trace::{
        RUN_ID_BYTES, RunId, Trace, TraceBounds, TraceClocks, TraceEffects, TraceFile, TraceStop,
    };

    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
    const RUN_HEX: &str = "a1b2c3d4e5f60718";
    const FROZEN_MONOTONIC_MS: u64 = 7;
    const NOW_MS: u64 = 2_000_000_000_000;
    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const ENDPOINT_SEED: u8 = 3;
    const SPEC_NODE_HEX_CHARS: usize = 10;
    const SPEC_RAW_FILE_OPENED: &str = "raw_file_opened";
    const SPEC_IROH_LINE: &str = "iroh_line";
    const SPEC_DROPPED: &str = "dropped";
    const SPEC_LEVELS_COARSEST_FIRST: [&str; 5] = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"];
    const SPEC_TARGETS_AND_FINEST_LEVELS: [(&str, &str); 4] = [
        ("iroh::_events", "DEBUG"),
        ("iroh::socket::remote_map", "TRACE"),
        ("iroh_gossip::proto::hyparview", "DEBUG"),
        ("iroh_gossip::net", "DEBUG"),
    ];
    const RAW_FILE_SUFFIX: &str = ".raw.jsonl";
    const SMALL_QUEUE_LINES: usize = 2;
    const WRITER_STEP_BOUND: Duration = Duration::from_secs(10);
    const TEST_DRAIN_BOUND: Duration = Duration::from_secs(10);
    const DIRECT_ADDRS_LINE_AFTER_HEADER: &str = ",\"target\":\"iroh::_events::direct_addrs\",\"level\":\"DEBUG\",\"message\":\"a line\",\"fields\":{}}\n";

    /// How iroh's `Side` reads in its lines.
    #[derive(Debug)]
    enum Side {
        Client,
    }

    /// A fresh scratch root for the test and the trace directory under its data home.
    fn scratch_trace_directory(test_name: &str) -> (PathBuf, TraceDirectory) {
        let root = std::env::temp_dir().join(format!(
            "estafette-raw-iroh-lines-{test_name}-{}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        let data_home = root.join("data");
        let directory = TraceDirectory::resolve(&move |name: &str| {
            (name == DATA_HOME_VARIABLE).then(|| OsString::from(&data_home))
        })
        .unwrap();
        (root, directory)
    }

    /// Clocks frozen at `FROZEN_MONOTONIC_MS` and `NOW_MS`.
    fn frozen_clocks() -> TraceClocks {
        TraceClocks {
            monotonic_ms: Arc::new(|| FROZEN_MONOTONIC_MS),
            wall_ms: Arc::new(|| NOW_MS),
        }
    }

    /// A trace started on the given effects within the given bounds.
    fn started(
        directory: &TraceDirectory,
        effects: &TraceEffects,
        bounds: TraceBounds,
    ) -> Arc<Trace> {
        Arc::new(
            Trace::start(
                directory,
                &OperatingSystemFilesystem,
                RunId::from_random_bytes(RUN_BYTES),
                effects,
                bounds,
            )
            .unwrap(),
        )
    }

    /// The header of the raw line numbered `n`, through its event name.
    fn header(n: u64, node: Option<&str>, event: &str) -> String {
        let node = node.map_or_else(String::new, |node| format!(",\"node\":\"{node}\""));
        format!(
            "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":{n},\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{NOW_MS}{node},\"event\":\"{event}\""
        )
    }

    /// The raw file's first line.
    fn raw_file_opened_line() -> String {
        format!(
            "{},\"safe_to_post\":false}}\n",
            header(0, None, SPEC_RAW_FILE_OPENED)
        )
    }

    #[test]
    fn observability_decision_7_a_raw_line_holds_the_header_then_the_target_the_level_the_message_and_the_fields_as_iroh_words_them()
     {
        let (root, directory) = scratch_trace_directory("shape");
        let trace = started(
            &directory,
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds::SPEC,
        );
        let id = SecretKey::from_bytes(&[ENDPOINT_SEED; 32]).public();
        let node = data_encoding::HEXLOWER.encode(id.as_bytes());
        let node = node.get(..SPEC_NODE_HEX_CHARS).unwrap();

        with_default(raw_file_subscriber(Arc::clone(&trace)), || {
            tracing::debug!(
                target: "iroh::_events::conn::connected",
                conn_id = 7_u64,
                side = ?Side::Client,
                remote_id = %"ab12c",
                alpn = "estafette/direct/1"
            );
            trace.note_endpoint(id);
            tracing::trace!(
                target: "iroh::socket::remote_map::remote_state",
                path_id = 3_u64,
                "sending datagram to {}",
                "the selected path"
            );
        });
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));

        assert_eq!(
            fs::read_to_string(&trace.files().raw).unwrap(),
            format!(
                "{}\
                 {},\"target\":\"iroh::_events::conn::connected\",\"level\":\"DEBUG\",\"fields\":{{\"alpn\":\"estafette/direct/1\",\"conn_id\":\"7\",\"remote_id\":\"ab12c\",\"side\":\"Client\"}}}}\n\
                 {},\"target\":\"iroh::socket::remote_map::remote_state\",\"level\":\"TRACE\",\"message\":\"sending datagram to the selected path\",\"fields\":{{\"path_id\":\"3\"}}}}\n",
                raw_file_opened_line(),
                header(1, None, SPEC_IROH_LINE),
                header(2, Some(node), SPEC_IROH_LINE),
            )
        );
        assert_eq!(fs::read_to_string(&trace.files().events).unwrap(), "");
        drop(fs::remove_dir_all(&root));
    }

    /// Writes one line at each of the five levels to each target given, saying which targets it wrote to.
    macro_rules! one_line_at_every_level_of_each {
        ($($target:literal),+ $(,)?) => {
            [$({
                tracing::error!(target: $target, "a line");
                tracing::warn!(target: $target, "a line");
                tracing::info!(target: $target, "a line");
                tracing::debug!(target: $target, "a line");
                tracing::trace!(target: $target, "a line");
                $target
            }),+]
        };
    }

    /// The position of the finest level the spec lets reach the raw file from the target, by the most specific spec target it falls under, or none when it falls under none.
    fn finest_spec_level(target: &str) -> Option<usize> {
        SPEC_TARGETS_AND_FINEST_LEVELS
            .iter()
            .filter(|(spec_target, _)| {
                target == *spec_target || target.starts_with(&format!("{spec_target}::"))
            })
            .max_by_key(|(spec_target, _)| spec_target.len())
            .and_then(|(_, finest)| {
                SPEC_LEVELS_COARSEST_FIRST
                    .iter()
                    .position(|level| level == finest)
            })
    }

    /// The target and the level of each line of the raw file after its first line.
    fn targets_and_levels_after_the_first_line(raw: &Path) -> Vec<(String, String)> {
        fs::read_to_string(raw)
            .unwrap()
            .lines()
            .skip(1)
            .map(|line| {
                let line: Value = serde_json::from_str(line).unwrap();
                (
                    line["target"].as_str().unwrap().to_owned(),
                    line["level"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn observability_decision_7_only_the_four_targets_reach_the_raw_file_each_up_to_its_own_level()
    {
        let (root, directory) = scratch_trace_directory("targets");
        let trace = started(
            &directory,
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds::SPEC,
        );

        let emitted = with_default(raw_file_subscriber(Arc::clone(&trace)), || {
            one_line_at_every_level_of_each!(
                "iroh::_events",
                "iroh::_events::path::selected",
                "iroh::socket::remote_map",
                "iroh::socket::remote_map::remote_state",
                "iroh_gossip::proto::hyparview",
                "iroh_gossip::net",
                "iroh_gossip::net::util",
                "iroh::endpoint",
                "iroh::socket",
                "iroh_gossip::proto::plumtree",
                "iroh_gossip::proto::state",
                "iroh_relay::client",
                "estafette::node",
            )
        });
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));

        let expected: Vec<(String, String)> = emitted
            .iter()
            .flat_map(|target| {
                SPEC_LEVELS_COARSEST_FIRST
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| {
                        finest_spec_level(target).is_some_and(|finest| *position <= finest)
                    })
                    .map(|(_, level)| ((*target).to_owned(), (*level).to_owned()))
            })
            .collect();
        assert_eq!(
            targets_and_levels_after_the_first_line(&trace.files().raw),
            expected
        );
        drop(fs::remove_dir_all(&root));
    }

    /// Writes one line of `iroh::_events::direct_addrs` with the message "a line".
    fn one_direct_addrs_line() {
        tracing::debug!(target: "iroh::_events::direct_addrs", "a line");
    }

    /// The raw line numbered `n` that `one_direct_addrs_line` writes.
    fn direct_addrs_line(n: u64) -> String {
        format!(
            "{}{DIRECT_ADDRS_LINE_AFTER_HEADER}",
            header(n, None, SPEC_IROH_LINE)
        )
    }

    #[test]
    fn observability_decision_13_iroh_lines_roll_the_raw_file_over_and_each_raw_file_opens_with_its_first_line()
     {
        const LINES_WRITTEN: u64 = 3;
        let (root, directory) = scratch_trace_directory("rollover");
        let first_line_bytes = u64::try_from(raw_file_opened_line().len()).unwrap();
        let iroh_line_bytes = u64::try_from(direct_addrs_line(1).len()).unwrap();
        let trace = started(
            &directory,
            &TraceEffects::of_the_operating_system(frozen_clocks()),
            TraceBounds {
                rollover_bytes: first_line_bytes + iroh_line_bytes,
                ..TraceBounds::SPEC
            },
        );

        with_default(raw_file_subscriber(Arc::clone(&trace)), || {
            for _line in 0..LINES_WRITTEN {
                one_direct_addrs_line();
            }
        });
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));

        let raw = &trace.files().raw;
        assert_eq!(
            fs::read_to_string(rolled_over_trace_file(raw)).unwrap(),
            format!("{}{}", raw_file_opened_line(), direct_addrs_line(2))
        );
        assert_eq!(
            fs::read_to_string(raw).unwrap(),
            format!("{}{}", raw_file_opened_line(), direct_addrs_line(3))
        );
        drop(fs::remove_dir_all(&root));
    }

    /// A raw file the test holds back: each write announces itself, waits for the test's release, then hands the line over.
    struct HeldBackRawFile {
        entered: Sender<()>,
        released: Receiver<()>,
        written: Sender<String>,
    }

    impl Write for HeldBackRawFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.entered.send(()).map_err(io::Error::other)?;
            self.released.recv().map_err(io::Error::other)?;
            self.written
                .send(String::from_utf8_lossy(bytes).into_owned())
                .map_err(io::Error::other)?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for HeldBackRawFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    /// A trace file that discards every byte written to it.
    struct DiscardedFile;

    impl Write for DiscardedFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl TraceFile for DiscardedFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    /// The test's ends of a held-back raw file: where a write announces itself, where the test releases it, and where its line arrives.
    struct HeldBackRaw {
        entering: Receiver<()>,
        release: Sender<()>,
        written: Receiver<String>,
    }

    /// A trace whose raw file the test holds back and whose events file is discarded, its queues `SMALL_QUEUE_LINES` long.
    fn held_back_trace(directory: &TraceDirectory) -> (Arc<Trace>, HeldBackRaw) {
        let (entered, entering) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (written_sender, written) = mpsc::channel();
        let raw_file = Mutex::new(Some(HeldBackRawFile {
            entered,
            released,
            written: written_sender,
        }));
        let effects = TraceEffects {
            create_file: Arc::new(move |path: &Path| -> io::Result<Box<dyn TraceFile>> {
                let is_raw = path
                    .to_str()
                    .is_some_and(|path| path.ends_with(RAW_FILE_SUFFIX));
                let held_back = is_raw.then(|| raw_file.lock().unwrap().take()).flatten();
                if let Some(file) = held_back {
                    return Ok(Box::new(file));
                }
                Ok(Box::new(DiscardedFile))
            }),
            ..TraceEffects::of_the_operating_system(frozen_clocks())
        };
        let trace = started(
            directory,
            &effects,
            TraceBounds {
                queue_lines: SMALL_QUEUE_LINES,
                drain: TEST_DRAIN_BOUND,
                ..TraceBounds::SPEC
            },
        );
        (
            trace,
            HeldBackRaw {
                entering,
                release,
                written,
            },
        )
    }

    /// What the receiver gets within `WRITER_STEP_BOUND`, failing with the condition's name when it gets nothing.
    fn within_bound<T>(receiver: &Receiver<T>, condition: &str) -> T {
        receiver
            .recv_timeout(WRITER_STEP_BOUND)
            .unwrap_or_else(|failure| panic!("{condition} within {WRITER_STEP_BOUND:?}: {failure}"))
    }

    /// The next line the held-back raw file writes once the test releases it: its number, its event and the count a `dropped` line carries.
    fn released_line(raw: &HeldBackRaw) -> (u64, String, Option<u64>) {
        raw.release.send(()).unwrap();
        let line: Value =
            serde_json::from_str(&within_bound(&raw.written, "a released line is written"))
                .unwrap();
        (
            line["n"].as_u64().unwrap(),
            line["event"].as_str().unwrap().to_owned(),
            line["count"].as_u64(),
        )
    }

    #[test]
    fn observability_decision_12_the_iroh_lines_a_full_raw_queue_loses_are_counted_and_the_next_line_is_a_dropped_line_where_n_skips()
     {
        const LINES_PAST_THE_HELD_FIRST_LINE: usize = SMALL_QUEUE_LINES + 2;
        let (root, directory) = scratch_trace_directory("full-queue");
        let (trace, raw) = held_back_trace(&directory);
        within_bound(&raw.entering, "the writer holds the raw file's first line");

        with_default(raw_file_subscriber(Arc::clone(&trace)), || {
            for _queued_or_lost in 0..LINES_PAST_THE_HELD_FIRST_LINE {
                one_direct_addrs_line();
            }
        });
        let held: Vec<_> = (0..=SMALL_QUEUE_LINES)
            .map(|_line| released_line(&raw))
            .collect();
        with_default(
            raw_file_subscriber(Arc::clone(&trace)),
            one_direct_addrs_line,
        );

        assert_eq!(
            held,
            [
                (0, SPEC_RAW_FILE_OPENED.to_owned(), None),
                (1, SPEC_IROH_LINE.to_owned(), None),
                (2, SPEC_IROH_LINE.to_owned(), None),
            ]
        );
        assert_eq!(released_line(&raw), (5, SPEC_DROPPED.to_owned(), Some(2)));
        assert_eq!(released_line(&raw), (6, SPEC_IROH_LINE.to_owned(), None));
        drop(raw.release);
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));
        drop(fs::remove_dir_all(&root));
    }
}
