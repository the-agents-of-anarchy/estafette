use std::time::{SystemTime, UNIX_EPOCH};

use estafette::crypto::NetworkKeys;
use estafette::payload::seal_payload;
use estafette::wire::{self, Addr, Departure, Payload, Presence, SignatureBytes, Version};
use iroh::{EndpointAddr, SecretKey};

/// The mac a payload carries before sealing fills it.
pub const UNSET_MAC: wire::Mac = [0; 32];
const UNSIGNED: SignatureBytes = SignatureBytes([0; 64]);
const EMPTY_PUBLIC: &[u8] = b"{}";

/// Milliseconds since the Unix epoch by this machine's clock.
pub fn wall_clock_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// A Presence of the signer's card under the handle at the address, signed by the card and sealed under the network keys, its seq the clock now.
pub fn presence_signed_by(
    keys: &NetworkKeys,
    signer: &SecretKey,
    handle: &str,
    address: &EndpointAddr,
) -> Vec<u8> {
    let presence = Payload::Presence(Presence {
        v: Version::CURRENT,
        card: *signer.public().as_bytes(),
        handle: handle.to_owned(),
        public: EMPTY_PUBLIC.to_vec(),
        attached: false,
        addr: Addr::from_endpoint_report(
            address.relay_urls().map(ToString::to_string),
            address.ip_addrs().copied(),
        ),
        seq: wall_clock_ms(),
        mac: UNSET_MAC,
        sig: UNSIGNED,
    });
    seal_payload(keys, signer, presence).unwrap()
}

/// A Departure of the signer's card carrying the seq, signed by the card and sealed under the network keys.
pub fn departure_signed_by(keys: &NetworkKeys, signer: &SecretKey, seq: u64) -> Vec<u8> {
    let departure = Payload::Departure(Departure {
        v: Version::CURRENT,
        card: *signer.public().as_bytes(),
        seq,
        mac: UNSET_MAC,
        sig: UNSIGNED,
    });
    seal_payload(keys, signer, departure).unwrap()
}
