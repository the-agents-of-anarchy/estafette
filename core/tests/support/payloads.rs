use estafette::crypto::NetworkKeys;
use estafette::payload::seal_payload;
use estafette::wire::{Addr, Payload, Presence, Version};
use iroh::{EndpointAddr, SecretKey};

use crate::payload_placeholders::{UNSET_MAC, UNSIGNED, wall_clock_ms};

const EMPTY_PUBLIC: &[u8] = b"{}";

/// A Presence of the signer's card under the handle at the address, signed by the card and sealed under the network keys, its seq the clock now.
pub fn presence_signed_by(
    keys: &NetworkKeys,
    signer: &SecretKey,
    handle: &str,
    address: &EndpointAddr,
) -> Vec<u8> {
    presence_signed_by_at(keys, signer, handle, address, wall_clock_ms())
}

/// A Presence as `presence_signed_by` makes it, carrying the given seq, so a test can name it among the events.
pub fn presence_signed_by_at(
    keys: &NetworkKeys,
    signer: &SecretKey,
    handle: &str,
    address: &EndpointAddr,
    seq: u64,
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
        seq,
        mac: UNSET_MAC,
        sig: UNSIGNED,
    });
    seal_payload(keys, signer, presence).unwrap()
}
