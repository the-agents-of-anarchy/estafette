use iroh::EndpointId;
use serde_json::Value;

use crate::wire;

const SHORT_ID_BYTES: usize = 5;
const PEER_FIELD: &str = "peer";

/// The first five bytes of a card or node id, the form iroh's own event lines name a node by (spec/observability.md decision 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortId([u8; SHORT_ID_BYTES]);

impl From<&wire::PublicKey> for ShortId {
    fn from(id: &wire::PublicKey) -> Self {
        let [first, second, third, fourth, fifth, ..] = *id;
        Self([first, second, third, fourth, fifth])
    }
}

impl From<EndpointId> for ShortId {
    fn from(id: EndpointId) -> Self {
        Self::from(id.as_bytes())
    }
}

impl ShortId {
    /// The id as the events file writes it, in a `peer` field and in the header's `node`: ten lowercase hex characters.
    pub(crate) fn text(self) -> String {
        data_encoding::HEXLOWER.encode(&self.0)
    }

    /// The id as the events file's `peer` field.
    pub(super) fn field(self) -> (&'static str, Value) {
        (PEER_FIELD, Value::from(self.text()))
    }
}
