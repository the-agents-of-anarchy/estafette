use std::sync::Arc;
use std::time::Duration;

use iroh::address_lookup::MemoryLookup;
use iroh::endpoint::{BindError, QuicTransportConfig, VarInt, presets};
use iroh::{Endpoint, EndpointAddr, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey};
use iroh_gossip::ALPN as GOSSIP_ALPN;

use super::gate::{NONCE_BYTES, NetworkSecretGate};
use crate::crypto::GateKey;

pub use super::gate::GateFailure;

pub const DIRECT_ALPN: &[u8] = b"estafette/direct/1";
const QUIC_IDLE_TIMEOUT_MILLIS: u32 = 10_000;
const QUIC_KEEP_ALIVE_INTERVAL: Duration = Duration::from_millis(500);

/// The node's iroh endpoint, gated on the network secret for every gossip connection, with the address lookup the node feeds and the relay map it edits.
pub struct NodeEndpoint {
    endpoint: Endpoint,
    address_lookup: MemoryLookup,
    relay_map: RelayMap,
}

impl NodeEndpoint {
    /// Binds an endpoint under the key with the relays, the fed lookup as its only address source, and the network secret gate on every gossip connection, which hands each closing reason to the sink.
    ///
    /// # Errors
    /// The iroh bind error when no socket can be bound.
    pub async fn bind(
        secret_key: SecretKey,
        gate_key: GateKey,
        relays: impl IntoIterator<Item = RelayUrl>,
        fill_nonce: impl Fn(&mut [u8; NONCE_BYTES]) + Send + Sync + 'static,
        report_gate_closing: impl Fn(GateFailure) + Send + Sync + 'static,
    ) -> Result<Self, BindError> {
        let relay_map: RelayMap = relays.into_iter().collect();
        let address_lookup = MemoryLookup::new();
        let gate = NetworkSecretGate::new(
            gate_key,
            secret_key.public(),
            fill_nonce,
            report_gate_closing,
        );
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(secret_key)
            .alpns(vec![DIRECT_ALPN.to_vec(), GOSSIP_ALPN.to_vec()])
            .relay_mode(RelayMode::Custom(relay_map.clone()))
            .transport_config(quic_transport_config())
            .address_lookup(address_lookup.clone())
            .hooks(gate)
            .bind()
            .await?;
        Ok(Self {
            endpoint,
            address_lookup,
            relay_map,
        })
    }

    /// The iroh endpoint the gossip layer and the direct protocol run on.
    #[must_use]
    pub const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Replaces, never merges, the lookup entry of the address's card, so a dial by card id goes there alone.
    pub fn replace_address(&self, address: EndpointAddr) {
        self.address_lookup.set_endpoint_info(address);
    }

    /// Adds the relay to the relay map unless the map already holds it, so a held relay keeps its entry.
    pub async fn add_relay(&self, relay: RelayUrl) {
        if !self.relay_map.contains(&relay) {
            let entry = Arc::new(RelayConfig::from(relay.clone()));
            self.endpoint.insert_relay(relay, entry).await;
        }
    }

    /// Removes the relay from the relay map.
    pub async fn remove_relay(&self, relay: &RelayUrl) {
        self.endpoint.remove_relay(relay).await;
    }
}

/// The QUIC transport config with the idle timeout of spec 3.14 and a keep-alive short enough that a silent peer times out within one keep-alive interval of it.
fn quic_transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .keep_alive_interval(QUIC_KEEP_ALIVE_INTERVAL)
        .max_idle_timeout(Some(VarInt::from_u32(QUIC_IDLE_TIMEOUT_MILLIS).into()))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use iroh::endpoint::{
        ConnectError, ConnectWithOptsError, ConnectingError, Connection, ConnectionError,
    };
    use iroh::{EndpointId, TransportAddr};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use tokio::time::timeout;

    const SPEC_DIRECT_ALPN: &[u8] = b"estafette/direct/1";
    const SPEC_GATE_KEY_CONTEXT: &str = "messaging gate v1";
    const SPEC_GATE_TIMEOUT: Duration = Duration::from_millis(3000);
    const SPEC_GATE_TIMEOUT_WITH_SLACK: Duration = Duration::from_millis(4000);
    const SPEC_QUIC_IDLE_TIMEOUT_EARLIEST: Duration = Duration::from_secs(9);
    const SPEC_QUIC_IDLE_TIMEOUT_LATEST: Duration = Duration::from_secs(11);
    const MEMBER_SEED: u8 = 1;
    const SECOND_MEMBER_SEED: u8 = 2;
    const OUTSIDER_SEED: u8 = 3;
    const SPEC_PEER_SEED: u8 = 4;
    const SILENT_PEER_SEED: u8 = 5;
    const STRANGER_SEED: u8 = 6;
    const VANISHING_PEER_SEED: u8 = 7;
    const SPEC_PEER_NONCE: [u8; KEY_LEN] = [44; KEY_LEN];
    const MEMBER_SECRET: [u8; KEY_LEN] = [21; KEY_LEN];
    const OUTSIDER_SECRET: [u8; KEY_LEN] = [12; KEY_LEN];
    const FIRST_STREAM_BYTES: &[u8] = b"the first stream after the gate";
    const FIRST_HOST_RELAY: &str = "https://relay-one.estafette.invalid/";
    const SECOND_HOST_RELAY: &str = "https://relay-two.estafette.invalid/";
    const UNKNOWN_HOST_RELAY: &str = "https://relay-unknown.estafette.invalid/";
    const LOOPBACK_ANY_PORT: &str = "127.0.0.1:0";
    const LOCAL_DIAL_WAIT: Duration = Duration::from_millis(2000);

    fn gate_key_of(secret: [u8; KEY_LEN]) -> GateKey {
        *NetworkKeys::derive(&NetworkSecret::from_bytes(secret)).gate_key()
    }

    async fn bind_node(seed: u8, secret: [u8; KEY_LEN]) -> NodeEndpoint {
        bind_node_with_relays(seed, secret, Vec::new()).await
    }

    async fn bind_node_with_relays(
        seed: u8,
        secret: [u8; KEY_LEN],
        relays: Vec<RelayUrl>,
    ) -> NodeEndpoint {
        bind_node_reporting_to(seed, secret, relays, |_ignored_reason: GateFailure| {}).await
    }

    async fn bind_node_reporting_to(
        seed: u8,
        secret: [u8; KEY_LEN],
        relays: Vec<RelayUrl>,
        report_gate_closing: impl Fn(GateFailure) + Send + Sync + 'static,
    ) -> NodeEndpoint {
        NodeEndpoint::bind(
            SecretKey::from_bytes(&[seed; KEY_LEN]),
            gate_key_of(secret),
            relays,
            move |nonce: &mut [u8; KEY_LEN]| *nonce = [seed; KEY_LEN],
            report_gate_closing,
        )
        .await
        .unwrap()
    }

    async fn bind_node_recording_gate_closings(
        seed: u8,
        secret: [u8; KEY_LEN],
    ) -> (NodeEndpoint, Arc<Mutex<Vec<GateFailure>>>) {
        let closing_reasons = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&closing_reasons);
        let node = bind_node_reporting_to(seed, secret, Vec::new(), move |reason| {
            sink.lock().unwrap().push(reason);
        })
        .await;
        (node, closing_reasons)
    }

    fn relay_url(text: &str) -> RelayUrl {
        text.parse().unwrap()
    }

    async fn dial(
        dialer: &Endpoint,
        acceptor: &Endpoint,
        alpn: &[u8],
    ) -> (
        Result<Connection, ConnectError>,
        Result<Connection, ConnectingError>,
    ) {
        tokio::join!(dialer.connect(acceptor.addr(), alpn), accept_next(acceptor))
    }

    async fn accept_next(acceptor: &Endpoint) -> Result<Connection, ConnectingError> {
        acceptor.accept().await.unwrap().await
    }

    #[tokio::test]
    async fn spec_3_6_relay_map_holds_the_relays_of_the_known_hosts() {
        let host_relays = vec![relay_url(FIRST_HOST_RELAY), relay_url(SECOND_HOST_RELAY)];
        let member = bind_node_with_relays(MEMBER_SEED, MEMBER_SECRET, host_relays.clone()).await;

        for relay in &host_relays {
            assert!(member.endpoint().remove_relay(relay).await.is_some());
        }
        assert!(
            member
                .endpoint()
                .remove_relay(&relay_url(UNKNOWN_HOST_RELAY))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn spec_3_6_gate_admits_both_dial_directions_between_holders_of_the_secret() {
        let first = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let second = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;

        for (dialer, acceptor) in [(&first, &second), (&second, &first)] {
            let (dialed, accepted) =
                dial(dialer.endpoint(), acceptor.endpoint(), GOSSIP_ALPN).await;
            let (dialed, accepted) = (dialed.unwrap(), accepted.unwrap());
            assert_eq!(dialed.remote_id(), acceptor.endpoint().id());
            assert_eq!(accepted.remote_id(), dialer.endpoint().id());

            let (mut send, _unused_recv) = dialed.open_bi().await.unwrap();
            send.write_all(FIRST_STREAM_BYTES).await.unwrap();
            send.finish().unwrap();
            let (_unused_send, mut recv) = accepted.accept_bi().await.unwrap();
            assert_eq!(
                recv.read_to_end(FIRST_STREAM_BYTES.len()).await.unwrap(),
                FIRST_STREAM_BYTES
            );
        }
    }

    fn dial_is_locally_rejected(outcome: &Result<Connection, ConnectError>) -> bool {
        matches!(
            outcome,
            Err(ConnectError::Connecting {
                source: ConnectingError::LocallyRejected { .. },
                ..
            })
        )
    }

    fn accept_is_locally_rejected(outcome: &Result<Connection, ConnectingError>) -> bool {
        matches!(outcome, Err(ConnectingError::LocallyRejected { .. }))
    }

    fn is_the_gate_timeout(waited: Duration) -> bool {
        SPEC_GATE_TIMEOUT <= waited && waited < SPEC_GATE_TIMEOUT_WITH_SLACK
    }

    #[tokio::test]
    async fn spec_3_15_test_21_another_secret_is_closed_in_both_dial_directions() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let outsider = bind_node(OUTSIDER_SEED, OUTSIDER_SECRET).await;

        for (dialer, acceptor) in [(&member, &outsider), (&outsider, &member)] {
            let started = Instant::now();
            let (dialed, accepted) =
                dial(dialer.endpoint(), acceptor.endpoint(), GOSSIP_ALPN).await;
            assert!(started.elapsed() < SPEC_GATE_TIMEOUT);
            assert!(dial_is_locally_rejected(&dialed));
            assert!(accept_is_locally_rejected(&accepted));
        }
    }

    async fn bind_plain_endpoint(seed: u8) -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&[seed; KEY_LEN]))
            .alpns(vec![GOSSIP_ALPN.to_vec(), DIRECT_ALPN.to_vec()])
            .bind()
            .await
            .unwrap()
    }

    fn spec_gate_proof(
        secret: [u8; KEY_LEN],
        nonce: &[u8; KEY_LEN],
        verifier: EndpointId,
        prover: EndpointId,
    ) -> [u8; KEY_LEN] {
        let gate_key = blake3::derive_key(SPEC_GATE_KEY_CONTEXT, &secret);
        let message = [nonce.as_slice(), verifier.as_bytes(), prover.as_bytes()].concat();
        *blake3::keyed_hash(&gate_key, &message).as_bytes()
    }

    #[tokio::test]
    async fn spec_3_6_peer_speaking_the_spec_challenge_is_admitted_and_gets_the_spec_proof() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_plain_endpoint(SPEC_PEER_SEED).await;
        let (member_id, peer_id) = (member.endpoint().id(), peer.id());

        let speak_the_spec_gate = async {
            let connection = peer
                .connect(member.endpoint().addr(), GOSSIP_ALPN)
                .await
                .unwrap();
            let (mut challenge_send, mut proof_recv) = connection.open_bi().await.unwrap();
            challenge_send.write_all(&SPEC_PEER_NONCE).await.unwrap();
            challenge_send.finish().unwrap();
            let mut proof_received = [0; KEY_LEN];
            proof_recv.read_exact(&mut proof_received).await.unwrap();

            let (mut answer_send, mut nonce_recv) = connection.accept_bi().await.unwrap();
            let mut member_nonce = [0; KEY_LEN];
            nonce_recv.read_exact(&mut member_nonce).await.unwrap();
            let answer = spec_gate_proof(MEMBER_SECRET, &member_nonce, member_id, peer_id);
            answer_send.write_all(&answer).await.unwrap();
            answer_send.finish().unwrap();
            (connection, proof_received, member_nonce)
        };
        let ((_open_connection, proof_received, member_nonce), accepted) =
            tokio::join!(speak_the_spec_gate, accept_next(member.endpoint()));

        assert_eq!(
            proof_received,
            spec_gate_proof(MEMBER_SECRET, &SPEC_PEER_NONCE, peer_id, member_id)
        );
        assert_eq!(member_nonce, [MEMBER_SEED; KEY_LEN]);
        assert_eq!(accepted.unwrap().remote_id(), peer_id);
    }

    #[tokio::test]
    async fn spec_3_14_gate_timeout_closes_a_peer_that_never_answers_in_both_dial_directions() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let silent = bind_plain_endpoint(SILENT_PEER_SEED).await;

        let started = Instant::now();
        let (dialed, _held_by_silent) = tokio::join!(
            member.endpoint().connect(silent.addr(), GOSSIP_ALPN),
            accept_next(&silent)
        );
        assert!(is_the_gate_timeout(started.elapsed()));
        assert!(dial_is_locally_rejected(&dialed));

        let started = Instant::now();
        let (closed_by_member, accepted) = tokio::join!(
            async {
                let connection = silent
                    .connect(member.endpoint().addr(), GOSSIP_ALPN)
                    .await
                    .unwrap();
                connection.closed().await
            },
            accept_next(member.endpoint())
        );
        assert!(is_the_gate_timeout(started.elapsed()));
        assert!(accept_is_locally_rejected(&accepted));
        assert!(matches!(
            closed_by_member,
            ConnectionError::ApplicationClosed(_)
        ));
    }

    #[tokio::test]
    async fn spec_3_6_direct_alpn_accepts_any_node_id_without_the_gate() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let stranger = bind_plain_endpoint(STRANGER_SEED).await;

        let started = Instant::now();
        let (dialed, accepted) = dial(&stranger, member.endpoint(), DIRECT_ALPN).await;
        assert!(started.elapsed() < SPEC_GATE_TIMEOUT);
        let (dialed, accepted) = (dialed.unwrap(), accepted.unwrap());
        assert_eq!(accepted.remote_id(), stranger.id());
        assert_eq!(accepted.alpn(), SPEC_DIRECT_ALPN);
        assert_eq!(dialed.close_reason(), None);
        assert_eq!(
            dialed.remote_id(),
            SecretKey::from_bytes(&[MEMBER_SEED; KEY_LEN]).public()
        );
    }

    #[tokio::test]
    async fn spec_3_14_quic_idle_timeout_closes_a_connection_whose_peer_vanished() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let member_addr = member.endpoint().addr();
        let vanishing_peer = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let peer_and_connection = runtime.block_on(async move {
                let peer = bind_plain_endpoint(VANISHING_PEER_SEED).await;
                let connection = peer.connect(member_addr, DIRECT_ALPN).await.unwrap();
                (peer, connection)
            });
            runtime.shutdown_background();
            peer_and_connection
        });

        let accepted = accept_next(member.endpoint()).await.unwrap();
        let _silenced_peer = tokio::task::spawn_blocking(move || vanishing_peer.join())
            .await
            .unwrap()
            .unwrap();
        let started = Instant::now();
        let closed = accepted.closed().await;
        let waited = started.elapsed();

        assert!(matches!(closed, ConnectionError::TimedOut));
        assert!(
            SPEC_QUIC_IDLE_TIMEOUT_EARLIEST <= waited && waited < SPEC_QUIC_IDLE_TIMEOUT_LATEST
        );
    }

    async fn dial_by_card_id(
        dialer: &NodeEndpoint,
        acceptor: &NodeEndpoint,
    ) -> Option<(Connection, Connection)> {
        let (dial_outcome, accept_outcome) = tokio::join!(
            timeout(
                LOCAL_DIAL_WAIT,
                dialer
                    .endpoint()
                    .connect(acceptor.endpoint().id(), DIRECT_ALPN)
            ),
            timeout(LOCAL_DIAL_WAIT, accept_next(acceptor.endpoint()))
        );
        Some((dial_outcome.ok()?.ok()?, accept_outcome.ok()?.ok()?))
    }

    #[tokio::test]
    async fn spec_3_8_dial_by_card_id_reaches_the_address_the_node_fed() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;

        member.replace_address(peer.endpoint().addr());

        let (dialed, accepted) = dial_by_card_id(&member, &peer).await.unwrap();
        assert_eq!(dialed.remote_id(), peer.endpoint().id());
        assert_eq!(accepted.remote_id(), member.endpoint().id());
    }

    #[tokio::test]
    async fn spec_3_6_no_discovery_service_finds_a_card_the_node_never_fed() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let unfed = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;

        let dialed = member
            .endpoint()
            .connect(unfed.endpoint().id(), DIRECT_ALPN)
            .await;

        assert!(matches!(
            dialed,
            Err(ConnectError::Connect {
                source: ConnectWithOptsError::NoAddress { .. },
                ..
            })
        ));
        assert_eq!(member.endpoint().address_lookup().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn spec_3_8_replacing_a_card_address_drops_the_old_one() {
        let member = bind_node(MEMBER_SEED, MEMBER_SECRET).await;
        let peer = bind_node(SECOND_MEMBER_SEED, MEMBER_SECRET).await;
        let black_hole = std::net::UdpSocket::bind(LOOPBACK_ANY_PORT).unwrap();
        let unanswered_address = EndpointAddr::from_parts(
            peer.endpoint().id(),
            [TransportAddr::Ip(black_hole.local_addr().unwrap())],
        );

        member.replace_address(peer.endpoint().addr());
        member.replace_address(unanswered_address);
        assert!(dial_by_card_id(&member, &peer).await.is_none());

        member.replace_address(peer.endpoint().addr());
        assert!(dial_by_card_id(&member, &peer).await.is_some());
    }

    #[tokio::test]
    async fn spec_3_8_relay_loss_removes_a_relay_and_a_newer_record_adds_it_back() {
        let first_relay = relay_url(FIRST_HOST_RELAY);
        let second_relay = relay_url(SECOND_HOST_RELAY);
        let member =
            bind_node_with_relays(MEMBER_SEED, MEMBER_SECRET, vec![first_relay.clone()]).await;

        member.remove_relay(&first_relay).await;
        assert!(member.endpoint().remove_relay(&first_relay).await.is_none());

        member.add_relay(first_relay.clone()).await;
        assert!(member.endpoint().remove_relay(&first_relay).await.is_some());

        member.add_relay(second_relay.clone()).await;
        assert!(
            member
                .endpoint()
                .remove_relay(&second_relay)
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn spec_3_8_adding_a_relay_the_map_holds_leaves_its_entry_untouched() {
        let first_relay = relay_url(FIRST_HOST_RELAY);
        let member =
            bind_node_with_relays(MEMBER_SEED, MEMBER_SECRET, vec![first_relay.clone()]).await;
        let entry_without_address_discovery = Arc::new(RelayConfig::new(first_relay.clone(), None));
        member
            .endpoint()
            .insert_relay(first_relay.clone(), entry_without_address_discovery)
            .await;

        member.add_relay(first_relay.clone()).await;

        let entry = member.endpoint().remove_relay(&first_relay).await.unwrap();
        assert_eq!(entry.quic, None);
    }

    fn recorded_reasons_match(
        recorded: &Mutex<Vec<GateFailure>>,
        expected: fn(&GateFailure) -> bool,
    ) -> bool {
        let reasons = recorded.lock().unwrap();
        reasons.len() == 1 && reasons.iter().all(expected)
    }

    #[tokio::test]
    async fn spec_3_6_gate_reports_a_wrong_answer_as_the_reason_it_closed() {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let outsider = bind_plain_endpoint(OUTSIDER_SEED).await;
        let (member_id, outsider_id) = (member.endpoint().id(), outsider.id());

        let answer_under_another_secret = async {
            let connection = outsider
                .connect(member.endpoint().addr(), GOSSIP_ALPN)
                .await
                .unwrap();
            let (mut answer_send, mut nonce_recv) = connection.accept_bi().await.unwrap();
            let mut member_nonce = [0; KEY_LEN];
            nonce_recv.read_exact(&mut member_nonce).await.unwrap();
            let answer = spec_gate_proof(OUTSIDER_SECRET, &member_nonce, member_id, outsider_id);
            answer_send.write_all(&answer).await.unwrap();
            answer_send.finish().unwrap();
            connection
        };
        let (_closed_connection, accepted) =
            tokio::join!(answer_under_another_secret, accept_next(member.endpoint()));

        assert!(accept_is_locally_rejected(&accepted));
        assert!(recorded_reasons_match(&recorded, |reason| matches!(
            reason,
            GateFailure::WrongProof
        )));
    }

    #[tokio::test]
    async fn spec_3_14_gate_reports_the_timeout_as_the_reason_it_closed_on_a_silent_peer() {
        let (member, recorded) =
            bind_node_recording_gate_closings(MEMBER_SEED, MEMBER_SECRET).await;
        let silent = bind_plain_endpoint(SILENT_PEER_SEED).await;

        let (dialed, _held_by_silent) = tokio::join!(
            member.endpoint().connect(silent.addr(), GOSSIP_ALPN),
            accept_next(&silent)
        );

        assert!(dial_is_locally_rejected(&dialed));
        assert!(recorded_reasons_match(&recorded, |reason| matches!(
            reason,
            GateFailure::Timeout
        )));
    }
}
