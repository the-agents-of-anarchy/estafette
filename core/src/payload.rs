use std::fmt;

use iroh::{PublicKey, RelayUrl, SecretKey, Signature};

use crate::card::is_card_public;
use crate::config::{AcceptedValue, host_relay_url};
use crate::crypto::{
    CryptoError, MAC_LEN, Mac, NetworkKeys, SignedPayloadKind, sign_payload,
    verify_host_certificate, verify_payload_signature,
};
use crate::wire::{
    self, CodecError, Departure, Infrastructure, Payload, Presence, PresenceRequest, Request,
    Response, SignatureBytes, Unreachable, Wire, decode, encode,
};

const GOSSIP_FRAME_BYTES: usize = 4096;
const GOSSIP_LAYER_OVERHEAD_BYTES: usize = 38;
const LARGEST_GOSSIP_PAYLOAD_BYTES: usize = GOSSIP_FRAME_BYTES - GOSSIP_LAYER_OVERHEAD_BYTES;
pub(crate) const ZERO_MAC: wire::Mac = [0; MAC_LEN];
pub(crate) const ZERO_SIGNATURE: SignatureBytes = SignatureBytes([0; Signature::LENGTH]);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    Codec(CodecError),
    Crypto(CryptoError),
    SignerIsNotThePayloadSigner,
    HostNotCertified,
    HostRelayRefused,
    AddrRelaysNotTheHostRelay,
    PublicNotAdmitted,
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
            Self::HostRelayRefused => write!(
                formatter,
                "the infrastructure relay is not {}",
                AcceptedValue::RelayUrl
            ),
            Self::AddrRelaysNotTheHostRelay => {
                formatter.write_str("the infrastructure address must list exactly the host relay")
            }
            Self::PublicNotAdmitted => formatter.write_str(
                "the presence public is not what a card sends: a JSON mapping within the serialized cap and the deepest nesting, naming each key once, with integers that fit 64 bits",
            ),
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
            Payload::PresenceRequest($inner) => $body,
        }
    };
}

fn signer_of(payload: &Payload) -> (SignedPayloadKind, wire::PublicKey) {
    let facts = payload.facts();
    (facts.kind, facts.signer)
}

/// The number a payload is ordered by: the seq of a Presence, Departure or `PresenceRequest`, the ts of an Unreachable or Infrastructure record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadStamp {
    Seq(u64),
    Ts(u64),
}

/// What a gossip payload says of itself, whether or not it verified: its kind, the key it names as its signer, and its stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadFacts {
    pub kind: SignedPayloadKind,
    pub signer: wire::PublicKey,
    pub stamp: PayloadStamp,
}

/// A gossip payload that tells what it says of itself.
pub trait DescribedPayload {
    /// The payload's kind, the key it names as its signer, and its stamp.
    fn facts(&self) -> PayloadFacts;
}

impl DescribedPayload for Presence {
    fn facts(&self) -> PayloadFacts {
        PayloadFacts {
            kind: SignedPayloadKind::Presence,
            signer: self.card,
            stamp: PayloadStamp::Seq(self.seq),
        }
    }
}

impl DescribedPayload for Departure {
    fn facts(&self) -> PayloadFacts {
        PayloadFacts {
            kind: SignedPayloadKind::Departure,
            signer: self.card,
            stamp: PayloadStamp::Seq(self.seq),
        }
    }
}

impl DescribedPayload for Unreachable {
    fn facts(&self) -> PayloadFacts {
        PayloadFacts {
            kind: SignedPayloadKind::Unreachable,
            signer: self.reporter,
            stamp: PayloadStamp::Ts(self.ts),
        }
    }
}

impl DescribedPayload for Infrastructure {
    fn facts(&self) -> PayloadFacts {
        PayloadFacts {
            kind: SignedPayloadKind::Infrastructure,
            signer: self.host,
            stamp: PayloadStamp::Ts(self.ts),
        }
    }
}

impl DescribedPayload for PresenceRequest {
    fn facts(&self) -> PayloadFacts {
        PayloadFacts {
            kind: SignedPayloadKind::PresenceRequest,
            signer: self.requester,
            stamp: PayloadStamp::Seq(self.seq),
        }
    }
}

impl DescribedPayload for Payload {
    fn facts(&self) -> PayloadFacts {
        on_each_payload_variant!(self, inner => inner.facts())
    }
}

/// Each rule a node drops a received gossip payload by, at the payload layer or at its admission (spec 3.4, 3.7, 3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadDropRule {
    /// A Presence or Departure signed by this node's own card.
    FromOwnCard,
    /// A stamp older than the announcement max age by this node's clock.
    OlderThanMaxAge,
    /// A Presence or Departure whose seq is not above the seq held for its card.
    NotNewerThanHeld,
    /// A Presence or Departure whose seq is not above the Departure held for its card.
    NotNewerThanDeparture,
    /// A Presence whose handle is outside the announced-handle grammar.
    HandleOutsideGrammar,
    /// An Unreachable report observed before the seq held for the card it reports.
    ReportOlderThanHeldSeq,
    /// An Unreachable report observed before this node last contacted the card it reports directly.
    ReportBeforeDirectContact,
    /// An Unreachable report about a card the table does not hold.
    ReportedCardUnknown,
    /// An Unreachable report about a card that departed.
    ReportedCardGone,
    /// An Unreachable report about this node observed before its last Presence.
    ReportOlderThanLastPresence,
    /// A `PresenceRequest` whose seq is not above the last one heard from its requester.
    NotAboveRequesterSeq,
    /// A `PresenceRequest` dated further ahead of this node's clock than the announcement max age.
    AheadOfMaxAge,
    /// An Infrastructure record whose ts is not above the one held for its host.
    RecordNotNewerThanHeld,
    /// An Infrastructure record whose ts is past the future bound of this node's clock.
    RecordPastFutureBound,
    /// Bytes longer than the wire limit.
    PastWireLimit,
    /// Bytes left over after a whole payload decoded.
    TrailingBytes,
    /// A payload whose zeroed encoding, which its mac or signature covers, does not encode.
    Unencodable,
    /// Bytes that do not decode as a payload.
    Malformed,
    /// A payload of a wire version this node does not speak.
    UnsupportedVersion,
    /// A relay URL longer than the wire allows.
    RelayUrlTooLong,
    /// An address listing more entries than the wire allows.
    AddrOverCap,
    /// A mac that the network key did not produce.
    MacMismatch,
    /// A signature its named signer did not make for its kind.
    SignatureMismatch,
    /// An Infrastructure record whose host the owner key did not certify.
    HostNotCertified,
    /// An Infrastructure record whose relay is no host relay.
    HostRelayRefused,
    /// An Infrastructure record whose address lists anything but exactly its relay.
    AddrRelaysNotTheHostRelay,
    /// A Presence whose `public` is not what a card sends.
    PublicNotAdmitted,
}

/// Why the payload layer refused bytes received on the topic, each cause one the opening checks apply.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OpeningFailure {
    Codec(CodecError),
    Crypto(CryptoError),
    HostNotCertified,
    HostRelayRefused,
    AddrRelaysNotTheHostRelay,
    PublicNotAdmitted,
}

impl From<CodecError> for OpeningFailure {
    fn from(source: CodecError) -> Self {
        Self::Codec(source)
    }
}

impl From<CryptoError> for OpeningFailure {
    fn from(source: CryptoError) -> Self {
        Self::Crypto(source)
    }
}

impl From<OpeningFailure> for PayloadError {
    fn from(failure: OpeningFailure) -> Self {
        match failure {
            OpeningFailure::Codec(source) => Self::Codec(source),
            OpeningFailure::Crypto(source) => Self::Crypto(source),
            OpeningFailure::HostNotCertified => Self::HostNotCertified,
            OpeningFailure::HostRelayRefused => Self::HostRelayRefused,
            OpeningFailure::AddrRelaysNotTheHostRelay => Self::AddrRelaysNotTheHostRelay,
            OpeningFailure::PublicNotAdmitted => Self::PublicNotAdmitted,
        }
    }
}

impl OpeningFailure {
    /// The drop rule the failure applies.
    const fn drop_rule(&self) -> PayloadDropRule {
        match self {
            Self::Codec(CodecError::PastWireLimit { .. }) => PayloadDropRule::PastWireLimit,
            Self::Codec(CodecError::TrailingBytes) => PayloadDropRule::TrailingBytes,
            Self::Codec(CodecError::Unencodable(_)) => PayloadDropRule::Unencodable,
            Self::Codec(CodecError::Malformed(_)) => PayloadDropRule::Malformed,
            Self::Codec(CodecError::UnsupportedVersion { .. }) => {
                PayloadDropRule::UnsupportedVersion
            }
            Self::Codec(CodecError::RelayUrlTooLong { .. }) => PayloadDropRule::RelayUrlTooLong,
            Self::Codec(CodecError::AddrOverCap) => PayloadDropRule::AddrOverCap,
            Self::Crypto(CryptoError::MacMismatch) => PayloadDropRule::MacMismatch,
            Self::Crypto(CryptoError::SignatureMismatch) => PayloadDropRule::SignatureMismatch,
            Self::HostNotCertified => PayloadDropRule::HostNotCertified,
            Self::HostRelayRefused => PayloadDropRule::HostRelayRefused,
            Self::AddrRelaysNotTheHostRelay => PayloadDropRule::AddrRelaysNotTheHostRelay,
            Self::PublicNotAdmitted => PayloadDropRule::PublicNotAdmitted,
        }
    }
}

/// Bytes received on the topic that the payload layer dropped: what they said of themselves when they decoded, and the rule that dropped them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadRefusal {
    pub facts: Option<PayloadFacts>,
    pub rule: PayloadDropRule,
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

fn verify_mac<T: MacCarrying>(keys: &NetworkKeys, structure: &T) -> Result<(), OpeningFailure> {
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

fn verify_signature(payload: &Payload) -> Result<(), OpeningFailure> {
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

/// Signs, macs and encodes a gossip payload with any key, so a test can forge a payload its named signer never signed.
///
/// # Errors
/// `PayloadError` for an encoding past the gossip frame limit.
#[cfg(test)]
pub(crate) fn sealed_by_any_key(
    keys: &NetworkKeys,
    signer: &SecretKey,
    payload: Payload,
) -> Result<Vec<u8>, PayloadError> {
    let (kind, _named_signer) = signer_of(&payload);
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

/// An Infrastructure record whose certificate, relay and address rules hold, carrying its relay parsed; only `open_payload` hands one out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidInfrastructure {
    record: Infrastructure,
    relay: RelayUrl,
}

impl ValidInfrastructure {
    /// The record as it arrived.
    #[must_use]
    pub const fn record(&self) -> &Infrastructure {
        &self.record
    }

    /// The record's relay, a host relay by construction.
    #[must_use]
    pub const fn relay(&self) -> &RelayUrl {
        &self.relay
    }
}

/// A gossip payload whose mac, signature and kind rules hold: each kind its wire structure, an Infrastructure record its validated type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenedPayload {
    Presence(Presence),
    Departure(Departure),
    Unreachable(Unreachable),
    Infrastructure(ValidInfrastructure),
    PresenceRequest(PresenceRequest),
}

#[cfg(test)]
impl From<OpenedPayload> for Payload {
    /// The wire structure an opened payload carries.
    fn from(opened: OpenedPayload) -> Self {
        match opened {
            OpenedPayload::Presence(presence) => Self::Presence(presence),
            OpenedPayload::Departure(departure) => Self::Departure(departure),
            OpenedPayload::Unreachable(report) => Self::Unreachable(report),
            OpenedPayload::Infrastructure(valid) => Self::Infrastructure(valid.record),
            OpenedPayload::PresenceRequest(request) => Self::PresenceRequest(request),
        }
    }
}

/// Accepts an Infrastructure record only when the owner certified its host, its relay is a host relay, and its address lists exactly that relay.
fn validated_infrastructure(
    owner: &PublicKey,
    infrastructure: Infrastructure,
) -> Result<ValidInfrastructure, OpeningFailure> {
    let certified = PublicKey::from_bytes(&infrastructure.host).is_ok_and(|host| {
        verify_host_certificate(owner, &host, &Signature::from_bytes(&infrastructure.cert.0))
            .is_ok()
    });
    if !certified {
        return Err(OpeningFailure::HostNotCertified);
    }
    let Some(relay) = host_relay_url(infrastructure.relay.as_str()) else {
        return Err(OpeningFailure::HostRelayRefused);
    };
    let addr_relays: Vec<Option<RelayUrl>> = infrastructure
        .addr
        .relays()
        .iter()
        .map(|url| url.as_str().parse().ok())
        .collect();
    if addr_relays != [Some(relay.clone())] {
        return Err(OpeningFailure::AddrRelaysNotTheHostRelay);
    }
    Ok(ValidInfrastructure {
        record: infrastructure,
        relay,
    })
}

fn verify_presence_rules(presence: &Presence) -> Result<(), OpeningFailure> {
    if is_card_public(&presence.public) {
        Ok(())
    } else {
        Err(OpeningFailure::PublicNotAdmitted)
    }
}

/// Decodes a gossip payload and accepts it only when its mac, its signature, and its kind's rules hold: certificate and relay for infrastructure, the card's `public` rule for presence.
///
/// # Errors
/// `PayloadError` for bytes that do not decode, a wrong mac, a wrong signature, or a failed infrastructure or presence rule, checked in that order.
pub fn open_payload(
    keys: &NetworkKeys,
    owner: &PublicKey,
    bytes: &[u8],
) -> Result<OpenedPayload, PayloadError> {
    opened_or_failure(keys, owner, bytes).map_err(|(_facts, failure)| failure.into())
}

/// Opens a gossip payload as `open_payload` does, naming for a refusal the rule that dropped it and what the payload said of itself when it decoded.
///
/// # Errors
/// `PayloadRefusal` for bytes `open_payload` refuses, with the rule its error applies.
pub fn open_described_payload(
    keys: &NetworkKeys,
    owner: &PublicKey,
    bytes: &[u8],
) -> Result<OpenedPayload, PayloadRefusal> {
    opened_or_failure(keys, owner, bytes).map_err(|(facts, failure)| PayloadRefusal {
        facts,
        rule: failure.drop_rule(),
    })
}

/// Decodes, then checks mac, signature and the kind's rules in that order, giving for a failure what the payload said of itself when it decoded.
fn opened_or_failure(
    keys: &NetworkKeys,
    owner: &PublicKey,
    bytes: &[u8],
) -> Result<OpenedPayload, (Option<PayloadFacts>, OpeningFailure)> {
    let payload: Payload = decode(bytes).map_err(|undecoded| (None, undecoded.into()))?;
    let facts = payload.facts();
    let refused = |failure: OpeningFailure| (Some(facts), failure);
    verify_mac(keys, &payload).map_err(refused)?;
    verify_signature(&payload).map_err(refused)?;
    Ok(match payload {
        Payload::Presence(presence) => {
            verify_presence_rules(&presence).map_err(refused)?;
            OpenedPayload::Presence(presence)
        }
        Payload::Departure(departure) => OpenedPayload::Departure(departure),
        Payload::Unreachable(report) => OpenedPayload::Unreachable(report),
        Payload::Infrastructure(infrastructure) => OpenedPayload::Infrastructure(
            validated_infrastructure(owner, infrastructure).map_err(refused)?,
        ),
        Payload::PresenceRequest(request) => OpenedPayload::PresenceRequest(request),
    })
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
    use crate::card::tests::{SPEC_PUBLIC_CAP_BYTES, public_mapping_of_bytes};
    use crate::crypto::{KEY_LEN, NetworkSecret, sign_host_certificate};
    use crate::wire::{
        Addr, Departure, Infrastructure, Introduce, IntroduceAck, Message, MessageAck, Presence,
        PresenceRequest, Probe, ProbeAck, RelayUrl, Unreachable, Version,
    };

    const NETWORK_SECRET: NetworkSecret = NetworkSecret::from_bytes([7; KEY_LEN]);
    const OTHER_NETWORK_SECRET: NetworkSecret = NetworkSecret::from_bytes([8; KEY_LEN]);

    fn keys() -> NetworkKeys {
        NetworkKeys::derive(&NETWORK_SECRET)
    }

    fn other_network() -> NetworkKeys {
        NetworkKeys::derive(&OTHER_NETWORK_SECRET)
    }

    fn secret_key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; KEY_LEN])
    }

    fn public_bytes(seed: u8) -> wire::PublicKey {
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

        let Payload::Departure(departure) = Payload::from(opened) else {
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

    fn unsealed_presence_request() -> Payload {
        Payload::PresenceRequest(PresenceRequest {
            v: Version::CURRENT,
            requester: public_bytes(REPORTER_SEED),
            seq: 1_700_000_000_007,
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
            (unsealed_presence_request(), REPORTER_SEED),
        ];

        for (payload, signer_seed) in kinds_with_their_signer_seed {
            let sealed = seal_payload(&keys(), &secret_key(signer_seed), payload.clone()).unwrap();
            assert_eq!(opened(&sealed), Ok(payload.clone()));
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
        let presence_request_signed_by_foreign_key =
            forged_by(FOREIGN_SEED, unsealed_presence_request());

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
        assert_eq!(
            open_payload(
                &keys(),
                &owner_public(),
                &presence_request_signed_by_foreign_key
            ),
            Err(PayloadError::Crypto(CryptoError::SignatureMismatch))
        );
    }

    const SIGNATURE_BYTES_AT_END: usize = Signature::LENGTH;
    const MAC_BYTES_FROM_END_OF_DEPARTURE: usize = SIGNATURE_BYTES_AT_END + MAC_LEN;
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
        let last_mac_byte = forged_with_broken_mac.len() - SIGNATURE_BYTES_AT_END - 1;
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
        let sealed = seal_payload(&other_network(), &secret_key(1), unsealed_departure(1)).unwrap();

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

        assert_eq!(opened(&sealed), Ok(payload));
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
            Err(PayloadError::HostRelayRefused)
        );
    }

    /// Spec 3.3 and 3.8: a record whose relay the network configuration would refuse is dropped, so nothing of it is held or saved.
    #[test]
    fn spec_3_3_infrastructure_with_a_relay_the_configuration_refuses_is_dropped() {
        let plain_http_relay = "http://relay.example/";
        let payload = certified_infrastructure(plain_http_relay, &[plain_http_relay]);
        let sealed = seal_payload(&keys(), &secret_key(HOST_SEED), payload).unwrap();

        assert_eq!(
            open_payload(&keys(), &owner_public(), &sealed),
            Err(PayloadError::HostRelayRefused)
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

        assert_eq!(opened(&sealed), Ok(same_relay_spelled_differently));
    }

    const SPEC_GOSSIP_FRAME_BYTES: usize = 4096;
    const SPEC_GOSSIP_LAYER_OVERHEAD_BYTES: usize = 38;
    const SPEC_LARGEST_GOSSIP_PAYLOAD_BYTES: usize =
        SPEC_GOSSIP_FRAME_BYTES - SPEC_GOSSIP_LAYER_OVERHEAD_BYTES;
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
        let at_the_limit = presence_encoding_to_bytes(SPEC_LARGEST_GOSSIP_PAYLOAD_BYTES);
        let one_past = presence_encoding_to_bytes(SPEC_LARGEST_GOSSIP_PAYLOAD_BYTES + 1);

        let sealed = seal_payload(&keys(), &secret_key(REPORTED_CARD_SEED), at_the_limit).unwrap();

        assert_eq!(sealed.len(), SPEC_LARGEST_GOSSIP_PAYLOAD_BYTES);
        assert_eq!(
            seal_payload(&keys(), &secret_key(REPORTED_CARD_SEED), one_past),
            Err(PayloadError::OverGossipFrame {
                bytes: SPEC_LARGEST_GOSSIP_PAYLOAD_BYTES + 1
            })
        );
    }

    fn presence_with_public(public: Vec<u8>) -> Payload {
        let Payload::Presence(presence) = unsealed_presence() else {
            panic!("a presence was built");
        };
        Payload::Presence(Presence { public, ..presence })
    }

    fn sealed_presence_with_public(public: Vec<u8>) -> Vec<u8> {
        let payload = presence_with_public(public);
        seal_payload(&keys(), &secret_key(REPORTED_CARD_SEED), payload).unwrap()
    }

    fn opened(sealed: &[u8]) -> Result<Payload, PayloadError> {
        open_payload(&keys(), &owner_public(), sealed)
            .map(Payload::from)
            .map(without_authentication)
    }

    /// Spec 3.5 and 3.7: a Presence whose `public` mapping is past 3072 bytes is dropped, one at 3072 bytes is accepted.
    #[test]
    fn spec_3_7_presence_whose_public_is_past_3072_bytes_is_dropped() {
        let at_the_cap = public_mapping_of_bytes(SPEC_PUBLIC_CAP_BYTES);
        let one_past = public_mapping_of_bytes(SPEC_PUBLIC_CAP_BYTES + 1);

        assert_eq!(
            opened(&sealed_presence_with_public(at_the_cap.clone())),
            Ok(presence_with_public(at_the_cap))
        );
        assert_eq!(
            opened(&sealed_presence_with_public(one_past)),
            Err(PayloadError::PublicNotAdmitted)
        );
    }

    /// Spec 3.5 and 3.7: a Presence whose `public` is not a JSON mapping, as a card's `public` always is, is dropped.
    #[test]
    fn spec_3_7_presence_whose_public_is_no_json_mapping_is_dropped() {
        let not_a_mapping: [&[u8]; 6] = [
            b"",
            b"[]",
            b"\"text\"",
            b"null",
            b"{\"k\":1",
            b"{\"k\":1}{}",
        ];

        for public in not_a_mapping {
            assert_eq!(
                opened(&sealed_presence_with_public(public.to_vec())),
                Err(PayloadError::PublicNotAdmitted),
                "{}",
                String::from_utf8_lossy(public)
            );
        }
    }

    /// Spec 3.5 and 3.7: a Presence whose `public` names a key twice, written plainly, escaped or nested, or holds an integer past 64 bits, which no card's `public` serializes to, is dropped.
    #[test]
    fn spec_3_7_presence_whose_public_no_card_sends_is_dropped() {
        let no_card_sends: [&[u8]; 5] = [
            br#"{"a":1,"\u0061":2}"#,
            br#"{"a":1,"a":2}"#,
            br#"{"x":{"a":1,"a":2}}"#,
            br#"{"a":18446744073709551616}"#,
            br#"{"a":-9223372036854775809}"#,
        ];

        for public in no_card_sends {
            assert_eq!(
                opened(&sealed_presence_with_public(public.to_vec())),
                Err(PayloadError::PublicNotAdmitted),
                "{}",
                String::from_utf8_lossy(public)
            );
        }
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

    const SPEC_WIRE_LIMIT_BYTES: usize = 1_114_112;
    const SPEC_RELAY_URLS_PER_ADDR_CAP: u8 = 4;
    const RELAY_URLS_PAST_THE_COUNT_CAP: u8 = 5;
    const SPEC_RELAY_URL_CAP_BYTES: usize = 128;
    const VERSION_BYTE_INDEX: usize = 1;
    const UNSUPPORTED_VERSION: u8 = 2;
    const PRESENCE_RELAY: &str = "https://relay.example/";
    const RELAY_PADDING: u8 = b'a';
    const LENGTH_128_VARINT: [u8; 2] = [0x80, 0x01];
    const LENGTH_129_VARINT: [u8; 2] = [0x81, 0x01];
    const TRAILING_BYTE: u8 = 0;
    const PLAIN_HTTP_RELAY: &str = "http://relay.example/";
    const OTHER_RELAY: &str = "https://other-relay.example";
    const NOT_A_JSON_MAPPING: &[u8] = b"[]";

    fn sealed_presence_with_relays(relays: &[String]) -> Vec<u8> {
        let Payload::Presence(presence) = unsealed_presence() else {
            panic!("a presence was built");
        };
        let payload = Payload::Presence(Presence {
            addr: Addr::from_endpoint_report(relays.iter().cloned(), std::iter::empty()),
            ..presence
        });
        seal_payload(&keys(), &secret_key(REPORTED_CARD_SEED), payload).unwrap()
    }

    fn position_of(bytes: &[u8], wanted: &[u8]) -> usize {
        bytes
            .windows(wanted.len())
            .position(|window| window == wanted)
            .unwrap()
    }

    fn sealed_presence() -> Vec<u8> {
        seal_payload(
            &keys(),
            &secret_key(REPORTED_CARD_SEED),
            unsealed_presence(),
        )
        .unwrap()
    }

    /// A sealed Presence whose one relay URL is spliced one byte past the cap, its length prefix with it.
    fn presence_with_a_relay_url_past_the_cap() -> Vec<u8> {
        let mut at_the_cap = PRESENCE_RELAY.as_bytes().to_vec();
        at_the_cap.resize(SPEC_RELAY_URL_CAP_BYTES, RELAY_PADDING);
        let url = String::from_utf8(at_the_cap).unwrap();
        let sealed = sealed_presence_with_relays(std::slice::from_ref(&url));
        let prefixed = [LENGTH_128_VARINT.as_slice(), url.as_bytes()].concat();
        let (before, from_the_url) = sealed.split_at(position_of(&sealed, &prefixed));
        let after = from_the_url.strip_prefix(prefixed.as_slice()).unwrap();
        [
            before,
            &LENGTH_129_VARINT,
            url.as_bytes(),
            &[RELAY_PADDING],
            after,
        ]
        .concat()
    }

    /// A sealed Presence at the relay URL count cap, spliced to hold one relay URL more, its count with it.
    fn presence_with_one_relay_url_past_the_count_cap() -> Vec<u8> {
        let relays = vec![PRESENCE_RELAY.to_owned(); usize::from(SPEC_RELAY_URLS_PER_ADDR_CAP)];
        let sealed = sealed_presence_with_relays(&relays);
        let one_relay = [
            [u8::try_from(PRESENCE_RELAY.len()).unwrap()].as_slice(),
            PRESENCE_RELAY.as_bytes(),
        ]
        .concat();
        let counted = [[SPEC_RELAY_URLS_PER_ADDR_CAP].as_slice(), &one_relay].concat();
        let (before, from_the_count) = sealed.split_at(position_of(&sealed, &counted));
        let after = from_the_count.strip_prefix(counted.as_slice()).unwrap();
        [
            before,
            &[RELAY_URLS_PAST_THE_COUNT_CAP],
            &one_relay,
            &one_relay,
            after,
        ]
        .concat()
    }

    fn refused_by_the_payload_layer(bytes: &[u8]) -> PayloadRefusal {
        open_described_payload(&keys(), &owner_public(), bytes).unwrap_err()
    }

    /// Observability decision 9: each input meets one payload-layer rule alone, and the refusal names that rule.
    #[test]
    fn observability_decision_9_a_payload_the_payload_layer_refuses_is_named_by_the_one_rule_it_meets()
     {
        let presence_facts = Some(unsealed_presence().facts());
        let infrastructure = |relay: &str, addr_relays: &[&str], certificate| {
            let payload = unsealed_infrastructure(relay, addr_relays, certificate);
            let facts = Some(payload.facts());
            let sealed = seal_payload(&keys(), &secret_key(HOST_SEED), payload).unwrap();
            (sealed, facts)
        };
        let certified = host_certificate(OWNER_SEED, HOST_SEED);
        let mut with_another_version = sealed_presence();
        with_another_version[VERSION_BYTE_INDEX] = UNSUPPORTED_VERSION;
        let whole = sealed_presence();
        let (_last_byte, truncated) = whole.split_last().unwrap();
        let mut with_a_trailing_byte = sealed_presence();
        with_a_trailing_byte.push(TRAILING_BYTE);
        let cases = [
            (
                (vec![0; SPEC_WIRE_LIMIT_BYTES + 1], None),
                PayloadDropRule::PastWireLimit,
            ),
            ((with_a_trailing_byte, None), PayloadDropRule::TrailingBytes),
            ((truncated.to_vec(), None), PayloadDropRule::Malformed),
            (
                (with_another_version, None),
                PayloadDropRule::UnsupportedVersion,
            ),
            (
                (presence_with_a_relay_url_past_the_cap(), None),
                PayloadDropRule::RelayUrlTooLong,
            ),
            (
                (presence_with_one_relay_url_past_the_count_cap(), None),
                PayloadDropRule::AddrOverCap,
            ),
            (
                (
                    seal_payload(
                        &other_network(),
                        &secret_key(REPORTED_CARD_SEED),
                        unsealed_presence(),
                    )
                    .unwrap(),
                    presence_facts,
                ),
                PayloadDropRule::MacMismatch,
            ),
            (
                (forged_by(FOREIGN_SEED, unsealed_presence()), presence_facts),
                PayloadDropRule::SignatureMismatch,
            ),
            (
                infrastructure(
                    HOST_RELAY,
                    &[HOST_RELAY],
                    host_certificate(IMPOSTOR_OWNER_SEED, HOST_SEED),
                ),
                PayloadDropRule::HostNotCertified,
            ),
            (
                infrastructure(PLAIN_HTTP_RELAY, &[PLAIN_HTTP_RELAY], certified),
                PayloadDropRule::HostRelayRefused,
            ),
            (
                infrastructure(HOST_RELAY, &[OTHER_RELAY], certified),
                PayloadDropRule::AddrRelaysNotTheHostRelay,
            ),
            (
                (
                    sealed_presence_with_public(NOT_A_JSON_MAPPING.to_vec()),
                    Some(presence_with_public(NOT_A_JSON_MAPPING.to_vec()).facts()),
                ),
                PayloadDropRule::PublicNotAdmitted,
            ),
        ];

        for ((bytes, facts), rule) in cases {
            assert_eq!(
                refused_by_the_payload_layer(&bytes),
                PayloadRefusal { facts, rule },
                "{rule:?}"
            );
        }
    }

    /// Observability decision 9: a re-encoding that fails is named unencodable, a failure no received bytes reach, since every structure that decodes encodes again.
    #[test]
    fn observability_decision_9_a_failed_re_encoding_is_named_unencodable() {
        assert_eq!(
            OpeningFailure::Codec(CodecError::Unencodable(
                postcard::Error::SerializeBufferFull
            ))
            .drop_rule(),
            PayloadDropRule::Unencodable
        );
    }
}
