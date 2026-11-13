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
    use std::path::PathBuf;
    use std::sync::Arc;

    use iroh::SecretKey;
    use tracing::subscriber::with_default;

    use super::raw_file_subscriber;
    use crate::dirs::{OperatingSystemFilesystem, TraceDirectory};
    use crate::trace::{
        RUN_ID_BYTES, RunId, Trace, TraceBounds, TraceClocks, TraceEffects, TraceStop,
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
}
