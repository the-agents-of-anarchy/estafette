//! Instruments of the branch ci/instrumented-loop-of-the-two-linux-failures, never merged: they record what test 21 computes and what iroh writes in the test process, and change no order, wait, bound or assertion.

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id};
use tracing::{Event, Subscriber};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::{LookupSpan, Scope};

/// The file, one per iteration, the set of ports test 21 computes and its outcome are appended to.
const PORTS_FILE_VARIABLE: &str = "INSTRUMENT_B_PORTS_FILE";
/// The file, one per iteration, the iroh lines of the test process are written to.
const IROH_LINES_FILE_VARIABLE: &str = "INSTRUMENT_B_IROH_LINES_FILE";
/// The targets the test process's lines are taken from, each with its most detailed level.
const HOST_TARGETS: [(&str, LevelFilter); 2] = [
    ("iroh::_events", LevelFilter::DEBUG),
    ("iroh::socket::remote_map::remote_state", LevelFilter::TRACE),
];

fn file_named_by(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable).map(PathBuf::from)
}

fn wall_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// Appends one line to the file named by the variable, when the variable is set.
pub fn append_line(variable_is_ports: bool, text: &str) {
    let variable = if variable_is_ports {
        PORTS_FILE_VARIABLE
    } else {
        IROH_LINES_FILE_VARIABLE
    };
    if let Some(path) = file_named_by(variable) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(format!("{text}\n").as_bytes()).unwrap();
    }
}

/// Appends a line to the ports file with the wall time in milliseconds.
pub fn note_ports(text: &str) {
    append_line(true, &format!("wall_ms={} {text}", wall_ms()));
}

/// Writes the test's outcome to the ports file when it is dropped: passed when the thread is not unwinding, failed when it is.
pub struct OutcomeNote(pub &'static str);

impl Drop for OutcomeNote {
    fn drop(&mut self) {
        let outcome = if std::thread::panicking() {
            "failed (the test thread was unwinding when the note was dropped)"
        } else {
            "passed (the test body ended without unwinding)"
        };
        note_ports(&format!("test={} outcome={outcome}", self.0));
    }
}

static INSTALL: Once = Once::new();

/// Sets, once per process, a global subscriber that writes each line of the two host targets to the iroh-lines file, when the variable is set.
pub fn install_iroh_lines_subscriber() {
    INSTALL.call_once(|| {
        let Some(path) = file_named_by(IROH_LINES_FILE_VARIABLE) else {
            return;
        };
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        let subscriber = Registry::default()
            .with(Targets::new().with_targets(HOST_TARGETS))
            .with(LinesToFile {
                file: Arc::new(Mutex::new(file)),
            });
        // A process with a subscriber already set keeps it; the file then stays empty and the run reports it.
        let _already_set = tracing::subscriber::set_global_default(subscriber);
    });
}

struct LinesToFile {
    file: Arc<Mutex<std::fs::File>>,
}

impl<S> Layer<S> for LinesToFile
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        if let Some(span) = context.span(id) {
            let mut fields = Worded::default();
            attributes.record(&mut fields);
            span.extensions_mut()
                .insert(WordedSpan(Value::Object(Map::from_iter([
                    ("name".to_owned(), Value::from(span.name())),
                    ("fields".to_owned(), Value::Object(fields.0)),
                ]))));
        }
    }

    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        let metadata = event.metadata();
        let mut fields = Worded::default();
        event.record(&mut fields);
        let message = fields.0.remove("message");
        let spans: Vec<Value> = context
            .event_scope(event)
            .into_iter()
            .flat_map(Scope::from_root)
            .filter_map(|span| {
                span.extensions()
                    .get::<WordedSpan>()
                    .map(|worded| worded.0.clone())
            })
            .collect();
        let thread = std::thread::current();
        let line = Value::Object(Map::from_iter([
            ("wall_ms".to_owned(), Value::from(wall_ms())),
            (
                "thread".to_owned(),
                Value::from(thread.name().unwrap_or("unnamed")),
            ),
            ("target".to_owned(), Value::from(metadata.target())),
            ("level".to_owned(), Value::from(metadata.level().as_str())),
            ("message".to_owned(), message.unwrap_or(Value::Null)),
            ("fields".to_owned(), Value::Object(fields.0)),
            ("spans".to_owned(), Value::Array(spans)),
        ]));
        let mut text = line.to_string();
        text.push('\n');
        if let Ok(mut file) = self.file.lock() {
            drop(file.write_all(text.as_bytes()));
        }
    }
}

#[derive(Default)]
struct Worded(Map<String, Value>);

impl Visit for Worded {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_owned(), Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0
            .insert(field.name().to_owned(), Value::from(format!("{value:?}")));
    }
}

struct WordedSpan(Value);
