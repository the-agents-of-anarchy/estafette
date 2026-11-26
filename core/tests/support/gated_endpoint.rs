use std::sync::Arc;

use estafette::clock::RuntimeClock;
use estafette::crypto::GateKey;
use estafette::net::direct::HeldConnections;
use estafette::net::endpoint::{GateFailure, NodeEndpoint, TransportRecord};
use estafette::trace::{LOCATION_SALT_BYTES, LocationSalt};
use iroh::{RelayUrl, SecretKey};
use tokio::runtime::Handle;

/// A register of held connections that measures silence on the runtime's monotonic clock and records nothing of the transport.
pub fn unrecorded_connections() -> HeldConnections {
    HeldConnections::new(
        Arc::new(RuntimeClock),
        TransportRecord::new(
            Arc::new(|_event| {}),
            LocationSalt::from_random_bytes([0; LOCATION_SALT_BYTES]),
        ),
    )
}

/// Binds on the runtime an endpoint under the key behind the network gate of the gate key, homed on the relays, handing every gate closing to the sink and holding its connections in the register.
pub fn bind_gated_endpoint(
    runtime: &Handle,
    key: SecretKey,
    gate_key: GateKey,
    relays: impl IntoIterator<Item = RelayUrl>,
    report_gate_closing: impl Fn(GateFailure) + Send + Sync + 'static,
    connections: HeldConnections,
) -> NodeEndpoint {
    runtime
        .block_on(NodeEndpoint::bind(
            key,
            gate_key,
            relays,
            |nonce| *nonce = SecretKey::generate().to_bytes(),
            report_gate_closing,
            connections,
        ))
        .unwrap()
}
