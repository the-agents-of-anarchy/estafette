const KEY_LEN: usize = 32;
const TOPIC_CONTEXT: &str = "messaging topic v1";
const MAC_CONTEXT: &str = "messaging mac v1";
const GATE_CONTEXT: &str = "messaging gate v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkKeys {
    topic_id: [u8; KEY_LEN],
    mac_key: [u8; KEY_LEN],
    gate_key: [u8; KEY_LEN],
}

impl NetworkKeys {
    /// Derives the topic id, mac key and gate key from the network secret (spec 3.4).
    #[must_use]
    pub fn derive(secret: &[u8; KEY_LEN]) -> Self {
        Self {
            topic_id: blake3::derive_key(TOPIC_CONTEXT, secret),
            mac_key: blake3::derive_key(MAC_CONTEXT, secret),
            gate_key: blake3::derive_key(GATE_CONTEXT, secret),
        }
    }

    /// The gossip topic id of the network.
    #[must_use]
    pub const fn topic_id(&self) -> &[u8; KEY_LEN] {
        &self.topic_id
    }

    /// The key that proves knowledge of the network secret at the gate.
    #[must_use]
    pub const fn gate_key(&self) -> &[u8; KEY_LEN] {
        &self.gate_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; KEY_LEN] = [7; KEY_LEN];

    #[test]
    fn spec_3_4_derive_is_deterministic_and_context_separated() {
        let keys = NetworkKeys::derive(&SECRET);

        assert_eq!(keys, NetworkKeys::derive(&SECRET));
        assert_ne!(keys, NetworkKeys::derive(&[8; KEY_LEN]));
        assert_eq!(
            keys.topic_id(),
            &blake3::derive_key("messaging topic v1", &SECRET)
        );
        assert_eq!(
            keys.gate_key(),
            &blake3::derive_key("messaging gate v1", &SECRET)
        );
        assert_eq!(
            keys.mac_key,
            blake3::derive_key("messaging mac v1", &SECRET)
        );
        assert_ne!(keys.topic_id(), keys.gate_key());
        assert_ne!(keys.topic_id(), &keys.mac_key);
        assert_ne!(keys.gate_key(), &keys.mac_key);
    }
}
