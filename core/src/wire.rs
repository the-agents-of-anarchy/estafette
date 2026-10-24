//! The postcard wire structures of spec 3.6, 3.7 and 3.13, with their encoding under the wire limit.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use serde::de::{self, DeserializeOwned, SeqAccess, Visitor};
use serde::ser::SerializeTuple;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const WIRE_LIMIT_BYTES: usize = 1_114_112;
const PROTOCOL_VERSION: u8 = 1;
const MAX_RELAY_URLS_PER_ADDR: usize = 4;
const MAX_DIRECT_ADDRS_PER_ADDR: usize = 8;
const MAX_RELAY_URL_BYTES: usize = 128;
const SIGNATURE_BYTES: usize = 64;
const MAC_BYTES: usize = 32;
const NONCE_BYTES: usize = 16;
const MESSAGE_ID_BYTES: usize = 16;
const PUBLIC_KEY_BYTES: usize = 32;
const SECRET_BYTES: usize = 32;

pub type Mac = [u8; MAC_BYTES];
pub type Nonce = [u8; NONCE_BYTES];
pub type MessageId = [u8; MESSAGE_ID_BYTES];
pub type PublicKey = [u8; PUBLIC_KEY_BYTES];
pub type Secret = [u8; SECRET_BYTES];

/// A wire structure that failed to encode or decode, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    PastWireLimit { bytes: usize },
    TrailingBytes,
    Unencodable(postcard::Error),
    Malformed(postcard::Error),
    UnsupportedVersion { found: u8 },
    RelayUrlTooLong { bytes: usize },
    AddrOverCap,
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PastWireLimit { bytes } => write!(
                formatter,
                "a wire structure of {bytes} bytes is past the wire limit of {WIRE_LIMIT_BYTES} bytes"
            ),
            Self::TrailingBytes => {
                formatter.write_str("bytes follow the end of the wire structure")
            }
            Self::Unencodable(source) => {
                write!(formatter, "the structure does not encode: {source}")
            }
            Self::Malformed(source) => {
                write!(formatter, "the bytes are not a wire structure: {source}")
            }
            Self::UnsupportedVersion { found } => write!(
                formatter,
                "wire version {found} is not the supported version {PROTOCOL_VERSION}"
            ),
            Self::RelayUrlTooLong { bytes } => write!(
                formatter,
                "a relay URL of {bytes} bytes is past the cap of {MAX_RELAY_URL_BYTES} bytes"
            ),
            Self::AddrOverCap => write!(
                formatter,
                "an address carries more than {MAX_RELAY_URLS_PER_ADDR} relay URLs or {MAX_DIRECT_ADDRS_PER_ADDR} direct addresses"
            ),
        }
    }
}

impl std::error::Error for CodecError {}

/// The version field every top-level structure opens with; only the value 1 exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Version(u8);

impl Version {
    pub const CURRENT: Self = Self(PROTOCOL_VERSION);
}

/// A 64-byte signature, encoded as its bytes with no length prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignatureBytes(pub [u8; SIGNATURE_BYTES]);

impl Serialize for SignatureBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut tuple = serializer.serialize_tuple(SIGNATURE_BYTES)?;
        for byte in &self.0 {
            tuple.serialize_element(byte)?;
        }
        tuple.end()
    }
}

struct SignatureVisitor;

impl<'de> Visitor<'de> for SignatureVisitor {
    type Value = SignatureBytes;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{SIGNATURE_BYTES} signature bytes")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<SignatureBytes, A::Error> {
        let mut bytes = [0; SIGNATURE_BYTES];
        for byte in &mut bytes {
            *byte = sequence
                .next_element()?
                .ok_or_else(|| de::Error::custom("signature shorter than its fixed length"))?;
        }
        Ok(SignatureBytes(bytes))
    }
}

impl<'de> Deserialize<'de> for SignatureBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_tuple(SIGNATURE_BYTES, SignatureVisitor)
    }
}

/// A relay URL of at most 128 bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayUrl(String);

impl RelayUrl {
    /// The URL text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RelayUrl {
    type Error = CodecError;

    fn try_from(url: String) -> Result<Self, CodecError> {
        let relay_url = Self(url);
        relay_url.validate()?;
        Ok(relay_url)
    }
}

/// The relay URLs and direct socket addresses of an endpoint, within the caps of spec 3.6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addr {
    relays: Vec<RelayUrl>,
    direct: Vec<SocketAddr>,
}

const fn is_link_local_v6(address: &SocketAddr) -> bool {
    match address.ip() {
        IpAddr::V4(_) => false,
        IpAddr::V6(ip) => ip.is_unicast_link_local(),
    }
}

impl Addr {
    /// Carries the first reported relay URLs and direct addresses that fit the caps, leaving out overlong relay URLs and `fe80::/10` addresses.
    pub fn from_endpoint_report(
        relays: impl IntoIterator<Item = String>,
        direct: impl IntoIterator<Item = SocketAddr>,
    ) -> Self {
        Self {
            relays: relays
                .into_iter()
                .filter_map(|url| RelayUrl::try_from(url).ok())
                .take(MAX_RELAY_URLS_PER_ADDR)
                .collect(),
            direct: direct
                .into_iter()
                .filter(|address| !is_link_local_v6(address))
                .take(MAX_DIRECT_ADDRS_PER_ADDR)
                .collect(),
        }
    }

    /// The relay URLs, at most 4.
    #[must_use]
    pub fn relays(&self) -> &[RelayUrl] {
        &self.relays
    }

    /// The direct socket addresses, at most 8.
    #[must_use]
    pub fn direct(&self) -> &[SocketAddr] {
        &self.direct
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Message(Message),
    Probe(Probe),
    Introduce(Introduce),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub v: Version,
    pub id: MessageId,
    pub to: PublicKey,
    pub handle: String,
    pub attached: bool,
    pub sent_at: u64,
    pub body: String,
    pub mac: Mac,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub v: Version,
    pub nonce: Nonce,
    pub mac: Mac,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Introduce {
    pub v: Version,
    pub handle: Option<String>,
    pub public: Vec<u8>,
    pub attached: bool,
    pub addr: Addr,
    pub mac: Mac,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    MessageAck(MessageAck),
    ProbeAck(ProbeAck),
    IntroduceAck(IntroduceAck),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAck {
    pub v: Version,
    pub id: MessageId,
    pub accepted: bool,
    pub attached: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeAck {
    pub v: Version,
    pub nonce: Nonce,
    pub attached: bool,
    pub mac: Mac,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntroduceAck {
    pub v: Version,
    pub handle: Option<String>,
    pub public: Vec<u8>,
    pub attached: bool,
    pub addr: Addr,
    pub mac: Mac,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Payload {
    Presence(Presence),
    Departure(Departure),
    Unreachable(Unreachable),
    Infrastructure(Infrastructure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub v: Version,
    pub card: PublicKey,
    pub handle: String,
    pub public: Vec<u8>,
    pub attached: bool,
    pub addr: Addr,
    pub seq: u64,
    pub mac: Mac,
    pub sig: SignatureBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Departure {
    pub v: Version,
    pub card: PublicKey,
    pub seq: u64,
    pub mac: Mac,
    pub sig: SignatureBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unreachable {
    pub v: Version,
    pub card: PublicKey,
    pub reporter: PublicKey,
    pub ts: u64,
    pub mac: Mac,
    pub sig: SignatureBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Infrastructure {
    pub v: Version,
    pub host: PublicKey,
    pub relay: RelayUrl,
    pub addr: Addr,
    pub ts: u64,
    pub cert: SignatureBytes,
    pub mac: Mac,
    pub sig: SignatureBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostAddr {
    pub v: Version,
    pub id: PublicKey,
    pub relay: RelayUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invitation {
    pub v: Version,
    pub network: String,
    pub secret: Secret,
    pub owner: PublicKey,
    pub hosts: Vec<HostAddr>,
}

trait Validate {
    fn validate(&self) -> Result<(), CodecError>;
}

impl Validate for Version {
    fn validate(&self) -> Result<(), CodecError> {
        if *self == Self::CURRENT {
            Ok(())
        } else {
            Err(CodecError::UnsupportedVersion { found: self.0 })
        }
    }
}

impl Validate for RelayUrl {
    fn validate(&self) -> Result<(), CodecError> {
        if self.0.len() > MAX_RELAY_URL_BYTES {
            Err(CodecError::RelayUrlTooLong {
                bytes: self.0.len(),
            })
        } else {
            Ok(())
        }
    }
}

impl Validate for Addr {
    fn validate(&self) -> Result<(), CodecError> {
        if self.relays.len() > MAX_RELAY_URLS_PER_ADDR
            || self.direct.len() > MAX_DIRECT_ADDRS_PER_ADDR
        {
            return Err(CodecError::AddrOverCap);
        }
        self.relays.iter().try_for_each(Validate::validate)
    }
}

impl Validate for Request {
    fn validate(&self) -> Result<(), CodecError> {
        match self {
            Self::Message(message) => message.v.validate(),
            Self::Probe(probe) => probe.v.validate(),
            Self::Introduce(introduce) => {
                introduce.v.validate()?;
                introduce.addr.validate()
            }
        }
    }
}

impl Validate for Response {
    fn validate(&self) -> Result<(), CodecError> {
        match self {
            Self::MessageAck(ack) => ack.v.validate(),
            Self::ProbeAck(ack) => ack.v.validate(),
            Self::IntroduceAck(ack) => {
                ack.v.validate()?;
                ack.addr.validate()
            }
        }
    }
}

impl Validate for Payload {
    fn validate(&self) -> Result<(), CodecError> {
        match self {
            Self::Presence(presence) => {
                presence.v.validate()?;
                presence.addr.validate()
            }
            Self::Departure(departure) => departure.v.validate(),
            Self::Unreachable(unreachable) => unreachable.v.validate(),
            Self::Infrastructure(infrastructure) => {
                infrastructure.v.validate()?;
                infrastructure.relay.validate()?;
                infrastructure.addr.validate()
            }
        }
    }
}

impl Validate for HostAddr {
    fn validate(&self) -> Result<(), CodecError> {
        self.v.validate()?;
        self.relay.validate()
    }
}

impl Validate for Invitation {
    fn validate(&self) -> Result<(), CodecError> {
        self.v.validate()?;
        self.hosts.iter().try_for_each(|host| host.relay.validate())
    }
}

/// A top-level wire structure: one that crosses the network as a whole.
pub trait Wire: Serialize + DeserializeOwned {
    /// Refuses a decoded structure whose version, relay URL or address breaks the wire rules.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`].
    fn refuse_if_invalid(&self) -> Result<(), CodecError>;
}

macro_rules! wire_structures {
    ($($structure:ty),+) => {
        $(
            impl Wire for $structure {
                fn refuse_if_invalid(&self) -> Result<(), CodecError> {
                    Validate::validate(self)
                }
            }
        )+
    };
}

wire_structures!(Request, Response, Payload, HostAddr, Invitation);

/// Encodes a wire structure as postcard bytes, refusing one past the wire limit.
///
/// # Errors
///
/// Returns [`CodecError`].
pub fn encode<T: Wire>(structure: &T) -> Result<Vec<u8>, CodecError> {
    let bytes = postcard::to_allocvec(structure).map_err(CodecError::Unencodable)?;
    ensure_within_wire_limit(bytes.len())?;
    Ok(bytes)
}

/// Decodes a wire structure from exactly the given bytes, refusing bytes past the wire limit.
///
/// # Errors
///
/// Returns [`CodecError`].
pub fn decode<T: Wire>(bytes: &[u8]) -> Result<T, CodecError> {
    ensure_within_wire_limit(bytes.len())?;
    let (structure, remainder) = postcard::take_from_bytes(bytes).map_err(CodecError::Malformed)?;
    if !remainder.is_empty() {
        return Err(CodecError::TrailingBytes);
    }
    T::refuse_if_invalid(&structure)?;
    Ok(structure)
}

const fn ensure_within_wire_limit(bytes: usize) -> Result<(), CodecError> {
    if bytes > WIRE_LIMIT_BYTES {
        return Err(CodecError::PastWireLimit { bytes });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv6Addr};

    use super::*;

    const GOSSIP_FRAME_BYTES: usize = 4096;
    const GOSSIP_LAYER_OVERHEAD_BYTES: usize = 38;
    const SPEC_LARGEST_PRESENCE_BYTES: usize = 3951;
    const PUBLIC_MAPPING_CAP_BYTES: usize = 3072;
    const HANDLE_CAP_CHARACTERS: usize = 53;
    const HIGH_PORT: u16 = 65535;
    const MESSAGE_BODY_CAP_BYTES: usize = 1_048_576;
    const ROUND_TRIP_CASES: u64 = 200;
    const LONG_BODY_EXTRA_LENGTH_PREFIX_BYTES: usize = 2;
    const UNSUPPORTED_VERSION: u8 = 2;
    const REQUEST_PROBE_INDEX: u8 = 1;
    const PAYLOAD_PRESENCE_INDEX: u8 = 0;
    const PAYLOAD_DEPARTURE_INDEX: u8 = 1;
    const VARIANT_TAG_BYTES: usize = 1;
    const VERSION_BYTES: usize = 1;
    const ZERO_VARINT_BYTES: usize = 1;
    const DEPARTURE_WITH_ZERO_SEQ_ENCODED_BYTES: usize = VARIANT_TAG_BYTES
        + VERSION_BYTES
        + PUBLIC_KEY_BYTES
        + ZERO_VARINT_BYTES
        + MAC_BYTES
        + SIGNATURE_BYTES;
    const SEED_SPREAD_MULTIPLIER: u64 = 0x9E37_79B9_7F4A_7C15;
    const XORSHIFT_FIRST_LEFT_SHIFT: u32 = 13;
    const XORSHIFT_RIGHT_SHIFT: u32 = 7;
    const XORSHIFT_SECOND_LEFT_SHIFT: u32 = 17;
    const FLAG_VALUES: usize = 2;
    const ROUND_TRIP_BODY_CAP_CHARACTERS: usize = 200;
    const ROUND_TRIP_PUBLIC_CAP_BYTES: usize = 300;
    const ROUND_TRIP_HOSTS_CAP: usize = 4;
    const ROUND_TRIP_NETWORK_NAME_CAP_CHARACTERS: usize = 40;
    const LINK_LOCAL_FIRST_SEGMENT: u16 = 0xfe80;
    const LINK_LOCAL_LAST_SEGMENT: u16 = 0xfebf;
    const ABOVE_LINK_LOCAL_SEGMENT: u16 = 0xfec0;
    const BELOW_LINK_LOCAL_SEGMENT: u16 = 0xfe7f;

    fn relay_of_bytes(length: usize) -> String {
        format!("{:x<length$}", "https://")
    }

    fn global_v6(index: u16) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(
                0x2001, 0xdb8, index, index, index, index, index, index,
            )),
            HIGH_PORT,
        )
    }

    fn largest_presence() -> Payload {
        Payload::Presence(Presence {
            v: Version::CURRENT,
            card: [0xAB; PUBLIC_KEY_BYTES],
            handle: "h".repeat(HANDLE_CAP_CHARACTERS),
            public: vec![0xCD; PUBLIC_MAPPING_CAP_BYTES],
            attached: true,
            addr: Addr::from_endpoint_report(
                (0..MAX_RELAY_URLS_PER_ADDR).map(|_| relay_of_bytes(MAX_RELAY_URL_BYTES)),
                (0..).take(MAX_DIRECT_ADDRS_PER_ADDR).map(global_v6),
            ),
            seq: u64::MAX,
            mac: [0xEF; MAC_BYTES],
            sig: SignatureBytes([0x12; SIGNATURE_BYTES]),
        })
    }

    #[test]
    fn test_17_largest_presence_encodes_to_3951_bytes_within_the_gossip_frame() {
        let encoded = encode(&largest_presence()).unwrap();

        assert_eq!(encoded.len(), SPEC_LARGEST_PRESENCE_BYTES);
        assert!(encoded.len() <= GOSSIP_FRAME_BYTES - GOSSIP_LAYER_OVERHEAD_BYTES);
    }

    fn link_local_v6(first_segment: u16) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(first_segment, 0, 0, 0, 0, 0, 0, 1)),
            HIGH_PORT,
        )
    }

    fn probe_request() -> Request {
        Request::Probe(Probe {
            v: Version::CURRENT,
            nonce: [7; NONCE_BYTES],
            mac: [9; MAC_BYTES],
        })
    }

    fn introduce_with_addr(addr: Addr) -> Request {
        Request::Introduce(Introduce {
            v: Version::CURRENT,
            handle: None,
            public: Vec::new(),
            attached: false,
            addr,
            mac: [3; MAC_BYTES],
        })
    }

    fn departure() -> Departure {
        Departure {
            v: Version::CURRENT,
            card: [0xAA; PUBLIC_KEY_BYTES],
            seq: 0,
            mac: [0xBB; MAC_BYTES],
            sig: SignatureBytes([0xCC; SIGNATURE_BYTES]),
        }
    }

    #[test]
    fn spec_3_6_sender_keeps_the_first_four_fitting_relays_and_never_an_overlong_one() {
        let reported = [
            relay_of_bytes(MAX_RELAY_URL_BYTES),
            relay_of_bytes(MAX_RELAY_URL_BYTES + 1),
            relay_of_bytes(10),
            relay_of_bytes(20),
            relay_of_bytes(30),
            relay_of_bytes(40),
        ];

        let addr = Addr::from_endpoint_report(reported.clone(), []);

        let kept: Vec<&str> = addr.relays().iter().map(RelayUrl::as_str).collect();
        assert_eq!(
            kept,
            [
                reported[0].as_str(),
                reported[2].as_str(),
                reported[3].as_str(),
                reported[4].as_str()
            ]
        );
    }

    #[test]
    fn spec_3_6_sender_leaves_link_local_out_and_keeps_the_first_eight_others() {
        let link_local = [LINK_LOCAL_FIRST_SEGMENT, LINK_LOCAL_LAST_SEGMENT].map(link_local_v6);
        let outside_the_prefix =
            [ABOVE_LINK_LOCAL_SEGMENT, BELOW_LINK_LOCAL_SEGMENT].map(link_local_v6);
        let reported: Vec<SocketAddr> = link_local
            .into_iter()
            .chain(outside_the_prefix)
            .chain((0..).take(MAX_DIRECT_ADDRS_PER_ADDR).map(global_v6))
            .collect();

        let addr = Addr::from_endpoint_report([], reported.clone());

        let expected: Vec<SocketAddr> = reported
            .into_iter()
            .skip(link_local.len())
            .take(MAX_DIRECT_ADDRS_PER_ADDR)
            .collect();
        assert_eq!(addr.direct(), expected);
    }

    #[test]
    fn spec_3_6_receiver_drops_a_structure_whose_addr_has_five_relay_urls() {
        let over_cap = Addr {
            relays: (0..=MAX_RELAY_URLS_PER_ADDR)
                .map(|_| RelayUrl(relay_of_bytes(8)))
                .collect(),
            direct: Vec::new(),
        };

        let outcome = decode::<Request>(&encode(&introduce_with_addr(over_cap)).unwrap());

        assert_eq!(outcome, Err(CodecError::AddrOverCap));
    }

    #[test]
    fn spec_3_6_receiver_drops_a_structure_whose_addr_has_nine_direct_addresses() {
        let over_cap = Addr {
            relays: Vec::new(),
            direct: (0..)
                .take(MAX_DIRECT_ADDRS_PER_ADDR + 1)
                .map(global_v6)
                .collect(),
        };

        let outcome = decode::<Request>(&encode(&introduce_with_addr(over_cap)).unwrap());

        assert_eq!(outcome, Err(CodecError::AddrOverCap));
    }

    #[test]
    fn spec_3_6_receiver_drops_a_structure_with_a_relay_url_of_129_bytes() {
        let over_cap = Addr {
            relays: vec![RelayUrl(relay_of_bytes(MAX_RELAY_URL_BYTES + 1))],
            direct: Vec::new(),
        };

        let outcome = decode::<Request>(&encode(&introduce_with_addr(over_cap)).unwrap());

        assert_eq!(
            outcome,
            Err(CodecError::RelayUrlTooLong {
                bytes: MAX_RELAY_URL_BYTES + 1
            })
        );
    }

    #[test]
    fn spec_3_13_host_addr_relay_of_129_bytes_does_not_decode() {
        let oversized = HostAddr {
            v: Version::CURRENT,
            id: [1; PUBLIC_KEY_BYTES],
            relay: RelayUrl(relay_of_bytes(MAX_RELAY_URL_BYTES + 1)),
        };

        assert_eq!(
            decode::<HostAddr>(&encode(&oversized).unwrap()),
            Err(CodecError::RelayUrlTooLong {
                bytes: MAX_RELAY_URL_BYTES + 1
            })
        );
    }

    #[test]
    fn spec_2_wire_encoding_a_top_level_structure_opens_with_version_1_after_its_variant_index() {
        let encoded = encode(&probe_request()).unwrap();

        assert_eq!(encoded[..2], [REQUEST_PROBE_INDEX, PROTOCOL_VERSION]);
    }

    #[test]
    fn spec_2_wire_encoding_a_version_other_than_1_does_not_decode() {
        let mut encoded = encode(&probe_request()).unwrap();
        encoded[1] = UNSUPPORTED_VERSION;

        assert_eq!(
            decode::<Request>(&encoded),
            Err(CodecError::UnsupportedVersion {
                found: UNSUPPORTED_VERSION
            })
        );
    }

    #[test]
    fn spec_2_wire_encoding_variants_are_indexed_in_the_order_the_spec_lists_them() {
        let presence_index = encode(&largest_presence()).unwrap()[0];
        let departure_index = encode(&Payload::Departure(departure())).unwrap()[0];
        let probe_index = encode(&probe_request()).unwrap()[0];

        assert_eq!(
            [presence_index, departure_index, probe_index],
            [
                PAYLOAD_PRESENCE_INDEX,
                PAYLOAD_DEPARTURE_INDEX,
                REQUEST_PROBE_INDEX
            ]
        );
    }

    #[test]
    fn spec_2_wire_encoding_fixed_arrays_carry_no_length_prefix() {
        let variant_version_card_seq_mac_sig = DEPARTURE_WITH_ZERO_SEQ_ENCODED_BYTES;
        assert_eq!(
            encode(&Payload::Departure(departure())).unwrap().len(),
            variant_version_card_seq_mac_sig
        );
    }

    #[test]
    fn spec_3_14_a_structure_encoding_past_the_wire_limit_does_not_encode() {
        let oversized = Request::Message(Message {
            v: Version::CURRENT,
            id: [0; MESSAGE_ID_BYTES],
            to: [0; PUBLIC_KEY_BYTES],
            handle: String::new(),
            attached: false,
            sent_at: 0,
            body: "b".repeat(WIRE_LIMIT_BYTES),
            mac: [0; MAC_BYTES],
        });

        assert!(matches!(
            encode(&oversized),
            Err(CodecError::PastWireLimit { .. })
        ));
    }

    fn message_with_body_of(body_bytes: usize) -> Request {
        Request::Message(Message {
            v: Version::CURRENT,
            id: [0; MESSAGE_ID_BYTES],
            to: [0; PUBLIC_KEY_BYTES],
            handle: String::new(),
            attached: false,
            sent_at: 0,
            body: "b".repeat(body_bytes),
            mac: [0; MAC_BYTES],
        })
    }

    fn message_encoding_to_exactly_the_wire_limit(extra_body_bytes: usize) -> Request {
        let empty_body_encoding_bytes = encode(&message_with_body_of(0)).unwrap().len();
        message_with_body_of(
            WIRE_LIMIT_BYTES
                .saturating_sub(empty_body_encoding_bytes)
                .saturating_sub(LONG_BODY_EXTRA_LENGTH_PREFIX_BYTES)
                .saturating_add(extra_body_bytes),
        )
    }

    #[test]
    fn spec_3_14_a_structure_of_exactly_the_wire_limit_encodes_and_decodes() {
        let at_the_limit = message_encoding_to_exactly_the_wire_limit(0);

        let encoded = encode(&at_the_limit).unwrap();

        assert_eq!(encoded.len(), WIRE_LIMIT_BYTES);
        assert_eq!(decode::<Request>(&encoded), Ok(at_the_limit));
    }

    #[test]
    fn spec_3_14_a_structure_one_byte_past_the_wire_limit_does_not_encode() {
        let past_the_limit = message_encoding_to_exactly_the_wire_limit(1);

        assert_eq!(
            encode(&past_the_limit),
            Err(CodecError::PastWireLimit {
                bytes: WIRE_LIMIT_BYTES.saturating_add(1)
            })
        );
    }

    #[test]
    fn spec_3_14_bytes_past_the_wire_limit_do_not_decode() {
        let past_the_limit = vec![0; WIRE_LIMIT_BYTES + 1];

        assert_eq!(
            decode::<Request>(&past_the_limit),
            Err(CodecError::PastWireLimit {
                bytes: WIRE_LIMIT_BYTES + 1
            })
        );
    }

    #[test]
    fn spec_3_14_a_message_with_the_largest_body_round_trips_within_the_wire_limit() {
        let message = Request::Message(Message {
            v: Version::CURRENT,
            id: [1; MESSAGE_ID_BYTES],
            to: [2; PUBLIC_KEY_BYTES],
            handle: "h".repeat(HANDLE_CAP_CHARACTERS),
            attached: true,
            sent_at: u64::MAX,
            body: "b".repeat(MESSAGE_BODY_CAP_BYTES),
            mac: [4; MAC_BYTES],
        });

        assert_eq!(decode::<Request>(&encode(&message).unwrap()), Ok(message));
    }

    #[test]
    fn spec_2_wire_encoding_bytes_after_the_structure_do_not_decode() {
        let mut encoded = encode(&probe_request()).unwrap();
        encoded.push(0);

        assert_eq!(decode::<Request>(&encoded), Err(CodecError::TrailingBytes));
    }

    #[test]
    fn spec_2_wire_encoding_truncated_bytes_do_not_decode() {
        let encoded = encode(&largest_presence()).unwrap();

        assert_eq!(
            decode::<Payload>(&encoded[..encoded.len() - 1]),
            Err(CodecError::Malformed(
                postcard::Error::DeserializeUnexpectedEnd
            ))
        );
    }

    struct Seeded(u64);

    impl Seeded {
        fn for_case(case: u64) -> Self {
            Self(case.wrapping_add(1).wrapping_mul(SEED_SPREAD_MULTIPLIER))
        }

        fn next_word(&mut self) -> u64 {
            self.0 ^= self.0 << XORSHIFT_FIRST_LEFT_SHIFT;
            self.0 ^= self.0 >> XORSHIFT_RIGHT_SHIFT;
            self.0 ^= self.0 << XORSHIFT_SECOND_LEFT_SHIFT;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next_word())
                .unwrap_or(usize::MAX)
                .checked_rem(bound)
                .unwrap_or(0)
        }

        fn flag(&mut self) -> bool {
            self.below(FLAG_VALUES) == 1
        }

        fn array<const N: usize>(&mut self) -> [u8; N] {
            std::array::from_fn(|_| self.next_word().to_be_bytes()[0])
        }

        fn signature(&mut self) -> SignatureBytes {
            SignatureBytes(self.array())
        }

        fn text(&mut self, longest: usize) -> String {
            const GLYPHS: [char; 5] = ['a', 'Z', 'é', '語', '🦀'];
            let length = self.below(longest.saturating_add(1));
            (0..length)
                .map(|_| GLYPHS[self.below(GLYPHS.len())])
                .collect()
        }

        fn maybe_text(&mut self) -> Option<String> {
            self.flag().then(|| self.text(HANDLE_CAP_CHARACTERS))
        }

        fn blob(&mut self, longest: usize) -> Vec<u8> {
            let length = self.below(longest.saturating_add(1));
            (0..length).map(|_| self.array::<1>()[0]).collect()
        }

        fn relay(&mut self) -> RelayUrl {
            RelayUrl(relay_of_bytes(self.below(MAX_RELAY_URL_BYTES + 1)))
        }

        fn socket_addr(&mut self) -> SocketAddr {
            let port = u16::from_be_bytes(self.array());
            if self.flag() {
                SocketAddr::new(IpAddr::from(self.array::<4>()), port)
            } else {
                SocketAddr::new(IpAddr::from(self.array::<16>()), port)
            }
        }

        fn addr(&mut self) -> Addr {
            let relay_count = self.below(MAX_RELAY_URLS_PER_ADDR + 1);
            let direct_count = self.below(MAX_DIRECT_ADDRS_PER_ADDR + 1);
            Addr {
                relays: (0..relay_count).map(|_| self.relay()).collect(),
                direct: (0..direct_count).map(|_| self.socket_addr()).collect(),
            }
        }

        fn message(&mut self) -> Message {
            Message {
                v: Version::CURRENT,
                id: self.array(),
                to: self.array(),
                handle: self.text(HANDLE_CAP_CHARACTERS),
                attached: self.flag(),
                sent_at: self.next_word(),
                body: self.text(ROUND_TRIP_BODY_CAP_CHARACTERS),
                mac: self.array(),
            }
        }

        fn probe(&mut self) -> Probe {
            Probe {
                v: Version::CURRENT,
                nonce: self.array(),
                mac: self.array(),
            }
        }

        fn introduce(&mut self) -> Introduce {
            Introduce {
                v: Version::CURRENT,
                handle: self.maybe_text(),
                public: self.blob(ROUND_TRIP_PUBLIC_CAP_BYTES),
                attached: self.flag(),
                addr: self.addr(),
                mac: self.array(),
            }
        }

        fn message_ack(&mut self) -> MessageAck {
            MessageAck {
                v: Version::CURRENT,
                id: self.array(),
                accepted: self.flag(),
                attached: self.flag(),
                reason: self.maybe_text(),
            }
        }

        fn probe_ack(&mut self) -> ProbeAck {
            ProbeAck {
                v: Version::CURRENT,
                nonce: self.array(),
                attached: self.flag(),
                mac: self.array(),
            }
        }

        fn introduce_ack(&mut self) -> IntroduceAck {
            IntroduceAck {
                v: Version::CURRENT,
                handle: self.maybe_text(),
                public: self.blob(ROUND_TRIP_PUBLIC_CAP_BYTES),
                attached: self.flag(),
                addr: self.addr(),
                mac: self.array(),
            }
        }

        fn presence(&mut self) -> Presence {
            Presence {
                v: Version::CURRENT,
                card: self.array(),
                handle: self.text(HANDLE_CAP_CHARACTERS),
                public: self.blob(ROUND_TRIP_PUBLIC_CAP_BYTES),
                attached: self.flag(),
                addr: self.addr(),
                seq: self.next_word(),
                mac: self.array(),
                sig: self.signature(),
            }
        }

        fn departure(&mut self) -> Departure {
            Departure {
                v: Version::CURRENT,
                card: self.array(),
                seq: self.next_word(),
                mac: self.array(),
                sig: self.signature(),
            }
        }

        fn unreachable(&mut self) -> Unreachable {
            Unreachable {
                v: Version::CURRENT,
                card: self.array(),
                reporter: self.array(),
                ts: self.next_word(),
                mac: self.array(),
                sig: self.signature(),
            }
        }

        fn infrastructure(&mut self) -> Infrastructure {
            Infrastructure {
                v: Version::CURRENT,
                host: self.array(),
                relay: self.relay(),
                addr: self.addr(),
                ts: self.next_word(),
                cert: self.signature(),
                mac: self.array(),
                sig: self.signature(),
            }
        }

        fn host_addr(&mut self) -> HostAddr {
            HostAddr {
                v: Version::CURRENT,
                id: self.array(),
                relay: self.relay(),
            }
        }

        fn invitation(&mut self) -> Invitation {
            let host_count = self.below(ROUND_TRIP_HOSTS_CAP).saturating_add(1);
            Invitation {
                v: Version::CURRENT,
                network: self.text(ROUND_TRIP_NETWORK_NAME_CAP_CHARACTERS),
                secret: self.array(),
                owner: self.array(),
                hosts: (0..host_count).map(|_| self.host_addr()).collect(),
            }
        }
    }

    fn assert_every_case_round_trips<T: Wire + PartialEq + fmt::Debug>(
        generate: impl Fn(&mut Seeded) -> T,
    ) {
        for case in 0..ROUND_TRIP_CASES {
            let structure = generate(&mut Seeded::for_case(case));
            assert_eq!(decode::<T>(&encode(&structure).unwrap()), Ok(structure));
        }
    }

    #[test]
    fn spec_3_6_message_round_trips() {
        assert_every_case_round_trips(|seed| Request::Message(seed.message()));
    }

    #[test]
    fn spec_3_6_probe_round_trips() {
        assert_every_case_round_trips(|seed| Request::Probe(seed.probe()));
    }

    #[test]
    fn spec_3_6_introduce_round_trips() {
        assert_every_case_round_trips(|seed| Request::Introduce(seed.introduce()));
    }

    #[test]
    fn spec_3_6_message_ack_round_trips() {
        assert_every_case_round_trips(|seed| Response::MessageAck(seed.message_ack()));
    }

    #[test]
    fn spec_3_6_probe_ack_round_trips() {
        assert_every_case_round_trips(|seed| Response::ProbeAck(seed.probe_ack()));
    }

    #[test]
    fn spec_3_6_introduce_ack_round_trips() {
        assert_every_case_round_trips(|seed| Response::IntroduceAck(seed.introduce_ack()));
    }

    #[test]
    fn spec_3_7_presence_round_trips() {
        assert_every_case_round_trips(|seed| Payload::Presence(seed.presence()));
    }

    #[test]
    fn spec_3_7_departure_round_trips() {
        assert_every_case_round_trips(|seed| Payload::Departure(seed.departure()));
    }

    #[test]
    fn spec_3_7_unreachable_round_trips() {
        assert_every_case_round_trips(|seed| Payload::Unreachable(seed.unreachable()));
    }

    #[test]
    fn spec_3_7_infrastructure_round_trips() {
        assert_every_case_round_trips(|seed| Payload::Infrastructure(seed.infrastructure()));
    }

    #[test]
    fn spec_3_13_host_addr_round_trips() {
        assert_every_case_round_trips(Seeded::host_addr);
    }

    #[test]
    fn spec_3_13_invitation_round_trips() {
        assert_every_case_round_trips(Seeded::invitation);
    }
}
