use std::fmt;

use iroh::{PublicKey, SecretKey, Signature};

pub const KEY_LEN: usize = 32;
pub const MAC_LEN: usize = 32;
const HOST_CERTIFICATE_PREFIX: &[u8] = b"estafette-host-cert-v1";
const TOPIC_CONTEXT: &str = "messaging topic v1";
const MAC_CONTEXT: &str = "messaging mac v1";
const GATE_CONTEXT: &str = "messaging gate v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    MacMismatch,
    SignatureMismatch,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MacMismatch => formatter.write_str("mac does not match the structure"),
            Self::SignatureMismatch => {
                formatter.write_str("signature does not verify for this purpose and signer")
            }
        }
    }
}

impl std::error::Error for CryptoError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignedPayloadKind {
    Presence,
    Departure,
    Unreachable,
    Infrastructure,
}

impl SignedPayloadKind {
    const fn domain_prefix(self) -> &'static [u8] {
        match self {
            Self::Presence => b"estafette-presence-v1",
            Self::Departure => b"estafette-departure-v1",
            Self::Unreachable => b"estafette-unreachable-v1",
            Self::Infrastructure => b"estafette-infra-v1",
        }
    }
}

/// Signs a payload encoded with its sig and mac zeroed, under the domain prefix of its kind.
#[must_use]
pub fn sign_payload(
    kind: SignedPayloadKind,
    signer: &SecretKey,
    zeroed_encoding: &[u8],
) -> Signature {
    sign_under_prefix(signer, kind.domain_prefix(), zeroed_encoding)
}

/// Accepts the signature only when the signer made it for this kind over this encoding.
///
/// # Errors
/// `CryptoError::SignatureMismatch` when the signature does not verify.
pub fn verify_payload_signature(
    kind: SignedPayloadKind,
    signer: &PublicKey,
    zeroed_encoding: &[u8],
    signature: &Signature,
) -> Result<(), CryptoError> {
    verify_under_prefix(signer, kind.domain_prefix(), zeroed_encoding, signature)
}

/// Signs the host's node id with the owner key as the host certificate (spec 3.4).
#[must_use]
pub fn sign_host_certificate(owner: &SecretKey, host: &PublicKey) -> Signature {
    sign_under_prefix(owner, HOST_CERTIFICATE_PREFIX, host.as_bytes())
}

/// Accepts the certificate only when the owner key signed this host's node id.
///
/// # Errors
/// `CryptoError::SignatureMismatch` when the certificate does not verify.
pub fn verify_host_certificate(
    owner: &PublicKey,
    host: &PublicKey,
    certificate: &Signature,
) -> Result<(), CryptoError> {
    verify_under_prefix(owner, HOST_CERTIFICATE_PREFIX, host.as_bytes(), certificate)
}

fn sign_under_prefix(signer: &SecretKey, domain_prefix: &[u8], body: &[u8]) -> Signature {
    signer.sign(&[domain_prefix, body].concat())
}

fn verify_under_prefix(
    signer: &PublicKey,
    domain_prefix: &[u8],
    body: &[u8],
    signature: &Signature,
) -> Result<(), CryptoError> {
    if signer
        .verify(&[domain_prefix, body].concat(), signature)
        .is_ok()
    {
        Ok(())
    } else {
        Err(CryptoError::SignatureMismatch)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NetworkSecret([u8; KEY_LEN]);

impl NetworkSecret {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MacKey([u8; KEY_LEN]);

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GateKey([u8; KEY_LEN]);

impl GateKey {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopicId([u8; KEY_LEN]);

impl TopicId {
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mac([u8; MAC_LEN]);

impl Mac {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; MAC_LEN]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; MAC_LEN] {
        &self.0
    }
}

macro_rules! debug_without_key_bytes {
    ($($key_type:ident),*) => {$(
        impl fmt::Debug for $key_type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.debug_struct(stringify!($key_type)).finish_non_exhaustive()
            }
        }
    )*};
}

debug_without_key_bytes!(NetworkSecret, MacKey, GateKey, NetworkKeys);

#[derive(Clone, PartialEq, Eq)]
pub struct NetworkKeys {
    topic_id: TopicId,
    mac_key: MacKey,
    gate_key: GateKey,
}

impl NetworkKeys {
    /// Derives the topic id, mac key and gate key from the network secret (spec 3.4).
    #[must_use]
    pub fn derive(secret: &NetworkSecret) -> Self {
        Self {
            topic_id: TopicId(blake3::derive_key(TOPIC_CONTEXT, &secret.0)),
            mac_key: MacKey(blake3::derive_key(MAC_CONTEXT, &secret.0)),
            gate_key: GateKey(blake3::derive_key(GATE_CONTEXT, &secret.0)),
        }
    }

    /// Computes the keyed mac of a structure encoded with its mac field zeroed.
    #[must_use]
    pub fn mac(&self, zeroed_encoding: &[u8]) -> Mac {
        Mac(*self.keyed_hash(zeroed_encoding).as_bytes())
    }

    /// Accepts the mac only when it equals the recomputed mac, compared in constant time.
    ///
    /// # Errors
    /// `CryptoError::MacMismatch` when the mac differs in any bit.
    pub fn verify_mac(&self, zeroed_encoding: &[u8], mac: &Mac) -> Result<(), CryptoError> {
        if self.keyed_hash(zeroed_encoding) == mac.0 {
            Ok(())
        } else {
            Err(CryptoError::MacMismatch)
        }
    }

    /// The gossip topic id of the network.
    #[must_use]
    pub const fn topic_id(&self) -> &TopicId {
        &self.topic_id
    }

    /// The key that proves knowledge of the network secret at the gate.
    #[must_use]
    pub const fn gate_key(&self) -> &GateKey {
        &self.gate_key
    }

    fn keyed_hash(&self, zeroed_encoding: &[u8]) -> blake3::Hash {
        blake3::keyed_hash(&self.mac_key.0, zeroed_encoding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET_BYTES: [u8; KEY_LEN] = [7; KEY_LEN];
    const SECRET: NetworkSecret = NetworkSecret::from_bytes(SECRET_BYTES);

    #[test]
    fn spec_3_4_derive_is_deterministic_and_context_separated() {
        let keys = NetworkKeys::derive(&SECRET);

        assert_eq!(keys, NetworkKeys::derive(&SECRET));
        assert_ne!(
            keys,
            NetworkKeys::derive(&NetworkSecret::from_bytes([8; KEY_LEN]))
        );
        assert_eq!(
            keys.topic_id().as_bytes(),
            &blake3::derive_key("messaging topic v1", &SECRET_BYTES)
        );
        assert_eq!(
            keys.gate_key().as_bytes(),
            &blake3::derive_key("messaging gate v1", &SECRET_BYTES)
        );
        assert_eq!(
            keys.mac_key.0,
            blake3::derive_key("messaging mac v1", &SECRET_BYTES)
        );
        assert_ne!(keys.topic_id().as_bytes(), keys.gate_key().as_bytes());
        assert_ne!(keys.topic_id().as_bytes(), &keys.mac_key.0);
        assert_ne!(keys.gate_key().as_bytes(), &keys.mac_key.0);
    }

    const ENCODING: &[u8] = b"structure with its mac field set to zero";

    #[test]
    fn spec_3_4_mac_verifies_and_rejects_one_flipped_bit() {
        let keys = NetworkKeys::derive(&SECRET);
        let mac = keys.mac(ENCODING);

        assert_eq!(
            mac.as_bytes(),
            blake3::keyed_hash(&keys.mac_key.0, ENCODING).as_bytes()
        );
        assert_eq!(keys.verify_mac(ENCODING, &mac), Ok(()));
        for bit_index in 0..MAC_LEN * 8 {
            let mut flipped_bytes = *mac.as_bytes();
            flipped_bytes[bit_index / 8] ^= 1 << (bit_index % 8);
            assert_eq!(
                keys.verify_mac(ENCODING, &Mac::from_bytes(flipped_bytes)),
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
            NetworkKeys::derive(&NetworkSecret::from_bytes([8; KEY_LEN]))
                .verify_mac(ENCODING, &mac),
            Err(CryptoError::MacMismatch)
        );
    }

    const SIGNER_SEED: [u8; KEY_LEN] = [3; KEY_LEN];
    const ALL_KINDS: [SignedPayloadKind; 4] = [
        SignedPayloadKind::Presence,
        SignedPayloadKind::Departure,
        SignedPayloadKind::Unreachable,
        SignedPayloadKind::Infrastructure,
    ];

    #[test]
    fn spec_3_4_payload_signature_under_one_prefix_fails_under_another() {
        let signer = SecretKey::from_bytes(&SIGNER_SEED);
        let other_signer = SecretKey::from_bytes(&[4; KEY_LEN]);
        let presence_signature = sign_payload(SignedPayloadKind::Presence, &signer, ENCODING);

        let mut prefixed = b"estafette-presence-v1".to_vec();
        prefixed.extend_from_slice(ENCODING);
        assert_eq!(presence_signature, signer.sign(&prefixed));
        for kind in ALL_KINDS {
            let outcome =
                verify_payload_signature(kind, &signer.public(), ENCODING, &presence_signature);
            if kind == SignedPayloadKind::Presence {
                assert_eq!(outcome, Ok(()));
            } else {
                assert_eq!(outcome, Err(CryptoError::SignatureMismatch));
            }
        }
        assert_eq!(
            verify_payload_signature(
                SignedPayloadKind::Presence,
                &other_signer.public(),
                ENCODING,
                &presence_signature
            ),
            Err(CryptoError::SignatureMismatch)
        );
        assert_eq!(
            verify_payload_signature(
                SignedPayloadKind::Presence,
                &signer.public(),
                b"another encoding",
                &presence_signature
            ),
            Err(CryptoError::SignatureMismatch)
        );
    }

    #[test]
    fn spec_3_4_host_certificate_verifies_only_under_the_owner_key() {
        let owner = SecretKey::from_bytes(&SIGNER_SEED);
        let impostor = SecretKey::from_bytes(&[4; KEY_LEN]);
        let host = SecretKey::from_bytes(&[5; KEY_LEN]).public();
        let other_host = SecretKey::from_bytes(&[6; KEY_LEN]).public();
        let certificate = sign_host_certificate(&owner, &host);

        let mut prefixed = b"estafette-host-cert-v1".to_vec();
        prefixed.extend_from_slice(host.as_bytes());
        assert_eq!(certificate, owner.sign(&prefixed));
        assert_eq!(
            verify_host_certificate(&owner.public(), &host, &certificate),
            Ok(())
        );
        assert_eq!(
            verify_host_certificate(&impostor.public(), &host, &certificate),
            Err(CryptoError::SignatureMismatch)
        );
        assert_eq!(
            verify_host_certificate(&owner.public(), &other_host, &certificate),
            Err(CryptoError::SignatureMismatch)
        );
        for kind in ALL_KINDS {
            let payload_signature = sign_payload(kind, &owner, host.as_bytes());
            assert_eq!(
                verify_host_certificate(&owner.public(), &host, &payload_signature),
                Err(CryptoError::SignatureMismatch)
            );
        }
    }

    #[test]
    fn spec_3_4_network_keys_debug_output_prints_no_key_bytes() {
        let keys = NetworkKeys::derive(&SECRET);

        assert_eq!(format!("{keys:?}"), "NetworkKeys { .. }");
        assert_eq!(format!("{SECRET:?}"), "NetworkSecret { .. }");
        assert_eq!(format!("{:?}", keys.gate_key()), "GateKey { .. }");
    }
}
