use std::fmt;
use std::sync::Arc;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::subscriber::SetGlobalDefaultError;
use tracing::{Event, Subscriber};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::{LookupSpan, Scope};

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
const SPANS_KEY: &str = "spans";
const SPAN_NAME_KEY: &str = "name";

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

impl<S> Layer<S> for RawFileLines
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        if let Some(span) = context.span(id) {
            let mut worded = WordedFields::default();
            attributes.record(&mut worded);
            span.extensions_mut()
                .insert(WordedSpan::named(span.name(), worded));
        }
    }

    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        self.trace
            .record_iroh_line(&iroh_line(event, spans_in_scope(event, &context)));
    }
}

/// Each span in scope at the event, outermost first, as the raw line names it.
fn spans_in_scope<S>(event: &Event<'_>, context: &Context<'_, S>) -> Vec<Value>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    context
        .event_scope(event)
        .into_iter()
        .flat_map(Scope::from_root)
        .filter_map(|span| {
            span.extensions()
                .get::<WordedSpan>()
                .map(|worded| worded.0.clone())
        })
        .collect()
}

/// What a raw line holds after its header: the line's target, its level, its message when it has one, its other fields, each as iroh words it, and the given spans in scope when there are any.
fn iroh_line(event: &Event<'_>, spans: Vec<Value>) -> Vec<(&'static str, Value)> {
    let metadata = event.metadata();
    let mut worded = WordedFields::default();
    event.record(&mut worded);
    let message = worded.0.remove(TRACING_MESSAGE_FIELD);
    let mut line = vec![
        (TARGET_KEY, Value::from(metadata.target())),
        (LEVEL_KEY, Value::from(metadata.level().as_str())),
    ];
    line.extend(message.map(|message| (MESSAGE_KEY, message)));
    line.push((FIELDS_KEY, Value::Object(worded.0)));
    if !spans.is_empty() {
        line.push((SPANS_KEY, Value::Array(spans)));
    }
    line
}

/// The fields of a line or a span, each as iroh words it.
#[derive(Default)]
struct WordedFields(Map<String, Value>);

impl Visit for WordedFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0
            .insert(field.name().to_owned(), Value::from(format!("{value:?}")));
    }
}

/// A span as a raw line names it: its name and its fields, each as iroh words it, kept with the span from its start.
struct WordedSpan(Value);

impl WordedSpan {
    /// The span of the given name holding the given fields.
    fn named(name: &str, fields: WordedFields) -> Self {
        Self(Value::Object(Map::from_iter([
            (SPAN_NAME_KEY.to_owned(), Value::from(name)),
            (FIELDS_KEY.to_owned(), Value::Object(fields.0)),
        ])))
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
