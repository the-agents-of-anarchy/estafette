use std::fmt;

use iroh::{PublicKey, RelayUrl, SecretKey, Signature};

use crate::crypto::{
    CryptoError, MAC_LEN, Mac, NetworkKeys, SignedPayloadKind, sign_payload,
    verify_host_certificate, verify_payload_signature,
};
use crate::wire::{self, CodecError, Infrastructure, Payload, SignatureBytes, decode, encode};

const ZERO_MAC: wire::Mac = [0; MAC_LEN];
const ZERO_SIGNATURE: SignatureBytes = SignatureBytes([0; Signature::LENGTH]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    Codec(CodecError),
    Crypto(CryptoError),
    SignerIsNotThePayloadSigner,
    HostNotCertified,
    RelayUrlUnparsable,
    AddrRelaysNotTheHostRelay,
}

impl fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec(source) => source.fmt(formatter),
            Self::Crypto(source) => source.fmt(formatter),
            Self::SignerIsNotThePayloadSigner => formatter
                .write_str("the signing key is not the key the payload names as its signer"),
            Self::HostNotCertified => {
                formatter.write_str("the owner key did not certify the infrastructure host")
            }
            Self::RelayUrlUnparsable => {
                formatter.write_str("the infrastructure relay is not a relay URL")
            }
            Self::AddrRelaysNotTheHostRelay => {
                formatter.write_str("the infrastructure address must list exactly the host relay")
            }
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

fn verify_infrastructure_rules(
    owner: &PublicKey,
    infrastructure: &Infrastructure,
) -> Result<(), PayloadError> {
    let certified = PublicKey::from_bytes(&infrastructure.host).is_ok_and(|host| {
        verify_host_certificate(owner, &host, &Signature::from_bytes(&infrastructure.cert.0))
            .is_ok()
    });
    if !certified {
        return Err(PayloadError::HostNotCertified);
    }
    let Ok(relay) = infrastructure.relay.as_str().parse::<RelayUrl>() else {
        return Err(PayloadError::RelayUrlUnparsable);
    };
    let addr_relays: Vec<Option<RelayUrl>> = infrastructure
        .addr
        .relays()
        .iter()
        .map(|url| url.as_str().parse().ok())
        .collect();
    if addr_relays != [Some(relay)] {
        return Err(PayloadError::AddrRelaysNotTheHostRelay);
    }
    Ok(())
}

/// Decodes a gossip payload and accepts it only when its mac, signature and, for infrastructure, certificate and relay rules hold.
///
/// # Errors
/// Refuses bytes that do not decode, a wrong mac, a wrong signature, then a failed infrastructure rule, in that order.
pub fn open_payload(
    keys: &NetworkKeys,
    owner: &PublicKey,
    bytes: &[u8],
) -> Result<Payload, PayloadError> {
    let payload: Payload = decode(bytes)?;
    keys.verify_mac(
        &mac_zeroed_encoding(&payload)?,
        &Mac::from_bytes(mac_of(&payload)),
    )?;
    verify_signature(&payload)?;
    match &payload {
        Payload::Infrastructure(infrastructure) => {
            verify_infrastructure_rules(owner, infrastructure)?;
        }
        Payload::Presence(_) | Payload::Departure(_) | Payload::Unreachable(_) => {}
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KEY_LEN, NetworkSecret, sign_host_certificate};
    use crate::wire::{Addr, Departure, Infrastructure, Presence, RelayUrl, Unreachable, Version};

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

        let opened = open_payload(&keys(), &owner_public(), &sealed).unwrap();

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

    fn without_authentication(payload: Payload) -> Payload {
        with_mac(with_signature(payload, ZERO_SIGNATURE), ZERO_MAC)
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
                open_payload(&keys(), &owner_public(), &sealed).map(without_authentication),
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
            open_payload(&keys(), &owner_public(), &departure_signed_by_foreign_key),
            Err(PayloadError::Crypto(CryptoError::SignatureMismatch))
        );
        assert_eq!(
            open_payload(
                &keys(),
                &owner_public(),
                &unreachable_signed_by_the_reported_card
            ),
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
            assert!(
                open_payload(&keys(), &owner_public(), &tampered).is_err(),
                "byte {position}"
            );
        }
        let mut seq_tampered = sealed.clone();
        seq_tampered[sealed.len() - SEQ_BYTE_FROM_END_OF_DEPARTURE] ^= 1;
        assert_eq!(
            open_payload(&keys(), &owner_public(), &seq_tampered),
            Err(PayloadError::Crypto(CryptoError::MacMismatch))
        );
        let mut forged_with_broken_mac =
            forged_by(FOREIGN_SEED, unsealed_departure(REPORTED_CARD_SEED));
        let last_mac_byte = forged_with_broken_mac.len() - 64 - 1;
        forged_with_broken_mac[last_mac_byte] ^= 1;
        assert_eq!(
            open_payload(&keys(), &owner_public(), &forged_with_broken_mac),
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
            open_payload(&keys(), &owner_public(), &encode(&remacked).unwrap()),
            Err(PayloadError::Crypto(CryptoError::SignatureMismatch))
        );
    }

    #[test]
    fn spec_3_4_payload_macked_under_another_network_is_dropped() {
        let other_network = NetworkKeys::derive(&NetworkSecret::from_bytes([8; KEY_LEN]));
        let sealed = seal_payload(&other_network, &secret_key(1), unsealed_departure(1)).unwrap();

        assert_eq!(
            open_payload(&keys(), &owner_public(), &sealed),
            Err(PayloadError::Crypto(CryptoError::MacMismatch))
        );
    }

    const OWNER_SEED: u8 = 4;
    const HOST_SEED: u8 = 5;
    const IMPOSTOR_OWNER_SEED: u8 = 6;
    const HOST_RELAY: &str = "https://relay.example";

    fn owner_public() -> PublicKey {
        secret_key(OWNER_SEED).public()
    }

    fn host_certificate(owner_seed: u8, host_seed: u8) -> SignatureBytes {
        SignatureBytes(
            sign_host_certificate(&secret_key(owner_seed), &secret_key(host_seed).public())
                .to_bytes(),
        )
    }

    fn unsealed_infrastructure(
        relay: &str,
        addr_relays: &[&str],
        certificate: SignatureBytes,
    ) -> Payload {
        Payload::Infrastructure(Infrastructure {
            v: Version::CURRENT,
            host: public_bytes(HOST_SEED),
            relay: RelayUrl::try_from(relay.to_owned()).unwrap(),
            addr: Addr::from_endpoint_report(
                addr_relays.iter().map(|url| (*url).to_owned()),
                std::iter::empty(),
            ),
            ts: 1_700_000_000,
            cert: certificate,
            mac: ZERO_MAC,
            sig: ZERO_SIGNATURE,
        })
    }

    fn certified_infrastructure(relay: &str, addr_relays: &[&str]) -> Payload {
        unsealed_infrastructure(relay, addr_relays, host_certificate(OWNER_SEED, HOST_SEED))
    }

    #[test]
    fn test_27_certified_infrastructure_signed_by_its_host_is_accepted() {
        let payload = certified_infrastructure(HOST_RELAY, &[HOST_RELAY]);
        let sealed = seal_payload(&keys(), &secret_key(HOST_SEED), payload.clone()).unwrap();

        assert_eq!(
            open_payload(&keys(), &owner_public(), &sealed).map(without_authentication),
            Ok(payload)
        );
    }

    #[test]
    fn test_27_infrastructure_signed_by_a_key_other_than_its_host_is_dropped() {
        let payload = certified_infrastructure(HOST_RELAY, &[HOST_RELAY]);
        let sealed = forged_by(FOREIGN_SEED, payload);

        assert_eq!(
            open_payload(&keys(), &owner_public(), &sealed),
            Err(PayloadError::Crypto(CryptoError::SignatureMismatch))
        );
    }

    #[test]
    fn test_27_infrastructure_from_a_host_the_owner_never_certified_is_dropped() {
        let certificates_that_do_not_certify_the_host = [
            host_certificate(IMPOSTOR_OWNER_SEED, HOST_SEED),
            host_certificate(OWNER_SEED, FOREIGN_SEED),
            ZERO_SIGNATURE,
        ];

        for certificate in certificates_that_do_not_certify_the_host {
            let payload = unsealed_infrastructure(HOST_RELAY, &[HOST_RELAY], certificate);
            let sealed = seal_payload(&keys(), &secret_key(HOST_SEED), payload).unwrap();

            assert_eq!(
                open_payload(&keys(), &owner_public(), &sealed),
                Err(PayloadError::HostNotCertified)
            );
        }
    }

    #[test]
    fn spec_3_7_infrastructure_with_a_relay_that_is_no_url_is_dropped() {
        let payload = certified_infrastructure("no relay url", &["no relay url"]);
        let sealed = seal_payload(&keys(), &secret_key(HOST_SEED), payload).unwrap();

        assert_eq!(
            open_payload(&keys(), &owner_public(), &sealed),
            Err(PayloadError::RelayUrlUnparsable)
        );
    }

    #[test]
    fn spec_3_7_infrastructure_addr_must_list_exactly_the_hosts_relay_as_parsed_urls() {
        let other_relay = "https://other-relay.example";
        let addr_relays_that_are_not_exactly_the_relay: [&[&str]; 4] = [
            &[],
            &[other_relay],
            &[HOST_RELAY, other_relay],
            &[HOST_RELAY, HOST_RELAY],
        ];
        for addr_relays in addr_relays_that_are_not_exactly_the_relay {
            let payload = certified_infrastructure(HOST_RELAY, addr_relays);
            let sealed = seal_payload(&keys(), &secret_key(HOST_SEED), payload).unwrap();

            assert_eq!(
                open_payload(&keys(), &owner_public(), &sealed),
                Err(PayloadError::AddrRelaysNotTheHostRelay),
                "{addr_relays:?}"
            );
        }
        let same_relay_spelled_differently =
            certified_infrastructure(HOST_RELAY, &["https://RELAY.example/"]);
        let sealed = seal_payload(
            &keys(),
            &secret_key(HOST_SEED),
            same_relay_spelled_differently.clone(),
        )
        .unwrap();

        assert_eq!(
            open_payload(&keys(), &owner_public(), &sealed).map(without_authentication),
            Ok(same_relay_spelled_differently)
        );
    }
}
