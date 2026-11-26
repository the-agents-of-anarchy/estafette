use std::fmt;

use data_encoding::HEXLOWER;
use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};

pub const KEY_LEN: usize = 32;
pub const MAC_LEN: usize = 32;
const HOST_CERTIFICATE_PREFIX: &[u8] = b"estafette-host-cert-v1";
const CERTIFICATE_LINE_END: char = '\n';
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
    PresenceRequest,
}

impl SignedPayloadKind {
    const fn domain_prefix(self) -> &'static [u8] {
        match self {
            Self::Presence => b"estafette-presence-v1",
            Self::Departure => b"estafette-departure-v1",
            Self::Unreachable => b"estafette-unreachable-v1",
            Self::Infrastructure => b"estafette-infra-v1",
            Self::PresenceRequest => b"estafette-presence-request-v1",
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

/// The length of a rendered host certificate: two hexadecimal characters per signature byte.
pub const HOST_CERTIFICATE_HEX_CHARS: usize = Signature::LENGTH * 2;

/// Writes a host certificate as lowercase hexadecimal characters.
#[must_use]
pub fn render_host_certificate(certificate: &Signature) -> String {
    HEXLOWER.encode(&certificate.to_bytes())
}

/// Reads a host certificate from 128 lowercase hexadecimal characters and an optional final newline.
pub(crate) fn parse_host_certificate(text: &str) -> Option<Signature> {
    let digits = text.strip_suffix(CERTIFICATE_LINE_END).unwrap_or(text);
    let bytes: [u8; Signature::LENGTH] =
        HEXLOWER.decode(digits.as_bytes()).ok()?.try_into().ok()?;
    Some(Signature::from_bytes(&bytes))
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
    signer
        .verify(&[domain_prefix, body].concat(), signature)
        .map_err(|_refusal| CryptoError::SignatureMismatch)
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSecret([u8; KEY_LEN]);

impl NetworkSecret {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    use crate::seeded_draws::Draws;

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

    const SIGNING_CASES: u64 = 16;
    const BYTE_VALUES: u64 = 256;
    const ENCODING_BYTES: usize = 40;

    fn drawn_bytes<const LENGTH: usize>(draws: &mut Draws) -> [u8; LENGTH] {
        [(); LENGTH].map(|()| u8::try_from(draws.below(BYTE_VALUES)).unwrap())
    }

    fn drawn_secret_key(draws: &mut Draws) -> SecretKey {
        SecretKey::from_bytes(&drawn_bytes(draws))
    }

    macro_rules! all_signed_kinds {
        ($($kind:ident),+) => {
            const ALL_KINDS: [SignedPayloadKind; [$(SignedPayloadKind::$kind),+].len()] =
                [$(SignedPayloadKind::$kind),+];

            /// Matches every signed kind against the one list `ALL_KINDS` is built from, so that a kind left out of it breaks the build.
            const _EVERY_SIGNED_KIND_IS_IN_ALL_KINDS: fn(SignedPayloadKind) =
                |kind| match kind {
                    $(SignedPayloadKind::$kind)|+ => {}
                };
        };
    }

    all_signed_kinds!(
        Presence,
        Departure,
        Unreachable,
        Infrastructure,
        PresenceRequest
    );

    const DOMAIN_PREFIXES: [(SignedPayloadKind, &[u8]); ALL_KINDS.len()] = [
        (SignedPayloadKind::Presence, b"estafette-presence-v1"),
        (SignedPayloadKind::Departure, b"estafette-departure-v1"),
        (SignedPayloadKind::Unreachable, b"estafette-unreachable-v1"),
        (SignedPayloadKind::Infrastructure, b"estafette-infra-v1"),
        (
            SignedPayloadKind::PresenceRequest,
            b"estafette-presence-request-v1",
        ),
    ];

    #[test]
    fn spec_3_4_each_signed_kind_signs_under_its_own_domain_prefix() {
        assert_eq!(DOMAIN_PREFIXES.map(|(kind, _prefix)| kind), ALL_KINDS);
        for case in 0..SIGNING_CASES {
            let mut draws = Draws::seeded(case);
            let signer = drawn_secret_key(&mut draws);
            let encoding: [u8; ENCODING_BYTES] = drawn_bytes(&mut draws);
            for (kind, prefix) in DOMAIN_PREFIXES {
                assert_eq!(
                    sign_payload(kind, &signer, &encoding),
                    signer.sign(&[prefix, &encoding].concat()),
                    "{kind:?}"
                );
            }
        }
    }

    #[test]
    fn spec_3_4_payload_signature_under_one_prefix_fails_under_another() {
        for case in 0..SIGNING_CASES {
            let mut draws = Draws::seeded(case);
            let signer = drawn_secret_key(&mut draws);
            let other_signer = drawn_secret_key(&mut draws);
            let encoding: [u8; ENCODING_BYTES] = drawn_bytes(&mut draws);
            let another_encoding: [u8; ENCODING_BYTES] = drawn_bytes(&mut draws);
            let presence_signature = sign_payload(SignedPayloadKind::Presence, &signer, &encoding);

            for kind in ALL_KINDS {
                let outcome = verify_payload_signature(
                    kind,
                    &signer.public(),
                    &encoding,
                    &presence_signature,
                );
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
                    &encoding,
                    &presence_signature
                ),
                Err(CryptoError::SignatureMismatch)
            );
            assert_eq!(
                verify_payload_signature(
                    SignedPayloadKind::Presence,
                    &signer.public(),
                    &another_encoding,
                    &presence_signature
                ),
                Err(CryptoError::SignatureMismatch)
            );
        }
    }

    #[test]
    fn spec_3_4_host_certificate_verifies_only_under_the_owner_key() {
        for case in 0..SIGNING_CASES {
            let mut draws = Draws::seeded(case);
            let owner = drawn_secret_key(&mut draws);
            let impostor = drawn_secret_key(&mut draws);
            let host = drawn_secret_key(&mut draws).public();
            let other_host = drawn_secret_key(&mut draws).public();
            let certificate = sign_host_certificate(&owner, &host);

            assert_eq!(
                certificate,
                owner.sign(&[b"estafette-host-cert-v1".as_slice(), host.as_bytes()].concat())
            );
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
    }

    #[test]
    fn spec_3_4_host_certificate_text_is_lowercase_hexadecimal_read_back_with_or_without_a_final_newline()
     {
        for case in 0..SIGNING_CASES {
            let mut draws = Draws::seeded(case);
            let owner = drawn_secret_key(&mut draws);
            let host = drawn_secret_key(&mut draws).public();
            let certificate = sign_host_certificate(&owner, &host);

            let text = render_host_certificate(&certificate);

            assert_eq!(text.len(), HOST_CERTIFICATE_HEX_CHARS);
            assert_eq!(text, text.to_lowercase());
            assert_eq!(parse_host_certificate(&text), Some(certificate));
            assert_eq!(
                parse_host_certificate(&format!("{text}\n")),
                Some(certificate)
            );
            assert_eq!(parse_host_certificate(&text.to_uppercase()), None);
            assert_eq!(parse_host_certificate(&format!("{text}0")), None);
        }
    }

    #[test]
    fn spec_3_4_network_keys_debug_output_prints_no_key_bytes() {
        let keys = NetworkKeys::derive(&SECRET);

        assert_eq!(format!("{keys:?}"), "NetworkKeys { .. }");
        assert_eq!(format!("{SECRET:?}"), "NetworkSecret { .. }");
        assert_eq!(format!("{:?}", keys.gate_key()), "GateKey { .. }");
        assert_eq!(format!("{:?}", keys.mac_key), "MacKey { .. }");
    }
}
