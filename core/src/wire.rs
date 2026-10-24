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

/// A wire structure that failed to encode or decode, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    PastWireLimit { bytes: usize },
    TrailingBytes,
    Unencodable(postcard::Error),
    Malformed(postcard::Error),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Version;

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(PROTOCOL_VERSION)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match u8::deserialize(deserializer)? {
            PROTOCOL_VERSION => Ok(Self),
            _ => Err(de::Error::custom("unsupported wire version")),
        }
    }
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
#[serde(try_from = "String")]
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
        if url.len() > MAX_RELAY_URL_BYTES {
            return Err(CodecError::RelayUrlTooLong { bytes: url.len() });
        }
        Ok(Self(url))
    }
}

/// The relay URLs and direct socket addresses of an endpoint, within the caps of spec 3.6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UncheckedAddr")]
pub struct Addr {
    relays: Vec<RelayUrl>,
    direct: Vec<SocketAddr>,
}

#[derive(Deserialize)]
struct UncheckedAddr {
    relays: Vec<RelayUrl>,
    direct: Vec<SocketAddr>,
}

impl TryFrom<UncheckedAddr> for Addr {
    type Error = CodecError;

    fn try_from(unchecked: UncheckedAddr) -> Result<Self, CodecError> {
        if unchecked.relays.len() > MAX_RELAY_URLS_PER_ADDR
            || unchecked.direct.len() > MAX_DIRECT_ADDRS_PER_ADDR
        {
            return Err(CodecError::AddrOverCap);
        }
        Ok(Self {
            relays: unchecked.relays,
            direct: unchecked.direct,
        })
    }
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
    pub id: [u8; 16],
    pub to: [u8; 32],
    pub handle: String,
    pub attached: bool,
    pub sent_at: u64,
    pub body: String,
    pub mac: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub v: Version,
    pub nonce: [u8; 16],
    pub mac: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Introduce {
    pub v: Version,
    pub handle: Option<String>,
    pub public: Vec<u8>,
    pub attached: bool,
    pub addr: Addr,
    pub mac: [u8; 32],
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
    pub id: [u8; 16],
    pub accepted: bool,
    pub attached: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeAck {
    pub v: Version,
    pub nonce: [u8; 16],
    pub attached: bool,
    pub mac: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntroduceAck {
    pub v: Version,
    pub handle: Option<String>,
    pub public: Vec<u8>,
    pub attached: bool,
    pub addr: Addr,
    pub mac: [u8; 32],
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
    pub card: [u8; 32],
    pub handle: String,
    pub public: Vec<u8>,
    pub attached: bool,
    pub addr: Addr,
    pub seq: u64,
    pub mac: [u8; 32],
    pub sig: SignatureBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Departure {
    pub v: Version,
    pub card: [u8; 32],
    pub seq: u64,
    pub mac: [u8; 32],
    pub sig: SignatureBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unreachable {
    pub v: Version,
    pub card: [u8; 32],
    pub reporter: [u8; 32],
    pub ts: u64,
    pub mac: [u8; 32],
    pub sig: SignatureBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Infrastructure {
    pub v: Version,
    pub host: [u8; 32],
    pub relay: RelayUrl,
    pub addr: Addr,
    pub ts: u64,
    pub cert: SignatureBytes,
    pub mac: [u8; 32],
    pub sig: SignatureBytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostAddr {
    pub v: Version,
    pub id: [u8; 32],
    pub relay: RelayUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invitation {
    pub v: Version,
    pub network: String,
    pub secret: [u8; 32],
    pub owner: [u8; 32],
    pub hosts: Vec<HostAddr>,
}

/// A top-level wire structure: one that crosses the network as a whole.
pub trait Wire: Serialize + DeserializeOwned {}

impl Wire for Request {}
impl Wire for Response {}
impl Wire for Payload {}
impl Wire for HostAddr {}
impl Wire for Invitation {}

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
    if remainder.is_empty() {
        Ok(structure)
    } else {
        Err(CodecError::TrailingBytes)
    }
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
    const REJECTED_BY_VALIDATION: CodecError =
        CodecError::Malformed(postcard::Error::SerdeDeCustom);

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
            v: Version,
            card: [0xAB; 32],
            handle: "h".repeat(HANDLE_CAP_CHARACTERS),
            public: vec![0xCD; PUBLIC_MAPPING_CAP_BYTES],
            attached: true,
            addr: Addr::from_endpoint_report(
                (0..MAX_RELAY_URLS_PER_ADDR).map(|_| relay_of_bytes(MAX_RELAY_URL_BYTES)),
                (0..).take(MAX_DIRECT_ADDRS_PER_ADDR).map(global_v6),
            ),
            seq: u64::MAX,
            mac: [0xEF; 32],
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
            v: Version,
            nonce: [7; 16],
            mac: [9; 32],
        })
    }

    fn introduce_with_addr(addr: Addr) -> Request {
        Request::Introduce(Introduce {
            v: Version,
            handle: None,
            public: Vec::new(),
            attached: false,
            addr,
            mac: [3; 32],
        })
    }

    fn departure() -> Departure {
        Departure {
            v: Version,
            card: [0xAA; 32],
            seq: 0,
            mac: [0xBB; 32],
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
        let link_local = [0xfe80, 0xfebf].map(link_local_v6);
        let outside_the_prefix = [0xfec0, 0xfe7f].map(link_local_v6);
        let reported: Vec<SocketAddr> = link_local
            .into_iter()
            .chain(outside_the_prefix)
            .chain((0..).take(8).map(global_v6))
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

        assert_eq!(outcome, Err(REJECTED_BY_VALIDATION));
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

        assert_eq!(outcome, Err(REJECTED_BY_VALIDATION));
    }

    #[test]
    fn spec_3_6_receiver_drops_a_structure_with_a_relay_url_of_129_bytes() {
        let over_cap = Addr {
            relays: vec![RelayUrl(relay_of_bytes(MAX_RELAY_URL_BYTES + 1))],
            direct: Vec::new(),
        };

        let outcome = decode::<Request>(&encode(&introduce_with_addr(over_cap)).unwrap());

        assert_eq!(outcome, Err(REJECTED_BY_VALIDATION));
    }

    #[test]
    fn spec_3_13_host_addr_relay_of_129_bytes_does_not_decode() {
        let oversized = HostAddr {
            v: Version,
            id: [1; 32],
            relay: RelayUrl(relay_of_bytes(MAX_RELAY_URL_BYTES + 1)),
        };

        assert_eq!(
            decode::<HostAddr>(&encode(&oversized).unwrap()),
            Err(REJECTED_BY_VALIDATION)
        );
    }

    #[test]
    fn spec_2_wire_encoding_a_top_level_structure_opens_with_version_1_after_its_variant_index() {
        let encoded = encode(&probe_request()).unwrap();

        assert_eq!(encoded[..2], [1, PROTOCOL_VERSION]);
    }

    #[test]
    fn spec_2_wire_encoding_a_version_other_than_1_does_not_decode() {
        let mut encoded = encode(&probe_request()).unwrap();
        encoded[1] = 2;

        assert_eq!(decode::<Request>(&encoded), Err(REJECTED_BY_VALIDATION));
    }

    #[test]
    fn spec_2_wire_encoding_variants_are_indexed_in_the_order_the_spec_lists_them() {
        let presence_index = encode(&largest_presence()).unwrap()[0];
        let departure_index = encode(&Payload::Departure(departure())).unwrap()[0];
        let probe_index = encode(&probe_request()).unwrap()[0];

        assert_eq!([presence_index, departure_index, probe_index], [0, 1, 1]);
    }

    #[test]
    fn spec_2_wire_encoding_fixed_arrays_carry_no_length_prefix() {
        let variant_version_card_seq_mac_sig = 1 + 1 + 32 + 1 + 32 + SIGNATURE_BYTES;

        assert_eq!(
            encode(&Payload::Departure(departure())).unwrap().len(),
            variant_version_card_seq_mac_sig
        );
    }

    #[test]
    fn spec_3_14_a_structure_encoding_past_the_wire_limit_does_not_encode() {
        let oversized = Request::Message(Message {
            v: Version,
            id: [0; 16],
            to: [0; 32],
            handle: String::new(),
            attached: false,
            sent_at: 0,
            body: "b".repeat(WIRE_LIMIT_BYTES),
            mac: [0; 32],
        });

        assert!(matches!(
            encode(&oversized),
            Err(CodecError::PastWireLimit { .. })
        ));
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
            v: Version,
            id: [1; 16],
            to: [2; 32],
            handle: "h".repeat(HANDLE_CAP_CHARACTERS),
            attached: true,
            sent_at: u64::MAX,
            body: "b".repeat(MESSAGE_BODY_CAP_BYTES),
            mac: [4; 32],
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
            Self(case.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15))
        }

        fn next_word(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: usize) -> usize {
            usize::try_from(self.next_word())
                .unwrap_or(usize::MAX)
                .checked_rem(bound)
                .unwrap_or(0)
        }

        fn flag(&mut self) -> bool {
            self.below(2) == 1
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
                v: Version,
                id: self.array(),
                to: self.array(),
                handle: self.text(HANDLE_CAP_CHARACTERS),
                attached: self.flag(),
                sent_at: self.next_word(),
                body: self.text(200),
                mac: self.array(),
            }
        }

        fn probe(&mut self) -> Probe {
            Probe {
                v: Version,
                nonce: self.array(),
                mac: self.array(),
            }
        }

        fn introduce(&mut self) -> Introduce {
            Introduce {
                v: Version,
                handle: self.maybe_text(),
                public: self.blob(300),
                attached: self.flag(),
                addr: self.addr(),
                mac: self.array(),
            }
        }

        fn message_ack(&mut self) -> MessageAck {
            MessageAck {
                v: Version,
                id: self.array(),
                accepted: self.flag(),
                attached: self.flag(),
                reason: self.maybe_text(),
            }
        }

        fn probe_ack(&mut self) -> ProbeAck {
            ProbeAck {
                v: Version,
                nonce: self.array(),
                attached: self.flag(),
                mac: self.array(),
            }
        }

        fn introduce_ack(&mut self) -> IntroduceAck {
            IntroduceAck {
                v: Version,
                handle: self.maybe_text(),
                public: self.blob(300),
                attached: self.flag(),
                addr: self.addr(),
                mac: self.array(),
            }
        }

        fn presence(&mut self) -> Presence {
            Presence {
                v: Version,
                card: self.array(),
                handle: self.text(HANDLE_CAP_CHARACTERS),
                public: self.blob(300),
                attached: self.flag(),
                addr: self.addr(),
                seq: self.next_word(),
                mac: self.array(),
                sig: self.signature(),
            }
        }

        fn departure(&mut self) -> Departure {
            Departure {
                v: Version,
                card: self.array(),
                seq: self.next_word(),
                mac: self.array(),
                sig: self.signature(),
            }
        }

        fn unreachable(&mut self) -> Unreachable {
            Unreachable {
                v: Version,
                card: self.array(),
                reporter: self.array(),
                ts: self.next_word(),
                mac: self.array(),
                sig: self.signature(),
            }
        }

        fn infrastructure(&mut self) -> Infrastructure {
            Infrastructure {
                v: Version,
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
                v: Version,
                id: self.array(),
                relay: self.relay(),
            }
        }

        fn invitation(&mut self) -> Invitation {
            let host_count = self.below(4).saturating_add(1);
            Invitation {
                v: Version,
                network: self.text(40),
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
