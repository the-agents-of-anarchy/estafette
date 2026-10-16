use serde::{Deserialize, Serialize};

/// 64-byte array as a serde tuple (postcard: no length prefix, 64 raw bytes).
pub mod arr64 {
    use serde::de::{Error, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        let mut t = s.serialize_tuple(64)?;
        for b in v {
            t.serialize_element(b)?;
        }
        t.end()
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = [u8; 64];
            fn expecting(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
                f.write_str("64 bytes")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<[u8; 64], A::Error> {
                let mut out = [0u8; 64];
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = a.next_element()?.ok_or_else(|| A::Error::invalid_length(i, &self))?;
                }
                Ok(out)
            }
        }
        d.deserialize_tuple(64, V)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Claim {
    pub v: u8,
    pub card: [u8; 32],
    pub node: [u8; 32],
    pub ts: u64,
    pub handle: String,
    #[serde(with = "arr64")]
    pub sig: [u8; 64],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Presence {
    pub v: u8,
    pub claim: Claim,
    pub handle: String,
    pub public: Vec<u8>,
    pub attached: bool,
    pub ts: u64,
    pub mac: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Departure {
    pub v: u8,
    pub card: [u8; 32],
    pub node: [u8; 32],
    pub ts: u64,
    pub mac: [u8; 32],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Unreachable {
    pub v: u8,
    pub card: [u8; 32],
    pub node: [u8; 32],
    pub reporter: Claim,
    pub ts: u64,
    pub mac: [u8; 32],
}

/// Spec: `Payload = Presence | Departure | Unreachable`. The spec gives no tag
/// or wrapper; this enum is the minimal postcard-encodable union (1-byte tag).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Payload {
    Presence(Presence),
    Departure(Departure),
    Unreachable(Unreachable),
}

pub const MAX_HANDLE: &str = "abcdefghijklmnopqrstuvwxyz012345"; // 32 chars, matches ^[a-z][a-z0-9-]{0,31}$

pub fn effective_max() -> String {
    format!("{MAX_HANDLE}-{}", u64::MAX) // 32 + 1 + 20 = 53
}

pub fn claim_max() -> Claim {
    Claim {
        v: 1,
        card: [0xff; 32],
        node: [0xff; 32],
        ts: u64::MAX,
        handle: MAX_HANDLE.to_string(),
        sig: [0xff; 64],
    }
}

pub fn presence_max() -> Presence {
    Presence {
        v: 1,
        claim: claim_max(),
        handle: effective_max(),
        public: vec![0xff; 4096],
        attached: true,
        ts: u64::MAX,
        mac: [0xff; 32],
    }
}

pub fn departure_max() -> Departure {
    Departure { v: 1, card: [0xff; 32], node: [0xff; 32], ts: u64::MAX, mac: [0xff; 32] }
}

pub fn unreachable_max() -> Unreachable {
    Unreachable { v: 1, card: [0xff; 32], node: [0xff; 32], reporter: claim_max(), ts: u64::MAX, mac: [0xff; 32] }
}
