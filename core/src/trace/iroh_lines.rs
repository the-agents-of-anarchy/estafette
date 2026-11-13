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
