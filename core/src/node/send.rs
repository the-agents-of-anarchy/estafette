use iroh::EndpointId;
use n0_future::join_all;

use crate::errors::ErrorCode;
use crate::keys::render_public_key;
use crate::net::direct::{DirectError, DirectHost};
use crate::node::presence::{Node, endpoint_id, message_id_text};
use crate::payload::ZERO_MAC;
use crate::peers::Peer;
use crate::wire::{self, MESSAGE_BODY_LIMIT_BYTES, Message, MessageAck, MessageId, Nonce, Version};

const UUID_V7_RANDOM_BYTES: usize = 10;

/// What a delivered message reports: its id and whether the recipient's inbox has a client attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub id: String,
    pub attached: bool,
}

/// The outcome of one send of a broadcast, with the handle it was sent to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastResult {
    pub to: String,
    pub outcome: Result<Delivery, ErrorCode>,
}

impl Node {
    /// Sends the body to the one peer whose effective handle is `to`, by the send algorithm of spec 3.10.
    ///
    /// # Errors
    /// `ErrorCode` naming why the message was not delivered, one of the codes of spec 3.10.
    pub async fn send(&self, to: &str, body: &str) -> Result<Delivery, ErrorCode> {
        let recipient = only_card(&cards_named(&self.peers(), to))?;
        refuse_oversized(body)?;
        self.deliver(recipient, self.message_for(recipient, body))
            .await
    }

    /// Sends the body to each handle concurrently and returns one result per handle, in the order given.
    pub async fn broadcast(&self, to: &[String], body: &str) -> Vec<BroadcastResult> {
        join_all(to.iter().map(|handle| async move {
            BroadcastResult {
                to: handle.clone(),
                outcome: self.send(handle, body).await,
            }
        }))
        .await
    }

    /// The Message that carries the body to the recipient under a fresh id stamped with its send time.
    fn message_for(&self, recipient: wire::PublicKey, body: &str) -> Message {
        let sent_at = self.state.now_ms();
        Message {
            v: Version::CURRENT,
            id: self.message_id_stamped(sent_at),
            to: recipient,
            handle: self.own_handle().as_str().to_owned(),
            attached: self.state.is_attached(),
            sent_at,
            body: body.to_owned(),
            mac: ZERO_MAC,
        }
    }

    /// A fresh message id: a UUID version 7 carrying `now_ms`, its other bits drawn from the node's secure random source (spec 3.1).
    fn message_id_stamped(&self, now_ms: u64) -> MessageId {
        let mut draw = Nonce::default();
        (self.fill_message_id)(&mut draw);
        let mut random = [0; UUID_V7_RANDOM_BYTES];
        random
            .iter_mut()
            .zip(draw)
            .for_each(|(byte, drawn)| *byte = drawn);
        uuid::Builder::from_unix_timestamp_millis(now_ms, &random)
            .into_uuid()
            .into_bytes()
    }

    /// Writes the message to the recipient and waits for its ack, dropping the held connection and recording the contact on any failure.
    async fn deliver(
        &self,
        recipient: wire::PublicKey,
        message: Message,
    ) -> Result<Delivery, ErrorCode> {
        let target = endpoint_id(&recipient).ok_or(ErrorCode::Unreachable)?;
        let id = message.id;
        match self.client.send_message(target, message).await {
            Ok(ack) => {
                self.state.contact_succeeded(recipient);
                self.delivery_of(ack, id, target).await
            }
            Err(failure) => {
                self.state.contact_failed(recipient);
                self.client.drop_held_connection(target).await;
                Err(failure_error(&failure))
            }
        }
    }

    /// The delivery an accepting ack reports, or the refusal of any other ack after dropping the held connection.
    async fn delivery_of(
        &self,
        ack: MessageAck,
        id: MessageId,
        target: EndpointId,
    ) -> Result<Delivery, ErrorCode> {
        if ack.accepted {
            Ok(Delivery {
                id: message_id_text(id),
                attached: ack.attached,
            })
        } else {
            self.client.drop_held_connection(target).await;
            Err(refusal_error(ack.reason.as_deref()))
        }
    }
}

/// The cards of the peers whose effective handle is `to`, whatever their status.
fn cards_named(peers: &[Peer], to: &str) -> Vec<wire::PublicKey> {
    peers
        .iter()
        .filter(|peer| peer.effective_handle.as_str() == to)
        .map(|peer| peer.card)
        .collect()
}

/// The one card of the matches, or `unknown_handle` for none and `ambiguous_handle` naming each card for several.
fn only_card(matches: &[wire::PublicKey]) -> Result<wire::PublicKey, ErrorCode> {
    match matches {
        [] => Err(ErrorCode::UnknownHandle),
        [card] => Ok(*card),
        several => Err(ErrorCode::AmbiguousHandle {
            candidates: several
                .iter()
                .filter_map(endpoint_id)
                .map(|id| render_public_key(&id))
                .collect(),
        }),
    }
}

/// `too_large` when the body is over the message body limit.
const fn refuse_oversized(body: &str) -> Result<(), ErrorCode> {
    if body.len() > MESSAGE_BODY_LIMIT_BYTES {
        Err(ErrorCode::TooLarge)
    } else {
        Ok(())
    }
}

/// The error code a recipient's refusal reason names, `refused` when it names no code the recipient sends.
fn refusal_error(reason: Option<&str>) -> ErrorCode {
    if reason == Some(ErrorCode::TooLarge.code()) {
        ErrorCode::TooLarge
    } else {
        ErrorCode::Refused
    }
}

/// The error code of a send that got no valid acknowledgement.
const fn failure_error(failure: &DirectError) -> ErrorCode {
    match failure {
        DirectError::Unreachable => ErrorCode::Unreachable,
        DirectError::Timeout => ErrorCode::Timeout,
        DirectError::Refused
        | DirectError::UnsealableRequest(_)
        | DirectError::InvalidAck(_)
        | DirectError::OversizedAck
        | DirectError::UnexpectedAck
        | DirectError::MessageAckIdMismatch
        | DirectError::ProbeNonceMismatch => ErrorCode::Refused,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use iroh::endpoint::Connection;
    use iroh::protocol::{AcceptError, ProtocolHandler, Router};

    use crate::net::direct::{ClientTimeouts, DirectClient, DirectServer};
    use crate::net::endpoint::DIRECT_ALPN;
    use crate::node::presence::tests::{
        DUPLICATE_KEY_PUBLIC, FIRST_HANDLE, FIRST_PUBLIC, FIRST_SEED, INTEGER_PAST_64_BITS_PUBLIC,
        INTRODUCE_ROUND_TRIP, Member, MemberPlan, PROBE_NONCE, SECOND_HANDLE, SECOND_SEED,
        SPEC_OBSERVER_WAIT, Scratch, THIRD_HANDLE, THIRD_SEED, WITNESS_HANDLE, WITNESS_SEED, bind,
        card, inbox_frames, keys, listed_within, message_to, start_members_reaching, start_pair,
        start_third_through_second, wall_clock_ms,
    };
    use crate::node::presence::wire_addr;
    use crate::payload::{PayloadError, open_request, seal_response};
    use crate::peers::Status;
    use crate::wire::{
        Addr, Introduce, IntroduceAck, ProbeAck, Request, Response, WIRE_LIMIT_BYTES,
    };

    use super::*;

    const BODY: &str = "a body sent by the send tool";
    const NO_ONE: &str = "nobody";
    const FIRST_CONNECTION_WAIT: Duration = Duration::from_secs(3);
    const CONNECTION_POLL: Duration = Duration::from_millis(20);
    const FIRST_MATCH_SEED: u8 = 11;
    const MOST_MATCHES: usize = 8;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum PeerPhase {
        Serving,
        Stopping,
    }

    struct ScriptedHost {
        own_card: wire::PublicKey,
        addr: Addr,
        phase: PeerPhase,
        introduced_as: Option<&'static str>,
        public: &'static [u8],
    }

    impl DirectHost for ScriptedHost {
        fn accepted_connections(&self) -> crate::net::direct::AcceptedConnections {
            crate::net::direct::AcceptedConnections::new(std::sync::Arc::new(
                tokio::time::Instant::now,
            ))
        }

        fn is_stopping(&self) -> bool {
            self.phase == PeerPhase::Stopping
        }

        fn own_card(&self) -> Option<wire::PublicKey> {
            Some(self.own_card)
        }

        fn is_attached(&self) -> bool {
            false
        }

        fn receive_message(&self, _sender: wire::PublicKey, _message: Message) {}

        fn receive_introduce(
            &self,
            _sender: wire::PublicKey,
            _introduce: Introduce,
        ) -> IntroduceAck {
            IntroduceAck {
                v: Version::CURRENT,
                handle: self.introduced_as.map(str::to_owned),
                public: self.public.to_vec(),
                attached: false,
                addr: self.addr.clone(),
                mac: ZERO_MAC,
            }
        }
    }

    #[derive(Debug)]
    struct CountedConnections<H> {
        server: H,
        connections: Arc<AtomicUsize>,
    }

    impl<H: ProtocolHandler> ProtocolHandler for CountedConnections<H> {
        async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
            self.connections.fetch_add(1, Ordering::SeqCst);
            self.server.accept(connection).await
        }
    }

    struct ScriptedPeer {
        endpoint: iroh::Endpoint,
        connections: Arc<AtomicUsize>,
        _router: Router,
    }

    impl ScriptedPeer {
        async fn connections_after_first(&self) -> usize {
            tokio::time::timeout(FIRST_CONNECTION_WAIT, async {
                while self.connections.load(Ordering::SeqCst) == 0 {
                    tokio::time::sleep(CONNECTION_POLL).await;
                }
            })
            .await
            .unwrap();
            self.connections.load(Ordering::SeqCst)
        }
    }

    async fn first_member_listing_a_scripted_peer(
        scratch: &Scratch,
        phase: PeerPhase,
    ) -> (Member, ScriptedPeer) {
        first_member_listing_a_peer_served_by(scratch, |addr| {
            DirectServer::new(
                keys(),
                ScriptedHost {
                    own_card: card(WITNESS_SEED),
                    addr,
                    phase,
                    introduced_as: None,
                    public: &[],
                },
            )
        })
        .await
    }

    async fn first_member_listing_a_peer_served_by<H: ProtocolHandler>(
        scratch: &Scratch,
        server_at: impl FnOnce(Addr) -> H,
    ) -> (Member, ScriptedPeer) {
        let endpoint = bind(WITNESS_SEED).await;
        let peer_address = endpoint.endpoint().addr();
        let mut members = start_members_reaching(
            scratch,
            vec![MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).reaching(&[WITNESS_SEED])],
            &[(WITNESS_SEED, peer_address)],
        )
        .await;
        let member = members.pop().unwrap();
        endpoint.replace_address(member.endpoint.addr());
        let connections = Arc::new(AtomicUsize::new(0));
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(
                DIRECT_ALPN,
                CountedConnections {
                    server: server_at(wire_addr(&endpoint.endpoint().addr())),
                    connections: Arc::clone(&connections),
                },
            )
            .spawn();
        let introducing =
            DirectClient::new(endpoint.endpoint().clone(), keys(), ClientTimeouts::SPEC);
        let introduction = message_to(FIRST_SEED, WITNESS_HANDLE);
        assert!(
            introducing
                .send_message(member.endpoint.id(), introduction)
                .await
                .unwrap()
                .accepted
        );
        let peer = ScriptedPeer {
            endpoint: endpoint.endpoint().clone(),
            connections,
            _router: router,
        };
        peer.connections_after_first().await;
        tokio::time::sleep(INTRODUCE_ROUND_TRIP).await;
        (member, peer)
    }

    #[tokio::test]
    async fn spec_3_10_send_to_a_listed_handle_delivers_it_and_reports_the_ack() {
        let scratch = Scratch::new("send-delivered").await;
        let (first, second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();

        let delivery = first.node.send(SECOND_HANDLE, BODY).await.unwrap();

        assert!(!delivery.attached);
        let frame = inbox_frames(&second, 1).await.remove(0);
        assert_eq!(frame["from"], FIRST_HANDLE);
        assert_eq!(frame["body"], BODY);
        assert_eq!(frame["id"], delivery.id);
        drop(scratch);
    }

    const SPEC_UUID_VERSION: usize = 7;
    const UUID_TIMESTAMP_BYTES: usize = 6;
    const BYTES_OF_A_MILLISECOND_COUNT: usize = 8;

    fn unix_millis_stamped_in(id: &uuid::Uuid) -> u64 {
        let mut millis = [0; BYTES_OF_A_MILLISECOND_COUNT];
        millis[BYTES_OF_A_MILLISECOND_COUNT - UUID_TIMESTAMP_BYTES..]
            .copy_from_slice(&id.as_bytes()[..UUID_TIMESTAMP_BYTES]);
        u64::from_be_bytes(millis)
    }

    #[tokio::test]
    async fn spec_3_1_the_id_the_recipient_stores_is_a_uuid_version_7_stamped_by_the_senders_clock()
    {
        let scratch = Scratch::new("send-uuid-v7").await;
        let (first, second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();

        let sent_from = wall_clock_ms();
        let delivery = first.node.send(SECOND_HANDLE, BODY).await.unwrap();
        let sent_until = wall_clock_ms();

        let frame = inbox_frames(&second, 1).await.remove(0);
        assert_eq!(frame["id"], delivery.id);
        let stored = uuid::Uuid::parse_str(frame["id"].as_str().unwrap()).unwrap();
        assert_eq!(stored.get_version_num(), SPEC_UUID_VERSION);
        assert_eq!(stored.get_variant(), uuid::Variant::RFC4122);
        let stamped = unix_millis_stamped_in(&stored);
        assert!(
            (sent_from..=sent_until).contains(&stamped),
            "stamped {stamped}, sent between {sent_from} and {sent_until}"
        );
        let drawn_by_the_node: [u8; UUID_V7_RANDOM_BYTES] =
            PROBE_NONCE[..UUID_V7_RANDOM_BYTES].try_into().unwrap();
        assert_eq!(
            stored,
            uuid::Builder::from_unix_timestamp_millis(stamped, &drawn_by_the_node).into_uuid()
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_10_send_reports_attached_true_when_the_recipient_has_an_inbox_client() {
        let scratch = Scratch::new("send-attached").await;
        let (first, second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        let inbox_client = tokio::net::UnixStream::connect(&second.inbox_path)
            .await
            .unwrap();

        let delivery = first.node.send(SECOND_HANDLE, BODY).await.unwrap();

        assert!(delivery.attached);
        drop(inbox_client);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_10_send_to_a_handle_no_member_holds_is_unknown_handle() {
        let scratch = Scratch::new("send-unknown").await;
        let (first, _second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();

        let refused = first.node.send(NO_ONE, BODY).await;

        assert_eq!(refused, Err(ErrorCode::UnknownHandle));
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_10_send_of_a_body_over_the_limit_is_too_large_before_any_connection() {
        let scratch = Scratch::new("send-too-large").await;
        let (first, second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        tokio::time::sleep(INTRODUCE_ROUND_TRIP).await;
        second.endpoint.close().await;
        let oversized = "x".repeat(MESSAGE_BODY_LIMIT_BYTES + 1);

        let refused = first.node.send(SECOND_HANDLE, &oversized).await;

        assert_eq!(refused, Err(ErrorCode::TooLarge));
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_10_send_of_a_body_at_the_limit_is_delivered() {
        let scratch = Scratch::new("send-at-limit").await;
        let (first, second) = start_pair(&scratch).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        let at_limit = "x".repeat(MESSAGE_BODY_LIMIT_BYTES);

        let delivery = first.node.send(SECOND_HANDLE, &at_limit).await;

        delivery.unwrap();
        let frame = inbox_frames(&second, 1).await.remove(0);
        assert_eq!(
            frame["body"].as_str().map(str::len),
            Some(MESSAGE_BODY_LIMIT_BYTES)
        );
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_10_send_to_a_peer_that_cannot_be_connected_is_unreachable_and_sets_it_unreachable()
     {
        let scratch = Scratch::new("send-unreachable").await;
        let (first, peer) =
            first_member_listing_a_scripted_peer(&scratch, PeerPhase::Serving).await;
        peer.endpoint.close().await;

        let failed = first.node.send(WITNESS_HANDLE, BODY).await;

        assert_eq!(failed, Err(ErrorCode::Unreachable));
        let status = listed_within(&first, WITNESS_SEED, Duration::ZERO)
            .await
            .unwrap()
            .status;
        assert_eq!(status, Status::Unreachable);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_10_a_refused_send_returns_refused_and_drops_the_held_connection() {
        let scratch = Scratch::new("send-refused").await;
        let (first, peer) =
            first_member_listing_a_scripted_peer(&scratch, PeerPhase::Stopping).await;

        let refused = first.node.send(WITNESS_HANDLE, BODY).await;
        let connections_after_refusal = peer.connections.load(Ordering::SeqCst);
        let refused_again = first.node.send(WITNESS_HANDLE, BODY).await;

        assert_eq!(refused, Err(ErrorCode::Refused));
        assert_eq!(refused_again, Err(ErrorCode::Refused));
        assert_eq!(
            peer.connections.load(Ordering::SeqCst),
            connections_after_refusal + 1
        );
        drop(scratch);
    }

    const ANOTHER_MESSAGE_ID: MessageId = [8; 16];

    #[derive(Debug)]
    struct AckingAnotherMessageId {
        addr: Addr,
    }

    impl AckingAnotherMessageId {
        fn answer(&self, request: Request) -> Response {
            match request {
                Request::Message(_) => Response::MessageAck(MessageAck {
                    v: Version::CURRENT,
                    id: ANOTHER_MESSAGE_ID,
                    accepted: true,
                    attached: true,
                    reason: None,
                }),
                Request::Probe(probe) => Response::ProbeAck(ProbeAck {
                    v: Version::CURRENT,
                    nonce: probe.nonce,
                    attached: false,
                    mac: ZERO_MAC,
                }),
                Request::Introduce(_) => Response::IntroduceAck(IntroduceAck {
                    v: Version::CURRENT,
                    handle: None,
                    public: Vec::new(),
                    attached: false,
                    addr: self.addr.clone(),
                    mac: ZERO_MAC,
                }),
            }
        }
    }

    impl ProtocolHandler for AckingAnotherMessageId {
        async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
            while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                let request_bytes = recv.read_to_end(WIRE_LIMIT_BYTES).await.unwrap();
                let request = open_request(&keys(), &request_bytes).unwrap();
                let response = seal_response(&keys(), self.answer(request)).unwrap();
                send.write_all(&response).await.unwrap();
                send.finish().unwrap();
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn spec_3_10_a_send_acknowledged_for_another_id_fails_sets_the_peer_unreachable_and_drops_the_held_connection()
     {
        let scratch = Scratch::new("send-ack-id").await;
        let (first, peer) =
            first_member_listing_a_peer_served_by(&scratch, |addr| AckingAnotherMessageId { addr })
                .await;

        let failed = first.node.send(WITNESS_HANDLE, BODY).await;
        let connections_after_failure = peer.connections.load(Ordering::SeqCst);
        let failed_again = first.node.send(WITNESS_HANDLE, BODY).await;

        assert_eq!(failed, Err(ErrorCode::Refused));
        assert_eq!(failed_again, Err(ErrorCode::Refused));
        assert_eq!(
            peer.connections.load(Ordering::SeqCst),
            connections_after_failure + 1
        );
        let status = listed_within(&first, WITNESS_SEED, Duration::ZERO)
            .await
            .unwrap()
            .status;
        assert_eq!(status, Status::Unreachable);
        drop(scratch);
    }

    #[tokio::test]
    async fn spec_3_5_an_introduce_ack_whose_public_no_card_sends_changes_nothing_at_the_node() {
        for public in [DUPLICATE_KEY_PUBLIC, INTEGER_PAST_64_BITS_PUBLIC] {
            let scratch = Scratch::new("introduce-ack-public").await;
            let (first, _peer) = first_member_listing_a_peer_served_by(&scratch, |addr| {
                DirectServer::new(
                    keys(),
                    ScriptedHost {
                        own_card: card(WITNESS_SEED),
                        addr,
                        phase: PeerPhase::Serving,
                        introduced_as: Some(WITNESS_HANDLE),
                        public,
                    },
                )
            })
            .await;

            let listed = listed_within(&first, WITNESS_SEED, Duration::ZERO)
                .await
                .unwrap();

            assert_eq!(listed.public, None, "{}", String::from_utf8_lossy(public));
            drop(scratch);
        }
    }

    #[tokio::test]
    async fn spec_3_8_a_send_answered_by_a_refusal_sets_an_unreachable_peer_online() {
        let scratch = Scratch::new("send-contact").await;
        let (first, _peer) =
            first_member_listing_a_scripted_peer(&scratch, PeerPhase::Stopping).await;
        let before = listed_within(&first, WITNESS_SEED, Duration::ZERO)
            .await
            .unwrap()
            .status;

        let refused = first.node.send(WITNESS_HANDLE, BODY).await;

        assert_eq!(before, Status::Unreachable);
        assert_eq!(refused, Err(ErrorCode::Refused));
        let after = listed_within(&first, WITNESS_SEED, Duration::ZERO)
            .await
            .unwrap()
            .status;
        assert_eq!(after, Status::Online);
        drop(scratch);
    }

    #[tokio::test]
    async fn test_14_broadcast_to_three_handles_one_unknown_returns_two_delivered_and_one_unknown_handle()
     {
        let scratch = Scratch::new("send-t14").await;
        let (first, second) = start_pair(&scratch).await;
        let third = start_third_through_second(&scratch, &second, THIRD_HANDLE).await;
        listed_within(&first, SECOND_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        listed_within(&first, THIRD_SEED, SPEC_OBSERVER_WAIT)
            .await
            .unwrap();
        let handles = [
            THIRD_HANDLE.to_owned(),
            NO_ONE.to_owned(),
            SECOND_HANDLE.to_owned(),
        ];

        let results = first.node.broadcast(&handles, BODY).await;

        assert_eq!(
            results
                .iter()
                .map(|result| result.to.as_str())
                .collect::<Vec<_>>(),
            [THIRD_HANDLE, NO_ONE, SECOND_HANDLE]
        );
        assert_eq!(results[1].outcome, Err(ErrorCode::UnknownHandle));
        let to_third = inbox_frames(&third, 1).await.remove(0);
        let to_second = inbox_frames(&second, 1).await.remove(0);
        assert_eq!(to_third["id"], results[0].outcome.as_ref().unwrap().id);
        assert_eq!(to_second["id"], results[2].outcome.as_ref().unwrap().id);
        assert_eq!(to_third["body"], BODY);
        assert_eq!(to_second["body"], BODY);
        drop(scratch);
    }

    fn cards_seeded_by_index(count: usize) -> Vec<wire::PublicKey> {
        (FIRST_MATCH_SEED..=u8::MAX).take(count).map(card).collect()
    }

    fn rendered(card: &wire::PublicKey) -> String {
        render_public_key(&endpoint_id(card).unwrap())
    }

    #[test]
    fn spec_3_10_no_match_is_unknown_handle() {
        assert_eq!(only_card(&[]), Err(ErrorCode::UnknownHandle));
    }

    #[test]
    fn spec_3_10_one_match_is_its_card_whichever_card_it_is() {
        for card in cards_seeded_by_index(MOST_MATCHES) {
            assert_eq!(only_card(&[card]), Ok(card));
        }
    }

    #[test]
    fn spec_3_10_several_matches_are_ambiguous_handle_naming_each_card_in_order() {
        for count in 2..=MOST_MATCHES {
            let matches = cards_seeded_by_index(count);

            let decided = only_card(&matches);

            assert_eq!(
                decided,
                Err(ErrorCode::AmbiguousHandle {
                    candidates: matches.iter().map(rendered).collect()
                })
            );
        }
    }

    fn every_direct_error() -> Vec<DirectError> {
        let payload_error = || PayloadError::SignerIsNotThePayloadSigner;
        vec![
            DirectError::Unreachable,
            DirectError::Timeout,
            DirectError::Refused,
            DirectError::UnsealableRequest(payload_error()),
            DirectError::InvalidAck(payload_error()),
            DirectError::OversizedAck,
            DirectError::UnexpectedAck,
            DirectError::MessageAckIdMismatch,
            DirectError::ProbeNonceMismatch,
        ]
    }

    #[test]
    fn spec_3_10_a_failed_ack_is_unreachable_or_timeout_only_for_those_failures() {
        for failure in every_direct_error() {
            let expected = match failure {
                DirectError::Unreachable => ErrorCode::Unreachable,
                DirectError::Timeout => ErrorCode::Timeout,
                DirectError::Refused
                | DirectError::UnsealableRequest(_)
                | DirectError::InvalidAck(_)
                | DirectError::OversizedAck
                | DirectError::UnexpectedAck
                | DirectError::MessageAckIdMismatch
                | DirectError::ProbeNonceMismatch => ErrorCode::Refused,
            };

            assert_eq!(failure_error(&failure), expected);
        }
    }

    #[test]
    fn spec_3_10_a_refusal_reason_is_too_large_only_when_it_is_the_too_large_code() {
        let reasons = [
            ErrorCode::NotClaimed,
            ErrorCode::CardNotFound,
            ErrorCode::CardHeld,
            ErrorCode::CardInvalid,
            ErrorCode::SidecarInvalid,
            ErrorCode::UnknownHandle,
            ErrorCode::AmbiguousHandle {
                candidates: Vec::new(),
            },
            ErrorCode::Unreachable,
            ErrorCode::Timeout,
            ErrorCode::Refused,
            ErrorCode::TooLarge,
            ErrorCode::BadRequest,
        ];
        for reason in &reasons {
            let expected = if *reason == ErrorCode::TooLarge {
                ErrorCode::TooLarge
            } else {
                ErrorCode::Refused
            };

            assert_eq!(refusal_error(Some(reason.code())), expected);
        }
        assert_eq!(refusal_error(None), ErrorCode::Refused);
    }
}
