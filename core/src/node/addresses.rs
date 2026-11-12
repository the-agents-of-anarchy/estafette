//! The conversions between a card, a wire address and an iroh endpoint address, and the lookup entry they set.

use iroh::{EndpointAddr, EndpointId, TransportAddr};

use crate::net::endpoint::NodeEndpoint;
use crate::wire::{self, Addr};

/// Replaces the card's lookup entry on the endpoint with the carried address, never merging the two (spec 3.8, address lookup).
pub fn replace_lookup_entry(endpoint: &NodeEndpoint, card: &wire::PublicKey, addr: &Addr) {
    if let Some(id) = endpoint_id(card) {
        endpoint.replace_address(endpoint_addr(id, addr));
    }
}

/// The card as an iroh node id, or none when its bytes are not a valid key.
pub fn endpoint_id(card: &wire::PublicKey) -> Option<EndpointId> {
    EndpointId::from_bytes(card).ok()
}

/// The wire form of an endpoint address, within the caps of spec 3.6.
pub fn wire_addr(addr: &EndpointAddr) -> Addr {
    Addr::from_endpoint_report(
        addr.relay_urls().map(ToString::to_string),
        addr.ip_addrs().copied(),
    )
}

/// The endpoint address of the node at a carried wire address, leaving out relay URLs that do not parse.
pub fn endpoint_addr(id: EndpointId, addr: &Addr) -> EndpointAddr {
    let relays = addr
        .relays()
        .iter()
        .filter_map(|relay| relay.as_str().parse().ok())
        .map(TransportAddr::Relay);
    let direct = addr.direct().iter().copied().map(TransportAddr::Ip);
    EndpointAddr::from_parts(id, relays.chain(direct))
}
