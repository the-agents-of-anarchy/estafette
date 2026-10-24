use std::fmt;

use iroh::{PublicKey, RelayUrl, SecretKey, Signature};

use crate::crypto::{
    CryptoError, MAC_LEN, Mac, NetworkKeys, SignedPayloadKind, sign_payload,
    verify_host_certificate, verify_payload_signature,
};
use crate::wire::{
    self, CodecError, Infrastructure, Payload, Request, Response, SignatureBytes, Wire, decode,
    encode,
};

const GOSSIP_FRAME_BYTES: usize = 4096;
const GOSSIP_LAYER_OVERHEAD_BYTES: usize = 38;
const LARGEST_GOSSIP_PAYLOAD_BYTES: usize = GOSSIP_FRAME_BYTES - GOSSIP_LAYER_OVERHEAD_BYTES;
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
    OverGossipFrame { bytes: usize },
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
            Self::OverGossipFrame { bytes } => write!(
                formatter,
                "a payload of {bytes} bytes is past the {LARGEST_GOSSIP_PAYLOAD_BYTES} bytes the gossip frame of {GOSSIP_FRAME_BYTES} bytes leaves after {GOSSIP_LAYER_OVERHEAD_BYTES} bytes of gossip layer overhead"
            ),
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

const fn signature_of(payload: &Payload) -> SignatureBytes {
    on_each_payload_variant!(payload, inner => inner.sig)
}

trait MacCarrying: Wire + Clone {
    fn mac(&self) -> Option<wire::Mac>;

    fn with_mac(self, mac: wire::Mac) -> Self;
}

impl MacCarrying for Payload {
    fn mac(&self) -> Option<wire::Mac> {
        Some(on_each_payload_variant!(self, inner => inner.mac))
    }

    fn with_mac(mut self, mac: wire::Mac) -> Self {
        on_each_payload_variant!(&mut self, inner => inner.mac = mac);
        self
    }
}

impl MacCarrying for Request {
    fn mac(&self) -> Option<wire::Mac> {
        match self {
            Self::Message(message) => Some(message.mac),
            Self::Probe(probe) => Some(probe.mac),
            Self::Introduce(introduce) => Some(introduce.mac),
        }
    }

    fn with_mac(mut self, mac: wire::Mac) -> Self {
        match &mut self {
            Self::Message(message) => message.mac = mac,
            Self::Probe(probe) => probe.mac = mac,
            Self::Introduce(introduce) => introduce.mac = mac,
        }
        self
    }
}

impl MacCarrying for Response {
    fn mac(&self) -> Option<wire::Mac> {
        match self {
            Self::MessageAck(_) => None,
            Self::ProbeAck(probe_ack) => Some(probe_ack.mac),
            Self::IntroduceAck(introduce_ack) => Some(introduce_ack.mac),
        }
    }

    fn with_mac(mut self, mac: wire::Mac) -> Self {
        match &mut self {
            Self::MessageAck(_) => {}
            Self::ProbeAck(probe_ack) => probe_ack.mac = mac,
            Self::IntroduceAck(introduce_ack) => introduce_ack.mac = mac,
        }
        self
    }
}

const fn with_signature(mut payload: Payload, signature: SignatureBytes) -> Payload {
    on_each_payload_variant!(&mut payload, inner => inner.sig = signature);
    payload
}

fn mac_zeroed_encoding<T: MacCarrying>(structure: &T) -> Result<Vec<u8>, CodecError> {
    encode(&structure.clone().with_mac(ZERO_MAC))
}

fn with_computed_mac<T: MacCarrying>(keys: &NetworkKeys, structure: T) -> Result<T, CodecError> {
    let mac = keys.mac(&mac_zeroed_encoding(&structure)?);
    Ok(structure.with_mac(*mac.as_bytes()))
}

fn verify_mac<T: MacCarrying>(keys: &NetworkKeys, structure: &T) -> Result<(), PayloadError> {
    match structure.mac() {
        Some(mac) => Ok(keys.verify_mac(&mac_zeroed_encoding(structure)?, &Mac::from_bytes(mac))?),
        None => Ok(()),
    }
}

fn seal_with_mac<T: MacCarrying>(
    keys: &NetworkKeys,
    structure: T,
) -> Result<Vec<u8>, PayloadError> {
    Ok(encode(&with_computed_mac(keys, structure)?)?)
}

fn open_with_mac<T: MacCarrying>(keys: &NetworkKeys, bytes: &[u8]) -> Result<T, PayloadError> {
    let structure: T = decode(bytes)?;
    verify_mac(keys, &structure)?;
    Ok(structure)
}

fn signature_zeroed_encoding(payload: &Payload) -> Result<Vec<u8>, CodecError> {
    encode(&with_signature(
        payload.clone().with_mac(ZERO_MAC),
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
/// `PayloadError` for a signer key that is not the payload's own signer and for an encoding past the gossip frame limit.
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
    let encoded = seal_with_mac(keys, with_signature_set)?;
    if encoded.len() > LARGEST_GOSSIP_PAYLOAD_BYTES {
        return Err(PayloadError::OverGossipFrame {
            bytes: encoded.len(),
        });
    }
    Ok(encoded)
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
/// `PayloadError` for bytes that do not decode, a wrong mac, a wrong signature, or a failed infrastructure rule, checked in that order.
pub fn open_payload(
    keys: &NetworkKeys,
    owner: &PublicKey,
    bytes: &[u8],
) -> Result<Payload, PayloadError> {
    let payload: Payload = open_with_mac(keys, bytes)?;
    verify_signature(&payload)?;
    match &payload {
        Payload::Infrastructure(infrastructure) => {
            verify_infrastructure_rules(owner, infrastructure)?;
        }
        Payload::Presence(_) | Payload::Departure(_) | Payload::Unreachable(_) => {}
    }
    Ok(payload)
}

/// Macs and encodes a direct request, ready to write to the stream.
///
/// # Errors
/// `PayloadError::Codec` when the request does not encode within the wire limit.
pub fn seal_request(keys: &NetworkKeys, request: Request) -> Result<Vec<u8>, PayloadError> {
    seal_with_mac(keys, request)
}

/// Decodes a direct request and accepts it only when its mac verifies.
///
/// # Errors
/// `PayloadError` for bytes that do not decode and for a wrong mac.
pub fn open_request(keys: &NetworkKeys, bytes: &[u8]) -> Result<Request, PayloadError> {
    open_with_mac(keys, bytes)
}

/// Macs and encodes a direct response, ready to write to the stream.
///
/// # Errors
/// `PayloadError::Codec` when the response does not encode within the wire limit.
pub fn seal_response(keys: &NetworkKeys, response: Response) -> Result<Vec<u8>, PayloadError> {
    seal_with_mac(keys, response)
}

/// Decodes a direct response and accepts it only when its mac verifies.
///
/// # Errors
/// `PayloadError` for bytes that do not decode and for a wrong mac.
pub fn open_response(keys: &NetworkKeys, bytes: &[u8]) -> Result<Response, PayloadError> {
    open_with_mac(keys, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KEY_LEN, NetworkSecret, sign_host_certificate};
    use crate::wire::{
        Addr, Departure, Infrastructure, Introduce, IntroduceAck, Message, MessageAck, Presence,
        Probe, ProbeAck, RelayUrl, Unreachable, Version,
    };

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
        with_signature(payload, ZERO_SIGNATURE).with_mac(ZERO_MAC)
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
        let edited = Payload::Departure(departure);
        let remacked = with_computed_mac(&keys(), edited).unwrap();

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

    const GOSSIP_FRAME_BYTES: usize = 4096;
    const GOSSIP_LAYER_OVERHEAD_BYTES: usize = 38;
    const LARGEST_GOSSIP_PAYLOAD_BYTES: usize = GOSSIP_FRAME_BYTES - GOSSIP_LAYER_OVERHEAD_BYTES;
    const TWO_BYTE_LENGTH_PREFIX_EXTRA_BYTES: usize = 1;

    fn public_bytes_to_reach(encoded_bytes: usize, without_public: usize) -> usize {
        encoded_bytes
            .checked_sub(without_public)
            .and_then(|remaining| remaining.checked_sub(TWO_BYTE_LENGTH_PREFIX_EXTRA_BYTES))
            .unwrap()
    }

    fn presence_encoding_to_bytes(encoded_bytes: usize) -> Payload {
        let Payload::Presence(presence) = unsealed_presence() else {
            panic!("a presence was built");
        };
        let empty_public = Presence {
            public: Vec::new(),
            ..presence
        };
        let without_public = encode(&Payload::Presence(empty_public.clone()))
            .unwrap()
            .len();
        Payload::Presence(Presence {
            public: vec![b' '; public_bytes_to_reach(encoded_bytes, without_public)],
            ..empty_public
        })
    }

    #[test]
    fn spec_3_7_payload_past_the_gossip_frame_less_its_overhead_is_refused_before_send() {
        let at_the_limit = presence_encoding_to_bytes(LARGEST_GOSSIP_PAYLOAD_BYTES);
        let one_past = presence_encoding_to_bytes(LARGEST_GOSSIP_PAYLOAD_BYTES + 1);

        let sealed = seal_payload(&keys(), &secret_key(REPORTED_CARD_SEED), at_the_limit).unwrap();

        assert_eq!(sealed.len(), LARGEST_GOSSIP_PAYLOAD_BYTES);
        assert_eq!(
            seal_payload(&keys(), &secret_key(REPORTED_CARD_SEED), one_past),
            Err(PayloadError::OverGossipFrame {
                bytes: LARGEST_GOSSIP_PAYLOAD_BYTES + 1
            })
        );
    }

    const MESSAGE_ID: [u8; 16] = [0x5A; 16];
    const NONCE: [u8; 16] = [0xC3; 16];

    fn unsealed_requests() -> [Request; 3] {
        [
            Request::Message(Message {
                v: Version::CURRENT,
                id: MESSAGE_ID,
                to: public_bytes(REPORTER_SEED),
                handle: "alice".to_owned(),
                attached: true,
                sent_at: 1_700_000_000,
                body: "hello".to_owned(),
                mac: ZERO_MAC,
            }),
            Request::Probe(Probe {
                v: Version::CURRENT,
                nonce: NONCE,
                mac: ZERO_MAC,
            }),
            Request::Introduce(Introduce {
                v: Version::CURRENT,
                handle: Some("alice".to_owned()),
                public: b"{}".to_vec(),
                attached: false,
                addr: Addr::from_endpoint_report(
                    ["https://relay.example/".to_owned()],
                    std::iter::empty(),
                ),
                mac: ZERO_MAC,
            }),
        ]
    }

    fn unsealed_macked_responses() -> [Response; 2] {
        [
            Response::ProbeAck(ProbeAck {
                v: Version::CURRENT,
                nonce: NONCE,
                attached: true,
                mac: ZERO_MAC,
            }),
            Response::IntroduceAck(IntroduceAck {
                v: Version::CURRENT,
                handle: None,
                public: Vec::new(),
                attached: true,
                addr: Addr::from_endpoint_report(
                    ["https://relay.example/".to_owned()],
                    std::iter::empty(),
                ),
                mac: ZERO_MAC,
            }),
        ]
    }

    fn message_ack() -> Response {
        Response::MessageAck(MessageAck {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            accepted: false,
            attached: true,
            reason: Some("card_invalid".to_owned()),
        })
    }

    fn other_network() -> NetworkKeys {
        NetworkKeys::derive(&NetworkSecret::from_bytes([8; KEY_LEN]))
    }

    #[test]
    fn spec_3_4_sealed_request_carries_the_keyed_mac_of_its_zero_mac_encoding() {
        for request in unsealed_requests() {
            let expected_mac = keys().mac(&encode(&request).unwrap());

            let sealed = seal_request(&keys(), request.clone()).unwrap();

            assert_eq!(
                open_request(&keys(), &sealed),
                Ok(request.with_mac(*expected_mac.as_bytes()))
            );
        }
    }

    #[test]
    fn test_9_request_with_a_wrong_mac_is_dropped() {
        for request in unsealed_requests() {
            let sealed = seal_request(&keys(), request.clone()).unwrap();
            let sealed_under_other_network = seal_request(&other_network(), request).unwrap();

            assert_eq!(
                open_request(&keys(), &sealed_under_other_network),
                Err(PayloadError::Crypto(CryptoError::MacMismatch))
            );
            for position in 0..sealed.len() {
                let mut tampered = sealed.clone();
                tampered[position] ^= 1;
                assert!(open_request(&keys(), &tampered).is_err(), "byte {position}");
            }
        }
    }

    #[test]
    fn spec_3_4_response_with_a_mac_field_is_sealed_and_dropped_on_a_wrong_mac() {
        for response in unsealed_macked_responses() {
            let expected_mac = keys().mac(&encode(&response).unwrap());

            let sealed = seal_response(&keys(), response.clone()).unwrap();
            let sealed_under_other_network =
                seal_response(&other_network(), response.clone()).unwrap();

            assert_eq!(
                open_response(&keys(), &sealed),
                Ok(response.with_mac(*expected_mac.as_bytes()))
            );
            assert_eq!(
                open_response(&keys(), &sealed_under_other_network),
                Err(PayloadError::Crypto(CryptoError::MacMismatch))
            );
            for position in 0..sealed.len() {
                let mut tampered = sealed.clone();
                tampered[position] ^= 1;
                assert!(
                    open_response(&keys(), &tampered).is_err(),
                    "byte {position}"
                );
            }
        }
    }

    #[test]
    fn spec_3_6_message_ack_has_no_mac_field_so_it_travels_as_encoded() {
        let sealed = seal_response(&other_network(), message_ack()).unwrap();

        assert_eq!(sealed, encode(&message_ack()).unwrap());
        assert_eq!(open_response(&keys(), &sealed), Ok(message_ack()));
    }
}
