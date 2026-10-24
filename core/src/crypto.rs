use std::fmt;

const KEY_LEN: usize = 32;
pub const MAC_LEN: usize = 32;
const TOPIC_CONTEXT: &str = "messaging topic v1";
const MAC_CONTEXT: &str = "messaging mac v1";
const GATE_CONTEXT: &str = "messaging gate v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    MacMismatch,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MacMismatch => formatter.write_str("mac does not match the structure"),
        }
    }
}

impl std::error::Error for CryptoError {}

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

    /// Computes the keyed mac of a structure encoded with its mac field zeroed.
    #[must_use]
    pub fn mac(&self, zeroed_encoding: &[u8]) -> [u8; MAC_LEN] {
        *self.keyed_hash(zeroed_encoding).as_bytes()
    }

    /// Accepts the mac only when it equals the recomputed mac, compared in constant time.
    ///
    /// # Errors
    /// `CryptoError::MacMismatch` when the mac differs in any bit.
    pub fn verify_mac(
        &self,
        zeroed_encoding: &[u8],
        mac: &[u8; MAC_LEN],
    ) -> Result<(), CryptoError> {
        if self.keyed_hash(zeroed_encoding) == *mac {
            Ok(())
        } else {
            Err(CryptoError::MacMismatch)
        }
    }

    fn keyed_hash(&self, zeroed_encoding: &[u8]) -> blake3::Hash {
        blake3::keyed_hash(&self.mac_key, zeroed_encoding)
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

    const ENCODING: &[u8] = b"structure with its mac field set to zero";

    #[test]
    fn spec_3_4_mac_verifies_and_rejects_one_flipped_bit() {
        let keys = NetworkKeys::derive(&SECRET);
        let mac = keys.mac(ENCODING);

        assert_eq!(mac, *blake3::keyed_hash(&keys.mac_key, ENCODING).as_bytes());
        assert_eq!(keys.verify_mac(ENCODING, &mac), Ok(()));
        for bit_index in 0..MAC_LEN * 8 {
            let mut flipped = mac;
            flipped[bit_index / 8] ^= 1 << (bit_index % 8);
            assert_eq!(
                keys.verify_mac(ENCODING, &flipped),
                Err(CryptoError::MacMismatch)
            );
        }
        let mut flipped_encoding = ENCODING.to_vec();
        flipped_encoding[0] ^= 1;
        assert_eq!(
            keys.verify_mac(&flipped_encoding, &mac),
            Err(CryptoError::MacMismatch)
        );
        assert_eq!(
            NetworkKeys::derive(&[8; KEY_LEN]).verify_mac(ENCODING, &mac),
            Err(CryptoError::MacMismatch)
        );
    }
}
