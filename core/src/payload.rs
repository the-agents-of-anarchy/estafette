use std::fmt;

use iroh::{PublicKey, SecretKey, Signature};

use crate::crypto::{
    CryptoError, MAC_LEN, Mac, NetworkKeys, SignedPayloadKind, sign_payload,
    verify_payload_signature,
};
use crate::wire::{self, CodecError, Payload, SignatureBytes, decode, encode};

const ZERO_MAC: wire::Mac = [0; MAC_LEN];
const ZERO_SIGNATURE: SignatureBytes = SignatureBytes([0; Signature::LENGTH]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    Codec(CodecError),
    Crypto(CryptoError),
    SignerIsNotThePayloadSigner,
}

impl fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(source) => source.fmt(formatter),
            Self::Crypto(source) => source.fmt(formatter),
            Self::SignerIsNotThePayloadSigner => formatter
                .write_str("the signing key is not the key the payload names as its signer"),
        }
    }
}

impl std::error::Error for PayloadError {}

impl From<CodecError> for PayloadError {
    fn from(source: CodecError) -> Self {
        Self::Codec(source)
    }
}

impl From<CryptoError> for PayloadError {
    fn from(source: CryptoError) -> Self {
        Self::Crypto(source)
    }
}

macro_rules! on_each_payload_variant {
    ($payload:expr, $inner:ident => $body:expr) => {
        match $payload {
            Payload::Presence($inner) => $body,
            Payload::Departure($inner) => $body,
            Payload::Unreachable($inner) => $body,
            Payload::Infrastructure($inner) => $body,
        }
    };
}

const fn signer_of(payload: &Payload) -> (SignedPayloadKind, wire::PublicKey) {
    match payload {
        Payload::Presence(presence) => (SignedPayloadKind::Presence, presence.card),
        Payload::Departure(departure) => (SignedPayloadKind::Departure, departure.card),
        Payload::Unreachable(unreachable) => (SignedPayloadKind::Unreachable, unreachable.reporter),
        Payload::Infrastructure(infrastructure) => {
            (SignedPayloadKind::Infrastructure, infrastructure.host)
        }
    }
}

const fn mac_of(payload: &Payload) -> wire::Mac {
    on_each_payload_variant!(payload, inner => inner.mac)
}

const fn signature_of(payload: &Payload) -> SignatureBytes {
    on_each_payload_variant!(payload, inner => inner.sig)
}

const fn with_mac(mut payload: Payload, mac: wire::Mac) -> Payload {
    on_each_payload_variant!(&mut payload, inner => inner.mac = mac);
    payload
}

const fn with_signature(mut payload: Payload, signature: SignatureBytes) -> Payload {
    on_each_payload_variant!(&mut payload, inner => inner.sig = signature);
    payload
}

fn mac_zeroed_encoding(payload: &Payload) -> Result<Vec<u8>, CodecError> {
    encode(&with_mac(payload.clone(), ZERO_MAC))
}

fn signature_zeroed_encoding(payload: &Payload) -> Result<Vec<u8>, CodecError> {
    encode(&with_signature(
        with_mac(payload.clone(), ZERO_MAC),
        ZERO_SIGNATURE,
    ))
}

fn verify_signature(payload: &Payload) -> Result<(), PayloadError> {
    let (kind, signer_bytes) = signer_of(payload);
    let Ok(signer) = PublicKey::from_bytes(&signer_bytes) else {
        return Err(CryptoError::SignatureMismatch.into());
    };
    let signature = Signature::from_bytes(&signature_of(payload).0);
    Ok(verify_payload_signature(
        kind,
        &signer,
        &signature_zeroed_encoding(payload)?,
        &signature,
    )?)
}

/// Signs, macs and encodes a gossip payload, ready to broadcast.
///
/// # Errors
/// Refuses a signer key that is not the payload's own signer.
pub fn seal_payload(
    keys: &NetworkKeys,
    signer: &SecretKey,
    payload: Payload,
) -> Result<Vec<u8>, PayloadError> {
    let (kind, signer_bytes) = signer_of(&payload);
    if signer.public().as_bytes() != &signer_bytes {
        return Err(PayloadError::SignerIsNotThePayloadSigner);
    }
    sign_then_mac(keys, kind, signer, payload)
}

fn sign_then_mac(
    keys: &NetworkKeys,
    kind: SignedPayloadKind,
    signer: &SecretKey,
    payload: Payload,
) -> Result<Vec<u8>, PayloadError> {
    let signature = sign_payload(kind, signer, &signature_zeroed_encoding(&payload)?);
    let with_signature_set = with_signature(payload, SignatureBytes(signature.to_bytes()));
    let mac = keys.mac(&mac_zeroed_encoding(&with_signature_set)?);
    Ok(encode(&with_mac(with_signature_set, *mac.as_bytes()))?)
}

/// Decodes a gossip payload and accepts it only when its mac and signature verify.
///
/// # Errors
/// Refuses bytes that do not decode, a wrong mac, and a wrong signature, in that order.
pub fn open_payload(keys: &NetworkKeys, bytes: &[u8]) -> Result<Payload, PayloadError> {
    let payload: Payload = decode(bytes)?;
    keys.verify_mac(
        &mac_zeroed_encoding(&payload)?,
        &Mac::from_bytes(mac_of(&payload)),
    )?;
    verify_signature(&payload)?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KEY_LEN, NetworkSecret};
    use crate::wire::{Addr, Departure, Presence, Unreachable, Version};

    const NETWORK_SECRET: NetworkSecret = NetworkSecret::from_bytes([7; KEY_LEN]);

    fn keys() -> NetworkKeys {
        NetworkKeys::derive(&NETWORK_SECRET)
    }

    fn secret_key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; KEY_LEN])
    }

    fn public_bytes(seed: u8) -> [u8; 32] {
        *secret_key(seed).public().as_bytes()
    }

    fn unsealed_departure(card_seed: u8) -> Payload {
        Payload::Departure(Departure {
            v: Version::CURRENT,
            card: public_bytes(card_seed),
            seq: 41,
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        })
    }

    #[test]
    fn spec_3_4_sealed_departure_opens_with_the_cards_signature_and_a_mac() {
        let sealed = seal_payload(&keys(), &secret_key(1), unsealed_departure(1)).unwrap();

        let opened = open_payload(&keys(), &sealed).unwrap();

        let Payload::Departure(departure) = opened else {
            panic!("a departure was sealed");
        };
        let zeroed = encode(&unsealed_departure(1)).unwrap();
        assert_eq!(
            departure.sig.0,
            sign_payload(SignedPayloadKind::Departure, &secret_key(1), &zeroed).to_bytes()
        );
        assert_ne!(departure.mac, ZERO_MAC);
        assert_eq!(
            decode::<Payload>(&sealed).unwrap(),
            Payload::Departure(departure)
        );
    }

    const REPORTED_CARD_SEED: u8 = 1;
    const REPORTER_SEED: u8 = 2;
    const FOREIGN_SEED: u8 = 3;

    fn unsealed_presence() -> Payload {
        Payload::Presence(Presence {
            v: Version::CURRENT,
            card: public_bytes(REPORTED_CARD_SEED),
            handle: "alice".to_owned(),
            public: b"{}".to_vec(),
            attached: true,
            addr: Addr::from_endpoint_report(
                ["https://relay.example/".to_owned()],
                std::iter::empty(),
            ),
            seq: 5,
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        })
    }

    fn unsealed_unreachable() -> Payload {
        Payload::Unreachable(Unreachable {
            v: Version::CURRENT,
            card: public_bytes(REPORTED_CARD_SEED),
            reporter: public_bytes(REPORTER_SEED),
            ts: 1_700_000_000,
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        })
    }

    #[test]
    fn spec_3_4_each_kind_is_signed_by_its_own_signer_and_by_no_other_key() {
        let kinds_with_their_signer_seed = [
            (unsealed_presence(), REPORTED_CARD_SEED),
            (unsealed_departure(REPORTED_CARD_SEED), REPORTED_CARD_SEED),
            (unsealed_unreachable(), REPORTER_SEED),
        ];

        for (payload, signer_seed) in kinds_with_their_signer_seed {
            let sealed = seal_payload(&keys(), &secret_key(signer_seed), payload.clone()).unwrap();
            assert_eq!(
                open_payload(&keys(), &sealed)
                    .map(|opened| with_mac(with_signature(opened, ZERO_SIGNATURE), ZERO_MAC)),
                Ok(payload.clone())
            );
            for other_seed in [REPORTED_CARD_SEED, REPORTER_SEED, FOREIGN_SEED]
                .into_iter()
                .filter(|seed| *seed != signer_seed)
            {
                assert_eq!(
                    seal_payload(&keys(), &secret_key(other_seed), payload.clone()),
                    Err(PayloadError::SignerIsNotThePayloadSigner)
                );
            }
        }
    }

    fn forged_by(foreign_seed: u8, payload: Payload) -> Vec<u8> {
        let (kind, _) = signer_of(&payload);
        sign_then_mac(&keys(), kind, &secret_key(foreign_seed), payload).unwrap()
    }

    #[test]
    fn test_10_payload_signed_by_a_key_other_than_its_signer_is_dropped() {
        let departure_signed_by_foreign_key =
            forged_by(FOREIGN_SEED, unsealed_departure(REPORTED_CARD_SEED));
        let unreachable_signed_by_the_reported_card =
            forged_by(REPORTED_CARD_SEED, unsealed_unreachable());

        assert_eq!(
            open_payload(&keys(), &departure_signed_by_foreign_key),
            Err(PayloadError::Crypto(CryptoError::SignatureMismatch))
        );
        assert_eq!(
            open_payload(&keys(), &unreachable_signed_by_the_reported_card),
            Err(PayloadError::Crypto(CryptoError::SignatureMismatch))
        );
    }

    const MAC_BYTES_FROM_END_OF_DEPARTURE: usize = 64 + 32;
    const SEQ_BYTE_FROM_END_OF_DEPARTURE: usize = MAC_BYTES_FROM_END_OF_DEPARTURE + 1;

    #[test]
    fn test_27_tampered_payload_is_dropped_by_its_mac_before_its_signature() {
        let sealed = seal_payload(&keys(), &secret_key(1), unsealed_departure(1)).unwrap();

        for position in 0..sealed.len() {
            let mut tampered = sealed.clone();
            tampered[position] ^= 1;
            assert!(open_payload(&keys(), &tampered).is_err(), "byte {position}");
        }
        let mut seq_tampered = sealed.clone();
        seq_tampered[sealed.len() - SEQ_BYTE_FROM_END_OF_DEPARTURE] ^= 1;
        assert_eq!(
            open_payload(&keys(), &seq_tampered),
            Err(PayloadError::Crypto(CryptoError::MacMismatch))
        );
        let mut forged_with_broken_mac =
            forged_by(FOREIGN_SEED, unsealed_departure(REPORTED_CARD_SEED));
        let last_mac_byte = forged_with_broken_mac.len() - 64 - 1;
        forged_with_broken_mac[last_mac_byte] ^= 1;
        assert_eq!(
            open_payload(&keys(), &forged_with_broken_mac),
            Err(PayloadError::Crypto(CryptoError::MacMismatch))
        );
    }

    #[test]
    fn spec_3_4_payload_edited_and_remacked_under_the_network_key_fails_its_signature() {
        let sealed = seal_payload(&keys(), &secret_key(1), unsealed_departure(1)).unwrap();
        let Payload::Departure(mut departure) = decode::<Payload>(&sealed).unwrap() else {
            panic!("a departure was sealed");
        };
        departure.seq += 1;
        let edited = with_mac(Payload::Departure(departure), ZERO_MAC);
        let remacked = with_mac(
            edited.clone(),
            *keys().mac(&encode(&edited).unwrap()).as_bytes(),
        );

        assert_eq!(
            open_payload(&keys(), &encode(&remacked).unwrap()),
            Err(PayloadError::Crypto(CryptoError::SignatureMismatch))
        );
    }

    #[test]
    fn spec_3_4_payload_macked_under_another_network_is_dropped() {
        let other_network = NetworkKeys::derive(&NetworkSecret::from_bytes([8; KEY_LEN]));
        let sealed = seal_payload(&other_network, &secret_key(1), unsealed_departure(1)).unwrap();

        assert_eq!(
            open_payload(&keys(), &sealed),
            Err(PayloadError::Crypto(CryptoError::MacMismatch))
        );
    }
}
