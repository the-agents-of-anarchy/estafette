use std::sync::{Mutex, PoisonError};

use data_encoding::HEXLOWER;
use serde_json::Value;

use super::{FORMAT_VERSION, RunId, TraceClocks};

const NODE_TAG_BYTES: usize = 5;

/// What begins every line of both trace files, in the order decision 5 fixes: the format version, the run, the line number, the two clock readings and the node once there is one.
pub(super) struct HeaderSource {
    run: String,
    clocks: TraceClocks,
    node: Mutex<Option<String>>,
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

    /// Names the endpoint, by the first five bytes of its id, in every later line.
    pub(super) fn note_node(&self, id_bytes: &[u8]) {
        let tag = HEXLOWER.encode(id_bytes.get(..NODE_TAG_BYTES).unwrap_or(id_bytes));
        *self.node.lock().unwrap_or_else(PoisonError::into_inner) = Some(tag);
    }

    /// The wall clock's reading now.
    pub(super) fn wall_ms(&self) -> u64 {
        (self.clocks.wall_ms)()
    }

    /// One trace line numbered `n`: a JSON object of the header, the event name and the event's fields, ended by a newline.
    pub(super) fn line(&self, n: u64, name: &str, fields: &[(&str, Value)]) -> String {
        let node = self
            .node
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let header = [
            ("v", Some(Value::from(FORMAT_VERSION))),
            ("run", Some(Value::from(self.run.as_str()))),
            ("n", Some(Value::from(n))),
            ("mono_ms", Some(Value::from((self.clocks.monotonic_ms)()))),
            ("wall_ms", Some(Value::from((self.clocks.wall_ms)()))),
            ("node", node.map(Value::from)),
            ("event", Some(Value::from(name))),
        ];
        let members: Vec<String> = header
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key, value)))
            .chain(fields.iter().map(|(key, value)| (*key, value.clone())))
            .map(|(key, value)| format!("{}:{value}", Value::from(key)))
            .collect();
        format!("{{{}}}\n", members.join(","))
    }
}
