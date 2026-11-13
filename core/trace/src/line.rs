use std::sync::{Mutex, PoisonError};

use serde_json::Value;

use super::{FORMAT_VERSION, RunId, TraceClocks};

const FORMAT_VERSION_KEY: &str = "v";
const RUN_KEY: &str = "run";
const LINE_NUMBER_KEY: &str = "n";
const MONOTONIC_MS_KEY: &str = "mono_ms";
const WALL_MS_KEY: &str = "wall_ms";
const NODE_KEY: &str = "node";
const EVENT_KEY: &str = "event";

/// What begins every line of both trace files, in the order decision 5 fixes: the format version, the run, the line number, the two clock readings and the node once there is one.
pub struct HeaderSource {
    run: String,
    clocks: TraceClocks,
    node: Mutex<Option<String>>,
}

/// A trace line stamped with its clock readings, its node, its event and its fields, waiting for its number.
pub struct StampedLine {
    before_n: String,
    after_n: String,
}

impl HeaderSource {
    /// The header of a run with no endpoint yet.
    pub(super) fn new(run: RunId, clocks: TraceClocks) -> Self {
        Self {
            run: run.hex(),
            clocks,
            node: Mutex::new(None),
        }
    }

    /// Names the node, by the short form of its id, in every later line.
    pub(super) fn note_node(&self, short_id: String) {
        *self.node.lock().unwrap_or_else(PoisonError::into_inner) = Some(short_id);
    }

    /// The wall clock's reading now.
    pub(super) fn wall_ms(&self) -> u64 {
        (self.clocks.wall_ms)()
    }

    /// The event's line stamped now with both clocks and the node, its number left to the writer that writes it.
    pub(super) fn stamp(&self, name: &str, fields: &[(&str, Value)]) -> StampedLine {
        let node = self
            .node
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let mut after_n: Vec<(&str, Value)> = vec![
            (MONOTONIC_MS_KEY, Value::from((self.clocks.monotonic_ms)())),
            (WALL_MS_KEY, Value::from((self.clocks.wall_ms)())),
        ];
        after_n.extend(node.map(|node| (NODE_KEY, Value::from(node))));
        after_n.push((EVENT_KEY, Value::from(name)));
        after_n.extend(fields.iter().map(|(key, value)| (*key, value.clone())));
        StampedLine {
            before_n: joined_members(&[
                (FORMAT_VERSION_KEY, Value::from(FORMAT_VERSION)),
                (RUN_KEY, Value::from(self.run.as_str())),
            ]),
            after_n: joined_members(&after_n),
        }
    }
}

impl StampedLine {
    /// The line numbered `n`: one JSON object, ended by a newline.
    pub(super) fn numbered(&self, n: u64) -> String {
        format!(
            "{{{},{},{}}}\n",
            self.before_n,
            member(LINE_NUMBER_KEY, &Value::from(n)),
            self.after_n
        )
    }
}

/// The members of a JSON object in the order given, separated by commas.
fn joined_members(members: &[(&str, Value)]) -> String {
    members
        .iter()
        .map(|(key, value)| member(key, value))
        .collect::<Vec<_>>()
        .join(",")
}

/// One member of a JSON object: its key, a colon and its value.
fn member(key: &str, value: &Value) -> String {
    format!("{}:{value}", Value::from(key))
}
