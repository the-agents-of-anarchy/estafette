use estafette::crypto::NetworkKeys;
use estafette::payload::seal_payload;
use estafette::wire::{
    self, Addr, Infrastructure, Payload, PresenceRequest, RelayUrl, SignatureBytes, Version,
};
use iroh::SecretKey;

use crate::payload_placeholders::{UNSET_MAC, UNSIGNED, wall_clock_ms};

/// Milliseconds in a second.
const MILLISECONDS_PER_SECOND: u64 = 1000;

/// Seconds since the Unix epoch by this machine's clock, the unit of an Infrastructure `ts` (spec 3.14).
pub fn wall_clock_seconds() -> u64 {
    wall_clock_ms() / MILLISECONDS_PER_SECOND
}

/// An unsealed Infrastructure record naming the host under the relay, its address the relay alone, with the record counter and the certificate.
pub fn infrastructure_record(
    host: &iroh::PublicKey,
    relay: &str,
    ts: u64,
    cert: SignatureBytes,
) -> Payload {
    Payload::Infrastructure(Infrastructure {
        v: Version::CURRENT,
        host: *host.as_bytes(),
        relay: RelayUrl::try_from(relay.to_owned()).unwrap(),
        addr: Addr::from_endpoint_report([relay.to_owned()], []),
        ts,
        cert,
        mac: UNSET_MAC,
        sig: UNSIGNED,
    })
}

/// An unsealed request for Presence from the requester, carrying the seq.
pub fn presence_request(requester: &iroh::PublicKey, seq: u64) -> Payload {
    Payload::PresenceRequest(PresenceRequest {
        v: Version::CURRENT,
        requester: *requester.as_bytes(),
        seq,
        mac: UNSET_MAC,
        sig: UNSIGNED,
    })
}

/// The payload signed by its own signer and macked under the network keys, as the crate seals it for the topic.
pub fn sealed(keys: &NetworkKeys, signer: &SecretKey, payload: Payload) -> Vec<u8> {
    seal_payload(keys, signer, payload).unwrap()
}

/// The sealed payload decoded back, ready to edit after its signature was made.
pub fn unsealed(bytes: &[u8]) -> Payload {
    wire::decode(bytes).unwrap()
}

/// The payload as edited, its signature kept and its mac computed afresh under the network keys, as a forwarder holding the secret would send it.
pub fn remacked(keys: &NetworkKeys, payload: Payload) -> Vec<u8> {
    let mac = keys.mac(&wire::encode(&with_mac(payload.clone(), UNSET_MAC)).unwrap());
    wire::encode(&with_mac(payload, *mac.as_bytes())).unwrap()
}

/// The payload carrying the mac.
fn with_mac(mut payload: Payload, mac: wire::Mac) -> Payload {
    match &mut payload {
        Payload::Presence(presence) => presence.mac = mac,
        Payload::Departure(departure) => departure.mac = mac,
        Payload::Unreachable(report) => report.mac = mac,
        Payload::Infrastructure(record) => record.mac = mac,
        Payload::PresenceRequest(request) => request.mac = mac,
    }
    payload
}
