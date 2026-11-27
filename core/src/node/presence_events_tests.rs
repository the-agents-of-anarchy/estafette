use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, VarInt};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh_gossip::ALPN as GOSSIP_ALPN;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedReceiver;

use super::DirectContact;
use super::tests::{
    FIRST_HANDLE, FIRST_PUBLIC, FIRST_SEED, FROZEN_CLOCK_MS, HOST_RELAY, HOST_SEED, MARKER_HANDLE,
    MARKER_PUBLIC, MARKER_SEED, MemberPlan, NOT_AN_ANNOUNCED_HANDLE, OWNER_SEED, REPLAYED_PUBLIC,
    SECOND_EFFECTIVE_FIRST_HANDLE, SECOND_HANDLE, SECOND_MARKER_SEED, SECOND_PUBLIC, SECOND_SEED,
    SPEC_ANNOUNCEMENT_MAX_AGE_MS, SPEC_INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS,
    SPEC_OBSERVER_WAIT, Scratch, THIRD_HANDLE, THIRD_PUBLIC, THIRD_SEED, WITNESS_HANDLE,
    WITNESS_PUBLIC, WITNESS_SEED, bind, card, entry_meeting_within, infrastructure_of, keys,
    message_to, noting_neighbors, relay_url, settable_clock, settled_direct_answers, short_peer_id,
    spawn_member, start_members, start_members_reaching, start_pair, start_witnessed,
};
use crate::clock::RuntimeClock;
use crate::config::MILLIS_PER_SECOND;
use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
use crate::event::{EventFields, RunRecord};
use crate::net::direct::tests::{test_salt, unrecorded_register};
use crate::net::direct::{ClientTimeouts, DirectClient, HeldConnections};
use crate::net::endpoint::{BoundEndpoint, DIRECT_ALPN, LocationHash, TransportRecord};
use crate::net::gossip::{GossipEvent, NetworkTopic};
use crate::node::addresses::wire_addr;
use crate::payload::{
    ZERO_MAC, ZERO_SIGNATURE, open_request, seal_payload, seal_response, sealed_by_any_key,
};
use crate::test_waits::{
    RecordedEvent, next_recorded, next_recorded_among, next_recorded_meeting, next_recorded_within,
    recorded_events, secret_key, wall_clock_ms,
};
use crate::wire::{
    Addr, Departure, Introduce, IntroduceAck, Payload, Presence, PresenceRequest, ProbeAck,
    Request, Response, Unreachable, Version, WIRE_LIMIT_BYTES,
};

const EVENT_WAIT: Duration = Duration::from_secs(10);
const SECOND_EFFECTIVE_MARKER_HANDLE: &str = "mark-2";
const STALE_SEQ: u64 = FROZEN_CLOCK_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS - 1;
const MARKER_RELAY: &str = "https://relay.marker.invalid/";
const MARKER_DIRECT_ADDRESS: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7)),
    4433,
);
const SECOND_MARKER_DIRECT_ADDRESS: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 8)),
    4433,
);

/// The fields of the next lookup replaced for the seed's card, failing with the name and the events seen when none is recorded within the wait.
async fn lookup_replaced_of(
    recorded: &mut UnboundedReceiver<RecordedEvent>,
    seed: u8,
) -> EventFields {
    let peer = ("peer", short_peer_id(seed));
    next_recorded_meeting(
        recorded,
        "peers.lookup_replaced",
        |fields| fields.first() == Some(&peer),
        EVENT_WAIT,
    )
    .await
}

/// The fields of the next event of that name naming the seed's card as its peer, failing with the name and the events seen when none is recorded within the wait.
async fn next_named_from(
    recorded: &mut UnboundedReceiver<RecordedEvent>,
    name: &str,
    seed: u8,
) -> EventFields {
    let peer = ("peer", short_peer_id(seed));
    next_recorded_meeting(recorded, name, |fields| fields.contains(&peer), EVENT_WAIT).await
}

#[tokio::test]
async fn observability_decision_9_a_member_records_the_topic_joined_and_the_neighbor_up_by_its_five_byte_id()
 {
    let scratch = Scratch::new("events-join").await;
    let (report, mut recorded) = recorded_events();

    let _members = start_members(
        &scratch,
        vec![
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_hosts(&[SECOND_SEED])
                .with_report(report),
            MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC).with_hosts(&[FIRST_SEED]),
        ],
    )
    .await;

    assert_eq!(
        next_recorded(&mut recorded, "gossip.topic_joined").await,
        []
    );
    assert_eq!(
        next_recorded(&mut recorded, "gossip.neighbor_up").await,
        [("peer", short_peer_id(SECOND_SEED))]
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_a_neighbor_down_by_its_five_byte_id() {
    let scratch = Scratch::new("events-down").await;
    let (report, mut recorded) = recorded_events();
    let mut members = start_members(
        &scratch,
        vec![
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_hosts(&[SECOND_SEED])
                .with_report(report),
            MemberPlan::new(SECOND_SEED, SECOND_HANDLE, SECOND_PUBLIC).with_hosts(&[FIRST_SEED]),
        ],
    )
    .await;
    next_recorded(&mut recorded, "gossip.neighbor_up").await;
    let second = members.pop().unwrap();

    drop(second.node);
    second.endpoint.close().await;

    assert_eq!(
        next_recorded(&mut recorded, "gossip.neighbor_down").await,
        [("peer", short_peer_id(SECOND_SEED))]
    );
    drop(scratch);
}

/// A Presence of the seed's card under the handle, carrying the seq.
fn presence_of(seed: u8, handle: &str, seq: u64) -> Payload {
    presence_carrying(
        seed,
        handle,
        seq,
        Addr::from_endpoint_report([MARKER_RELAY.to_owned()], [MARKER_DIRECT_ADDRESS]),
    )
}

/// A Presence of the seed's card under the handle, carrying the seq and the address.
fn presence_carrying(seed: u8, handle: &str, seq: u64, addr: Addr) -> Payload {
    Payload::Presence(Presence {
        v: Version::CURRENT,
        card: card(seed),
        handle: handle.to_owned(),
        public: MARKER_PUBLIC.to_vec(),
        attached: false,
        addr,
        seq,
        mac: ZERO_MAC,
        sig: ZERO_SIGNATURE,
    })
}

#[tokio::test]
async fn observability_decision_9_a_member_records_a_payload_it_accepted_with_its_kind_sender_handle_and_seq()
 {
    let scratch = Scratch::new("events-accepted").await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(report),
        None,
    )
    .await;
    let seq = wall_clock_ms();

    witness
        .broadcast_as(MARKER_SEED, presence_of(MARKER_SEED, MARKER_HANDLE, seq))
        .await;

    assert_eq!(
        next_recorded(&mut recorded, "gossip.payload_accepted").await,
        [
            ("kind", Value::from("presence")),
            ("peer", short_peer_id(MARKER_SEED)),
            ("handle", Value::from(MARKER_HANDLE)),
            ("seq", Value::from(seq)),
        ]
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_a_payload_its_admission_dropped_with_the_rule_that_dropped_it()
 {
    let scratch = Scratch::new("events-dropped").await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_clock(Arc::new(|| FROZEN_CLOCK_MS))
            .with_report(report),
        None,
    )
    .await;
    witness
        .broadcast_as(
            MARKER_SEED,
            presence_of(MARKER_SEED, MARKER_HANDLE, FROZEN_CLOCK_MS),
        )
        .await;
    next_named_from(&mut recorded, "gossip.payload_accepted", MARKER_SEED).await;

    witness
        .broadcast_as(
            MARKER_SEED,
            replayed(presence_of(MARKER_SEED, MARKER_HANDLE, FROZEN_CLOCK_MS)),
        )
        .await;
    let replay = next_named_from(&mut recorded, "gossip.payload_dropped", MARKER_SEED).await;
    witness
        .broadcast_as(
            SECOND_MARKER_SEED,
            presence_of(SECOND_MARKER_SEED, MARKER_HANDLE, STALE_SEQ),
        )
        .await;
    let stale = next_named_from(&mut recorded, "gossip.payload_dropped", SECOND_MARKER_SEED).await;

    assert_eq!(
        replay,
        [
            ("kind", Value::from("presence")),
            ("peer", short_peer_id(MARKER_SEED)),
            ("handle", Value::from(MARKER_HANDLE)),
            ("seq", Value::from(FROZEN_CLOCK_MS)),
            ("rule", Value::from("not_newer_than_held")),
        ]
    );
    assert_eq!(
        stale,
        [
            ("kind", Value::from("presence")),
            ("peer", short_peer_id(SECOND_MARKER_SEED)),
            ("seq", Value::from(STALE_SEQ)),
            ("rule", Value::from("older_than_max_age")),
        ]
    );
    drop(scratch);
}

/// The Presence carrying another public mapping, so its bytes differ from the one first sent and gossip delivers it again.
fn replayed(presence: Payload) -> Payload {
    let Payload::Presence(presence) = presence else {
        panic!("a Presence is replayed");
    };
    Payload::Presence(Presence {
        public: REPLAYED_PUBLIC.to_vec(),
        ..presence
    })
}

/// A Departure of the seed's card carrying the seq.
fn departure_of(seed: u8, seq: u64) -> Payload {
    Payload::Departure(Departure {
        v: Version::CURRENT,
        card: card(seed),
        seq,
        mac: ZERO_MAC,
        sig: ZERO_SIGNATURE,
    })
}

/// An Unreachable report by the reporter's card about the absent card, observed at `ts`.
fn unreachable_of(reporter: u8, absent: u8, ts: u64) -> Payload {
    Payload::Unreachable(Unreachable {
        v: Version::CURRENT,
        card: card(absent),
        reporter: card(reporter),
        ts,
        mac: ZERO_MAC,
        sig: ZERO_SIGNATURE,
    })
}

/// A Presence request by the requester's card carrying the seq.
fn presence_request_of(requester: u8, seq: u64) -> Payload {
    Payload::PresenceRequest(PresenceRequest {
        v: Version::CURRENT,
        requester: card(requester),
        seq,
        mac: ZERO_MAC,
        sig: ZERO_SIGNATURE,
    })
}

/// The fields of a received payload as the events file writes them, the rule last when it was dropped.
fn admission_fields(
    kind: &str,
    signer: u8,
    handle: Option<&str>,
    (stamp_key, stamp): (&'static str, u64),
    rule: Option<&str>,
) -> EventFields {
    let mut fields = vec![("kind", Value::from(kind)), ("peer", short_peer_id(signer))];
    fields.extend(handle.map(|handle| ("handle", Value::from(handle))));
    fields.push((stamp_key, Value::from(stamp)));
    fields.extend(rule.map(|rule| ("rule", Value::from(rule))));
    fields
}

const ACCEPTED: &str = "gossip.payload_accepted";
const DROPPED: &str = "gossip.payload_dropped";
const PIN_SEED: u8 = 84;
const UNKNOWN_SEED: u8 = 83;
const PIN_HANDLE: &str = "pin";
const OLDER_THAN_MAX_AGE: u64 = FROZEN_CLOCK_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS - 1;
const AHEAD_OF_MAX_AGE: u64 = FROZEN_CLOCK_MS + SPEC_ANNOUNCEMENT_MAX_AGE_MS + 1;
const NOW_SECONDS: u64 = FROZEN_CLOCK_MS / MILLIS_PER_SECOND;
const PAST_FUTURE_BOUND_SECONDS: u64 =
    NOW_SECONDS + SPEC_INFRASTRUCTURE_RECORD_FUTURE_BOUND_SECONDS + 1;

/// One step of an admission script: the signer's seed, the payload it broadcasts, and the event the member records for it.
type AdmissionStep = (u8, Payload, &'static str, EventFields);

/// A step whose payload the member takes, recorded with its kind, signer, the signer's handle if held, and its stamp.
fn accepted(
    (signer, payload): (u8, Payload),
    kind: &str,
    handle: Option<&str>,
    stamp: (&'static str, u64),
) -> AdmissionStep {
    (
        signer,
        payload,
        ACCEPTED,
        admission_fields(kind, signer, handle, stamp, None),
    )
}

/// A step whose payload the member drops, recorded as an accepted one is and with the rule last.
fn dropped(
    (signer, payload): (u8, Payload),
    kind: &str,
    handle: Option<&str>,
    stamp: (&'static str, u64),
    rule: &str,
) -> AdmissionStep {
    (
        signer,
        payload,
        DROPPED,
        admission_fields(kind, signer, handle, stamp, Some(rule)),
    )
}

/// Presence and Departure steps reaching the announcement rules: taken, own card, a handle outside the grammar, a Departure, a Presence no newer than it.
fn announcement_steps() -> Vec<AdmissionStep> {
    let now = FROZEN_CLOCK_MS;
    vec![
        accepted(
            (MARKER_SEED, presence_of(MARKER_SEED, MARKER_HANDLE, now)),
            "presence",
            Some(MARKER_HANDLE),
            ("seq", now),
        ),
        dropped(
            (FIRST_SEED, presence_of(FIRST_SEED, FIRST_HANDLE, now)),
            "presence",
            None,
            ("seq", now),
            "from_own_card",
        ),
        dropped(
            (
                MARKER_SEED,
                presence_of(MARKER_SEED, NOT_AN_ANNOUNCED_HANDLE, now + 1),
            ),
            "presence",
            Some(MARKER_HANDLE),
            ("seq", now + 1),
            "handle_outside_grammar",
        ),
        accepted(
            (SECOND_MARKER_SEED, departure_of(SECOND_MARKER_SEED, now)),
            "departure",
            None,
            ("seq", now),
        ),
        dropped(
            (
                SECOND_MARKER_SEED,
                presence_of(SECOND_MARKER_SEED, MARKER_HANDLE, now),
            ),
            "presence",
            None,
            ("seq", now),
            "not_newer_than_departure",
        ),
    ]
}

/// Unreachable steps about other cards: older than held, unknown, gone, too old, then taken.
fn unreachable_steps() -> Vec<AdmissionStep> {
    let now = FROZEN_CLOCK_MS;
    let report = |absent: u8, ts: u64| (WITNESS_SEED, unreachable_of(WITNESS_SEED, absent, ts));
    vec![
        dropped(
            report(MARKER_SEED, now - 1),
            "unreachable",
            None,
            ("ts", now - 1),
            "report_older_than_held_seq",
        ),
        dropped(
            report(UNKNOWN_SEED, now),
            "unreachable",
            None,
            ("ts", now),
            "reported_card_unknown",
        ),
        accepted(
            (MARKER_SEED, departure_of(MARKER_SEED, now + 2)),
            "departure",
            Some(MARKER_HANDLE),
            ("seq", now + 2),
        ),
        dropped(
            report(MARKER_SEED, now + 2),
            "unreachable",
            None,
            ("ts", now + 2),
            "reported_card_gone",
        ),
        dropped(
            report(MARKER_SEED, OLDER_THAN_MAX_AGE),
            "unreachable",
            None,
            ("ts", OLDER_THAN_MAX_AGE),
            "older_than_max_age",
        ),
        accepted(
            (PIN_SEED, presence_of(PIN_SEED, PIN_HANDLE, now)),
            "presence",
            Some(PIN_HANDLE),
            ("seq", now),
        ),
        accepted(report(PIN_SEED, now), "unreachable", None, ("ts", now)),
    ]
}

/// Unreachable steps about this member's own card, which it answers by the rumor rules.
fn rumor_steps() -> Vec<AdmissionStep> {
    let rumor = |ts: u64| (WITNESS_SEED, unreachable_of(WITNESS_SEED, FIRST_SEED, ts));
    vec![
        dropped(
            rumor(OLDER_THAN_MAX_AGE),
            "unreachable",
            None,
            ("ts", OLDER_THAN_MAX_AGE),
            "older_than_max_age",
        ),
        dropped(
            rumor(FROZEN_CLOCK_MS - 1),
            "unreachable",
            None,
            ("ts", FROZEN_CLOCK_MS - 1),
            "report_older_than_last_presence",
        ),
        accepted(
            rumor(AHEAD_OF_MAX_AGE),
            "unreachable",
            None,
            ("ts", AHEAD_OF_MAX_AGE),
        ),
    ]
}

/// Presence request and Infrastructure steps reaching each of their rules, and a request from a card the table holds, named by its handle.
fn request_and_record_steps() -> Vec<AdmissionStep> {
    let now = FROZEN_CLOCK_MS;
    let request = |seq: u64| (WITNESS_SEED, presence_request_of(WITNESS_SEED, seq));
    let record = |ts: u64| (HOST_SEED, infrastructure_of(HOST_SEED, HOST_RELAY, ts));
    vec![
        accepted(request(now), "presence_request", None, ("seq", now)),
        accepted(
            (MARKER_SEED, presence_request_of(MARKER_SEED, now)),
            "presence_request",
            Some(MARKER_HANDLE),
            ("seq", now),
        ),
        dropped(
            request(now - 1),
            "presence_request",
            None,
            ("seq", now - 1),
            "not_above_requester_seq",
        ),
        dropped(
            request(AHEAD_OF_MAX_AGE),
            "presence_request",
            None,
            ("seq", AHEAD_OF_MAX_AGE),
            "ahead_of_max_age",
        ),
        dropped(
            request(OLDER_THAN_MAX_AGE),
            "presence_request",
            None,
            ("seq", OLDER_THAN_MAX_AGE),
            "older_than_max_age",
        ),
        accepted(
            record(NOW_SECONDS),
            "infrastructure",
            None,
            ("ts", NOW_SECONDS),
        ),
        dropped(
            record(NOW_SECONDS - 1),
            "infrastructure",
            None,
            ("ts", NOW_SECONDS - 1),
            "record_not_newer_than_held",
        ),
        dropped(
            record(PAST_FUTURE_BOUND_SECONDS),
            "infrastructure",
            None,
            ("ts", PAST_FUTURE_BOUND_SECONDS),
            "record_past_future_bound",
        ),
    ]
}

#[tokio::test]
async fn observability_decision_9_a_member_records_the_admission_of_every_payload_kind_with_the_rule_of_each_drop()
 {
    let scratch = Scratch::new("events-admission").await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_clock(Arc::new(|| FROZEN_CLOCK_MS))
            .with_report(report),
        None,
    )
    .await;
    let script = [
        announcement_steps(),
        unreachable_steps(),
        rumor_steps(),
        request_and_record_steps(),
    ]
    .concat();

    for (step, (signer, payload, name, fields)) in script.into_iter().enumerate() {
        witness.broadcast_as(signer, payload).await;
        assert_eq!(
            next_recorded_among(&mut recorded, &[ACCEPTED, DROPPED]).await,
            (name, fields),
            "step {step}"
        );
    }
    drop(scratch);
}

const OTHER_NETWORK_SECRET: [u8; KEY_LEN] = [34; KEY_LEN];
const NOT_A_JSON_MAPPING: &[u8] = b"[]";
const PLAIN_HTTP_RELAY: &str = "http://relay.marker.invalid/";
const TRAILING_BYTE: u8 = 0;

/// A Presence of the seed's card under the marker handle whose `public` is the bytes given.
fn presence_with_public(seed: u8, seq: u64, public: &[u8]) -> Payload {
    let Payload::Presence(presence) = presence_of(seed, MARKER_HANDLE, seq) else {
        panic!("a Presence is built");
    };
    Payload::Presence(Presence {
        public: public.to_vec(),
        ..presence
    })
}

/// The payload signed and macked under the node tests' network by the signer the payload names.
fn sealed(signer: u8, payload: Payload) -> Vec<u8> {
    seal_payload(&keys(), &secret_key(signer), payload).unwrap()
}

#[tokio::test]
async fn observability_decision_9_a_member_records_a_payload_the_payload_layer_dropped_with_its_rule_and_what_it_said_of_itself()
 {
    let now = FROZEN_CLOCK_MS;
    let other_network = NetworkKeys::derive(&NetworkSecret::from_bytes(OTHER_NETWORK_SECRET));
    let scratch = Scratch::new("events-refused").await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_clock(Arc::new(|| FROZEN_CLOCK_MS))
            .with_report(report),
        None,
    )
    .await;
    witness
        .broadcast_as(MARKER_SEED, presence_of(MARKER_SEED, MARKER_HANDLE, now))
        .await;
    next_recorded(&mut recorded, ACCEPTED).await;
    let marker = |seq: u64, rule: &str| {
        admission_fields(
            "presence",
            MARKER_SEED,
            Some(MARKER_HANDLE),
            ("seq", seq),
            Some(rule),
        )
    };
    let script = [
        (
            [
                sealed(
                    MARKER_SEED,
                    presence_of(MARKER_SEED, MARKER_HANDLE, now + 1),
                ),
                vec![TRAILING_BYTE],
            ]
            .concat(),
            vec![("rule", Value::from("trailing_bytes"))],
        ),
        (
            seal_payload(
                &other_network,
                &secret_key(MARKER_SEED),
                presence_of(MARKER_SEED, MARKER_HANDLE, now + 2),
            )
            .unwrap(),
            marker(now + 2, "mac_mismatch"),
        ),
        (
            sealed_by_any_key(
                &keys(),
                &secret_key(SECOND_MARKER_SEED),
                presence_of(MARKER_SEED, MARKER_HANDLE, now + 3),
            )
            .unwrap(),
            marker(now + 3, "signature_mismatch"),
        ),
        (
            sealed(
                MARKER_SEED,
                presence_with_public(MARKER_SEED, now + 4, NOT_A_JSON_MAPPING),
            ),
            marker(now + 4, "public_not_admitted"),
        ),
        (
            sealed(
                HOST_SEED,
                infrastructure_of(HOST_SEED, PLAIN_HTTP_RELAY, NOW_SECONDS),
            ),
            admission_fields(
                "infrastructure",
                HOST_SEED,
                None,
                ("ts", NOW_SECONDS),
                Some("host_relay_refused"),
            ),
        ),
    ];

    for (step, (bytes, fields)) in script.into_iter().enumerate() {
        witness.broadcast_bytes(bytes).await;
        assert_eq!(
            next_recorded(&mut recorded, DROPPED).await,
            fields,
            "step {step}"
        );
    }
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_each_payload_it_broadcast_with_its_kind_and_seq()
{
    let scratch = Scratch::new("events-broadcast").await;
    let (report, mut recorded) = recorded_events();
    let (_member, mut witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_clock(Arc::new(|| FROZEN_CLOCK_MS))
            .with_report(report),
        None,
    )
    .await;

    let requested = next_recorded(&mut recorded, "gossip.payload_broadcast").await;
    let broadcast = next_recorded(&mut recorded, "gossip.payload_broadcast").await;
    let heard = witness
        .payload_where(EVENT_WAIT, |payload| {
            matches!(payload, Payload::Presence(_))
        })
        .await;

    assert_eq!(
        requested,
        [
            ("kind", Value::from("presence_request")),
            ("seq", Value::from(FROZEN_CLOCK_MS)),
        ]
    );
    assert_eq!(
        broadcast,
        [
            ("kind", Value::from("presence")),
            ("seq", Value::from(FROZEN_CLOCK_MS)),
        ]
    );
    assert!(
        matches!(
            heard,
            Some(Payload::Presence(Presence {
                seq: FROZEN_CLOCK_MS,
                ..
            }))
        ),
        "the witness heard {heard:?}"
    );
    drop(scratch);
}

const ENTRY_CREATED: &str = "peers.entry_created";
const ENTRY_CHANGED: &str = "peers.entry_changed";
const ENTRY_REMOVED: &str = "peers.entry_removed";
const SPEC_GONE_ENTRY_RETENTION_MS: u64 = 3_600_000;
const SENT_BODY: &str = "ping";

/// The fields of an entry created at the table, as the events file writes them.
fn created_fields(
    seed: u8,
    (requested, assigned): (&str, &str),
    status: &str,
    cause: &str,
) -> EventFields {
    vec![
        ("peer", short_peer_id(seed)),
        ("handle", Value::from(assigned)),
        ("requested", Value::from(requested)),
        ("status", Value::from(status)),
        ("cause", Value::from(cause)),
    ]
}

/// The fields of an entry whose status moved, as the events file writes them.
fn changed_fields(seed: u8, handle: &str, (from, to): (&str, &str), cause: &str) -> EventFields {
    vec![
        ("peer", short_peer_id(seed)),
        ("handle", Value::from(handle)),
        ("from", Value::from(from)),
        ("to", Value::from(to)),
        ("cause", Value::from(cause)),
    ]
}

#[tokio::test]
async fn observability_decision_9_a_member_records_each_entry_its_payloads_create_change_and_expire_with_the_cause()
 {
    let scratch = Scratch::new("events-table").await;
    let (report, mut recorded) = recorded_events();
    let (clock, reading) = settable_clock(FROZEN_CLOCK_MS);
    let (_member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_clock(clock)
            .with_report(report),
        None,
    )
    .await;
    let now = FROZEN_CLOCK_MS;
    let later = now + SPEC_GONE_ENTRY_RETENTION_MS;

    witness
        .broadcast_as(MARKER_SEED, presence_of(MARKER_SEED, MARKER_HANDLE, now))
        .await;
    let first = next_recorded(&mut recorded, ENTRY_CREATED).await;
    witness
        .broadcast_as(
            SECOND_MARKER_SEED,
            presence_of(SECOND_MARKER_SEED, MARKER_HANDLE, now),
        )
        .await;
    let second = next_recorded(&mut recorded, ENTRY_CREATED).await;
    witness
        .broadcast_as(MARKER_SEED, departure_of(MARKER_SEED, now + 1))
        .await;
    let departed = next_recorded(&mut recorded, ENTRY_CHANGED).await;
    witness
        .broadcast_as(
            WITNESS_SEED,
            unreachable_of(WITNESS_SEED, SECOND_MARKER_SEED, now),
        )
        .await;
    let reported = next_recorded(&mut recorded, ENTRY_CHANGED).await;
    reading.store(later, std::sync::atomic::Ordering::SeqCst);
    witness
        .broadcast_as(PIN_SEED, presence_of(PIN_SEED, PIN_HANDLE, later))
        .await;
    let expired = next_recorded(&mut recorded, ENTRY_REMOVED).await;

    assert_eq!(
        first,
        created_fields(
            MARKER_SEED,
            (MARKER_HANDLE, MARKER_HANDLE),
            "online",
            "presence"
        )
    );
    assert_eq!(
        second,
        created_fields(
            SECOND_MARKER_SEED,
            (MARKER_HANDLE, SECOND_EFFECTIVE_MARKER_HANDLE),
            "online",
            "presence"
        )
    );
    assert_eq!(
        departed,
        changed_fields(MARKER_SEED, MARKER_HANDLE, ("online", "gone"), "departure")
    );
    assert_eq!(
        reported,
        changed_fields(
            SECOND_MARKER_SEED,
            SECOND_EFFECTIVE_MARKER_HANDLE,
            ("online", "unreachable"),
            "unreachable"
        )
    );
    assert_eq!(
        expired,
        [
            ("peer", short_peer_id(MARKER_SEED)),
            ("handle", Value::from(MARKER_HANDLE)),
            ("status", Value::from("gone")),
            ("cause", Value::from("expiry")),
        ]
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_its_own_handle_requested_and_assigned() {
    let scratch = Scratch::new("events-own-handle").await;
    let (report, mut recorded) = recorded_events();
    let (first, second) = start_pair(&scratch).await;
    entry_meeting_within(
        &first,
        SECOND_SEED,
        "second member listed by the first",
        |_listed| true,
        SPEC_OBSERVER_WAIT,
    )
    .await
    .unwrap_or_else(|missed| missed.fail_the_test());

    let _third = start_members_reaching(
        &scratch,
        vec![
            MemberPlan::new(THIRD_SEED, FIRST_HANDLE, THIRD_PUBLIC)
                .with_hosts(&[SECOND_SEED])
                .with_report(report),
        ],
        &[(SECOND_SEED, second.endpoint.addr())],
    )
    .await;

    assert_eq!(
        next_recorded(&mut recorded, "peers.own_handle_assigned").await,
        [
            ("requested", Value::from(FIRST_HANDLE)),
            ("handle", Value::from(SECOND_EFFECTIVE_FIRST_HANDLE)),
        ]
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_a_lookup_replaced_by_its_count_of_direct_addresses_and_relay()
 {
    let scratch = Scratch::new("events-lookup").await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_clock(Arc::new(|| FROZEN_CLOCK_MS))
            .with_report(report),
        None,
    )
    .await;

    witness
        .broadcast_as(
            MARKER_SEED,
            presence_carrying(
                MARKER_SEED,
                MARKER_HANDLE,
                FROZEN_CLOCK_MS,
                Addr::from_endpoint_report(
                    [MARKER_RELAY.to_owned()],
                    [MARKER_DIRECT_ADDRESS, SECOND_MARKER_DIRECT_ADDRESS],
                ),
            ),
        )
        .await;
    let relayed = lookup_replaced_of(&mut recorded, MARKER_SEED).await;
    witness
        .broadcast_as(
            SECOND_MARKER_SEED,
            presence_carrying(
                SECOND_MARKER_SEED,
                MARKER_HANDLE,
                FROZEN_CLOCK_MS,
                Addr::from_endpoint_report(std::iter::empty(), [MARKER_DIRECT_ADDRESS]),
            ),
        )
        .await;
    let unrelayed = lookup_replaced_of(&mut recorded, SECOND_MARKER_SEED).await;

    assert_eq!(
        relayed,
        [
            ("peer", short_peer_id(MARKER_SEED)),
            ("handle", Value::from(MARKER_HANDLE)),
            ("direct_addresses", Value::from(2)),
            ("relay", Value::from(true)),
        ]
    );
    assert_eq!(
        unrelayed,
        [
            ("peer", short_peer_id(SECOND_MARKER_SEED)),
            ("handle", Value::from(SECOND_EFFECTIVE_MARKER_HANDLE)),
            ("direct_addresses", Value::from(1)),
            ("relay", Value::from(false)),
        ]
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_entries_its_direct_answers_and_send_outcomes_move_with_the_cause()
 {
    let scratch = Scratch::new("events-direct-table").await;
    let (report, mut recorded) = recorded_events();
    let (report, mut neighbors) = noting_neighbors(report);
    let (member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_clock(Arc::new(|| FROZEN_CLOCK_MS))
            .with_report(report),
        Some(WITNESS_HANDLE),
    )
    .await;
    let introduced = next_recorded(&mut recorded, ENTRY_CREATED).await;
    settled_direct_answers(&member, WITNESS_SEED, DirectContact::Introduce).await;
    witness
        .broadcast_as(
            MARKER_SEED,
            unreachable_of(MARKER_SEED, WITNESS_SEED, FROZEN_CLOCK_MS),
        )
        .await;
    let reported = next_recorded(&mut recorded, ENTRY_CHANGED).await;

    member.node.send(WITNESS_HANDLE, SENT_BODY).await.unwrap();
    let acknowledged = next_recorded(&mut recorded, ENTRY_CHANGED).await;
    neighbors.until_recorded(WITNESS_SEED).await;
    witness.close().await;
    let probed = next_recorded(&mut recorded, ENTRY_CHANGED).await;

    assert_eq!(
        introduced,
        created_fields(
            WITNESS_SEED,
            (WITNESS_HANDLE, WITNESS_HANDLE),
            "online",
            "introduce_ack"
        )
    );
    assert_eq!(
        reported,
        changed_fields(
            WITNESS_SEED,
            WITNESS_HANDLE,
            ("online", "unreachable"),
            "unreachable"
        )
    );
    assert_eq!(
        acknowledged,
        changed_fields(
            WITNESS_SEED,
            WITNESS_HANDLE,
            ("unreachable", "online"),
            "send_acknowledged"
        )
    );
    assert_eq!(
        probed,
        changed_fields(
            WITNESS_SEED,
            WITNESS_HANDLE,
            ("online", "unreachable"),
            "probe_failed"
        )
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_an_entry_a_probe_answer_and_a_failed_send_move_with_the_cause()
 {
    let scratch = Scratch::new("ev-probe-send").await;
    let (report, mut recorded) = recorded_events();
    let (report, mut neighbors) = noting_neighbors(report);
    let (member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_clock(Arc::new(|| FROZEN_CLOCK_MS))
            .with_report(report),
        Some(WITNESS_HANDLE),
    )
    .await;
    next_recorded(&mut recorded, ENTRY_CREATED).await;
    settled_direct_answers(&member, WITNESS_SEED, DirectContact::Introduce).await;
    witness
        .broadcast_as(
            MARKER_SEED,
            unreachable_of(MARKER_SEED, WITNESS_SEED, FROZEN_CLOCK_MS),
        )
        .await;
    next_recorded(&mut recorded, ENTRY_CHANGED).await;

    neighbors.until_recorded(WITNESS_SEED).await;
    let answering = witness.leave_topic();
    let answered = next_recorded(&mut recorded, ENTRY_CHANGED).await;
    answering.shutdown().await.unwrap();
    member
        .node
        .send(WITNESS_HANDLE, SENT_BODY)
        .await
        .unwrap_err();
    let failed = next_recorded(&mut recorded, ENTRY_CHANGED).await;

    assert_eq!(
        answered,
        changed_fields(
            WITNESS_SEED,
            WITNESS_HANDLE,
            ("unreachable", "online"),
            "probe_answered"
        )
    );
    assert_eq!(
        failed,
        changed_fields(
            WITNESS_SEED,
            WITNESS_HANDLE,
            ("online", "unreachable"),
            "send_failed"
        )
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_an_entry_its_failed_introduce_moves_with_the_cause()
 {
    let scratch = Scratch::new("ev-intro-fail").await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
            .with_clock(Arc::new(|| FROZEN_CLOCK_MS))
            .with_report(report),
        None,
    )
    .await;

    witness
        .broadcast_as(
            MARKER_SEED,
            presence_of(MARKER_SEED, MARKER_HANDLE, FROZEN_CLOCK_MS),
        )
        .await;
    next_recorded(&mut recorded, ENTRY_CREATED).await;

    assert_eq!(
        next_recorded(&mut recorded, ENTRY_CHANGED).await,
        changed_fields(
            MARKER_SEED,
            MARKER_HANDLE,
            ("online", "unreachable"),
            "introduce_failed"
        )
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_entries_an_introduce_and_a_message_from_unseen_cards_create_with_the_cause()
 {
    let scratch = Scratch::new("ev-requests").await;
    let (report, mut recorded) = recorded_events();
    let (member, _witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(report),
        None,
    )
    .await;
    let introducer = bind(THIRD_SEED).await;
    introducer.replace_address(member.endpoint.addr());
    let sender = bind(PIN_SEED).await;
    sender.replace_address(member.endpoint.addr());

    DirectClient::new(
        introducer.endpoint().clone(),
        keys(),
        ClientTimeouts::SPEC,
        unrecorded_register(),
    )
    .introduce(
        member.endpoint.id(),
        Introduce {
            v: Version::CURRENT,
            handle: Some(THIRD_HANDLE.to_owned()),
            public: THIRD_PUBLIC.to_vec(),
            attached: true,
            addr: wire_addr(&introducer.endpoint().addr()),
            mac: ZERO_MAC,
        },
    )
    .await
    .unwrap();
    let created_by_introduce = next_recorded(&mut recorded, ENTRY_CREATED).await;
    DirectClient::new(
        sender.endpoint().clone(),
        keys(),
        ClientTimeouts::SPEC,
        unrecorded_register(),
    )
    .send_message(member.endpoint.id(), message_to(FIRST_SEED, PIN_HANDLE))
    .await
    .unwrap();
    let created_by_message = next_recorded(&mut recorded, ENTRY_CREATED).await;

    assert_eq!(
        created_by_introduce,
        created_fields(
            THIRD_SEED,
            (THIRD_HANDLE, THIRD_HANDLE),
            "online",
            "introduce"
        )
    );
    assert_eq!(
        created_by_message,
        created_fields(PIN_SEED, (PIN_HANDLE, PIN_HANDLE), "online", "message")
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_broadcast_the_topic_refuses_is_recorded_as_failed_and_never_as_broadcast()
 {
    let node = bind(FIRST_SEED).await;
    let gossip = node.endpoint().spawn_gossip();
    let topic = crate::net::gossip::NetworkTopic::join(
        &gossip,
        keys(),
        secret_key(HOST_SEED).public(),
        Vec::new(),
        Arc::new(crate::clock::RuntimeClock),
    )
    .await
    .unwrap();
    let payloads_naming_another_card = [
        presence_of(SECOND_SEED, SECOND_HANDLE, FROZEN_CLOCK_MS),
        departure_of(SECOND_SEED, FROZEN_CLOCK_MS),
        unreachable_of(SECOND_SEED, MARKER_SEED, FROZEN_CLOCK_MS),
        infrastructure_of(HOST_SEED, HOST_RELAY, FROZEN_CLOCK_MS),
        super::presence_request(card(SECOND_SEED), FROZEN_CLOCK_MS),
    ];

    for payload in payloads_naming_another_card {
        let (report, mut recorded) = recorded_events();

        super::broadcast_recording_outcome(&topic, &secret_key(FIRST_SEED), payload, &*report)
            .await;
        drop(report);

        let mut names = Vec::new();
        while let Some((name, _fields)) = recorded.recv().await {
            names.push(name);
        }
        assert_eq!(names, ["gossip.broadcast_failed"]);
    }
}

#[tokio::test]
async fn observability_decision_9_an_introduce_to_a_card_that_cannot_be_reached_is_recorded_with_its_peer_and_the_word_unreachable()
 {
    let scratch = Scratch::new("ev-cf-unr").await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(report),
        None,
    )
    .await;

    witness
        .broadcast_as(
            MARKER_SEED,
            presence_of(MARKER_SEED, MARKER_HANDLE, wall_clock_ms()),
        )
        .await;

    assert_eq!(
        next_recorded(&mut recorded, "direct.contact_failed").await,
        [
            ("peer", short_peer_id(MARKER_SEED)),
            ("error", Value::from("unreachable")),
        ]
    );
    drop(scratch);
}

const SCRIPTED_WITNESS_JOIN_WAIT: Duration = Duration::from_secs(8);
const CONTACT_FAILURE_WAIT: Duration = Duration::from_secs(20);
const RELAY_MAP_EDITED: &str = "transport.relay_map_edited";
const HTTPS_DEFAULT_PORT: u16 = 443;

/// A member whose register hands its transport events to the report, with the salt of the tests.
fn member_plan_recording_transport(report: &RunRecord) -> MemberPlan {
    MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
        .with_register(HeldConnections::new(
            Arc::new(RuntimeClock),
            TransportRecord::new(Arc::clone(report), test_salt()),
        ))
        .with_report(Arc::clone(report))
}

/// The fields of the relay map edit a member records once a witnessed host record, newer than any held, reaches it.
async fn relay_map_edit_of_a_host_record(scratch_name: &str) -> EventFields {
    let scratch = Scratch::new(scratch_name).await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) =
        start_witnessed(&scratch, member_plan_recording_transport(&report), None).await;

    witness
        .broadcast_as(
            HOST_SEED,
            infrastructure_of(HOST_SEED, HOST_RELAY, wall_clock_ms() / 1000),
        )
        .await;
    let edit = next_recorded(&mut recorded, RELAY_MAP_EDITED).await;
    drop(scratch);
    edit
}

#[tokio::test]
async fn spec_3_8_a_host_relay_added_to_the_relay_map_is_recorded_with_its_host_and_salted_location()
 {
    let relay = relay_url(HOST_RELAY);
    let expected_hash = LocationHash::of(&test_salt(), relay.as_str()).hex();

    assert_eq!(
        relay_map_edit_of_a_host_record("ev-rm-add").await,
        [
            ("edit", Value::from("added")),
            ("host", short_peer_id(HOST_SEED)),
            ("kind", Value::from("relay")),
            ("port", Value::from(HTTPS_DEFAULT_PORT)),
            ("location", Value::from(expected_hash)),
        ]
    );
}

#[tokio::test]
async fn observability_decision_6_a_relay_map_edit_never_writes_the_relay_in_clear() {
    let edit = relay_map_edit_of_a_host_record("ev-rm-clear").await;

    let written = format!("{edit:?}");
    assert!(
        !written.contains("relay-host.estafette.invalid"),
        "{written}"
    );
}

#[tokio::test]
async fn spec_3_8_a_host_record_whose_relay_the_map_already_holds_records_no_relay_map_edit() {
    let scratch = Scratch::new("ev-rm-held").await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) =
        start_witnessed(&scratch, member_plan_recording_transport(&report), None).await;
    let first_ts = wall_clock_ms() / 1000;

    witness
        .broadcast_as(
            HOST_SEED,
            infrastructure_of(HOST_SEED, HOST_RELAY, first_ts),
        )
        .await;
    witness
        .broadcast_as(
            HOST_SEED,
            infrastructure_of(HOST_SEED, HOST_RELAY, first_ts + 1),
        )
        .await;
    let mut names = Vec::new();
    let mut infrastructure_taken = 0;
    while infrastructure_taken < 2 {
        let (name, fields) = next_recorded_among(
            &mut recorded,
            &[RELAY_MAP_EDITED, "gossip.payload_accepted"],
        )
        .await;
        names.push(name);
        infrastructure_taken += usize::from(
            name == "gossip.payload_accepted"
                && fields.contains(&("kind", Value::from("infrastructure"))),
        );
    }

    assert_eq!(
        names
            .iter()
            .filter(|name| **name == RELAY_MAP_EDITED)
            .count(),
        1,
        "{names:?}"
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_the_lookup_it_points_at_the_address_of_a_host_record()
 {
    let scratch = Scratch::new("ev-lk-rec").await;
    let (report, mut recorded) = recorded_events();
    let (_member, witness) = start_witnessed(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(report),
        None,
    )
    .await;

    witness
        .broadcast_as(
            HOST_SEED,
            infrastructure_of(HOST_SEED, HOST_RELAY, wall_clock_ms() / 1000),
        )
        .await;

    assert_eq!(
        lookup_replaced_of(&mut recorded, HOST_SEED).await,
        [
            ("peer", short_peer_id(HOST_SEED)),
            ("direct_addresses", Value::from(0)),
            ("relay", Value::from(true)),
        ]
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_the_lookup_it_seeds_with_the_relay_of_a_configured_host()
 {
    let scratch = Scratch::new("ev-lk-seed").await;
    let (report, mut recorded) = recorded_events();

    let _members = start_members(
        &scratch,
        vec![
            MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC)
                .with_host_relays(&[(HOST_SEED, HOST_RELAY)])
                .with_report(report),
        ],
    )
    .await;

    assert_eq!(
        lookup_replaced_of(&mut recorded, HOST_SEED).await,
        [
            ("peer", short_peer_id(HOST_SEED)),
            ("direct_addresses", Value::from(0)),
            ("relay", Value::from(true)),
        ]
    );
    drop(scratch);
}
const UNANSWERED_RESET_CODE: u32 = 3;
const OVERSIZED_ACK_BYTES: usize = WIRE_LIMIT_BYTES + 1;
const NOT_AN_ACK: &[u8] = b"not a sealed response";

/// What a scripted peer does with every direct request stream it is handed.
#[derive(Debug, Clone)]
enum DirectAnswer {
    Silence,
    Reply(Vec<u8>),
    Reset,
}

/// What a scripted peer answers to a Probe and to any other direct request.
#[derive(Debug, Clone)]
struct ScriptedAnswers {
    probe: DirectAnswer,
    other: DirectAnswer,
}

impl ScriptedAnswers {
    /// The same answer to every request.
    fn to_every_request(answer: DirectAnswer) -> Self {
        Self {
            probe: answer.clone(),
            other: answer,
        }
    }

    /// The answer to the request, which the handler opens to tell a Probe from the rest.
    fn to(&self, request: &[u8]) -> DirectAnswer {
        match open_request(&keys(), request) {
            Ok(Request::Probe(_)) => self.probe.clone(),
            Ok(Request::Message(_) | Request::Introduce(_)) | Err(_) => self.other.clone(),
        }
    }
}

/// A direct protocol handler that reads each request whole and answers as scripted.
#[derive(Debug)]
struct ScriptedDirectHost(ScriptedAnswers);

impl ProtocolHandler for ScriptedDirectHost {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        while let Ok((mut send, mut recv)) = connection.accept_bi().await {
            let answers = self.0.clone();
            tokio::spawn(async move {
                let request = recv.read_to_end(WIRE_LIMIT_BYTES).await.unwrap();
                match answers.to(&request) {
                    DirectAnswer::Reply(bytes) => {
                        send.write_all(&bytes).await.unwrap();
                        send.finish().unwrap();
                    }
                    DirectAnswer::Reset => {
                        send.reset(VarInt::from_u32(UNANSWERED_RESET_CODE)).unwrap();
                    }
                    DirectAnswer::Silence => std::future::pending().await,
                }
            });
        }
        Ok(())
    }
}

/// A peer that joins the topic through the member and answers its direct requests as scripted, kept alive by its router.
struct ScriptedWitness {
    _router: Router,
    topic: Option<NetworkTopic>,
}

impl ScriptedWitness {
    /// Joins through the endpoint, answering as the scripts say, which are given the address the peer is reached at.
    async fn joined_through(
        through: &BoundEndpoint,
        answers: impl FnOnce(Addr) -> ScriptedAnswers,
    ) -> Self {
        let node_endpoint = bind(WITNESS_SEED).await;
        node_endpoint.replace_address(through.addr());
        let gossip = node_endpoint.endpoint().spawn_gossip();
        let own_address = wire_addr(&node_endpoint.endpoint().addr());
        let router = node_endpoint
            .endpoint()
            .router_builder()
            .accept(DIRECT_ALPN, ScriptedDirectHost(answers(own_address)))
            .accept(GOSSIP_ALPN, gossip.clone())
            .spawn();
        let mut topic = NetworkTopic::join(
            &gossip,
            keys(),
            secret_key(OWNER_SEED).public(),
            vec![through.id()],
            Arc::new(RuntimeClock),
        )
        .await
        .unwrap();
        tokio::time::timeout(SCRIPTED_WITNESS_JOIN_WAIT, async {
            while topic.next_event().await.unwrap().unwrap() != GossipEvent::Joined {}
        })
        .await
        .unwrap();
        Self {
            _router: router,
            topic: Some(topic),
        }
    }

    /// Leaves the topic while still answering direct requests, so its neighbors see it go.
    fn leaves_the_topic(&mut self) {
        self.topic = None;
    }
}

/// The fields of the `direct.contact_failed` a member records for the Introduce it sends a peer that answers as scripted.
async fn contact_failed_by_introduce_answered(
    scratch_name: &str,
    answer: DirectAnswer,
) -> EventFields {
    let scratch = Scratch::new(scratch_name).await;
    let (report, mut recorded) = recorded_events();
    let (endpoint, starting) = spawn_member(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(report),
    )
    .await;
    let _witness = ScriptedWitness::joined_through(&endpoint, |_address| {
        ScriptedAnswers::to_every_request(answer)
    })
    .await;
    let _member = starting.await.unwrap();

    let fields =
        next_recorded_within(&mut recorded, "direct.contact_failed", CONTACT_FAILURE_WAIT).await;
    drop(scratch);
    fields
}

fn contact_failed_fields(error_word: &str) -> EventFields {
    vec![
        ("peer", short_peer_id(WITNESS_SEED)),
        ("error", Value::from(error_word)),
    ]
}

const NONCE_THE_MEMBER_DID_NOT_SEND: [u8; 16] = [0; 16];

#[tokio::test]
async fn observability_decision_9_a_probe_answered_with_another_nonce_is_recorded_with_the_word_probe_nonce_mismatch()
 {
    let scratch = Scratch::new("ev-cf-prb").await;
    let (report, mut recorded) = recorded_events();
    let (endpoint, starting) = spawn_member(
        &scratch,
        MemberPlan::new(FIRST_SEED, FIRST_HANDLE, FIRST_PUBLIC).with_report(report),
    )
    .await;
    let mut witness = ScriptedWitness::joined_through(&endpoint, |address| {
        let introduce_ack = seal_response(
            &keys(),
            Response::IntroduceAck(IntroduceAck {
                v: Version::CURRENT,
                handle: Some(WITNESS_HANDLE.to_owned()),
                public: WITNESS_PUBLIC.to_vec(),
                attached: false,
                addr: address,
                mac: ZERO_MAC,
            }),
        )
        .unwrap();
        let probe_ack = seal_response(
            &keys(),
            Response::ProbeAck(ProbeAck {
                v: Version::CURRENT,
                nonce: NONCE_THE_MEMBER_DID_NOT_SEND,
                attached: false,
                mac: ZERO_MAC,
            }),
        )
        .unwrap();
        ScriptedAnswers {
            other: DirectAnswer::Reply(introduce_ack),
            probe: DirectAnswer::Reply(probe_ack),
        }
    })
    .await;
    let _member = starting.await.unwrap();
    next_recorded_within(&mut recorded, "peers.entry_created", CONTACT_FAILURE_WAIT).await;

    witness.leaves_the_topic();

    assert_eq!(
        next_recorded_within(&mut recorded, "direct.contact_failed", CONTACT_FAILURE_WAIT).await,
        contact_failed_fields("probe_nonce_mismatch")
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_an_introduce_the_peer_resets_is_recorded_with_the_word_refused() {
    assert_eq!(
        contact_failed_by_introduce_answered("ev-cf-ref", DirectAnswer::Reset).await,
        contact_failed_fields("refused")
    );
}

#[tokio::test]
async fn observability_decision_9_an_introduce_the_peer_leaves_unanswered_is_recorded_with_the_word_timeout()
 {
    assert_eq!(
        contact_failed_by_introduce_answered("ev-cf-tim", DirectAnswer::Silence).await,
        contact_failed_fields("timeout")
    );
}

#[tokio::test]
async fn observability_decision_9_an_introduce_answered_with_bytes_that_do_not_open_is_recorded_with_the_word_invalid_ack()
 {
    assert_eq!(
        contact_failed_by_introduce_answered("ev-cf-inv", DirectAnswer::Reply(NOT_AN_ACK.to_vec()))
            .await,
        contact_failed_fields("invalid_ack")
    );
}

#[tokio::test]
async fn observability_decision_9_an_introduce_answered_past_the_wire_limit_is_recorded_with_the_word_oversized_ack()
 {
    assert_eq!(
        contact_failed_by_introduce_answered(
            "ev-cf-ovr",
            DirectAnswer::Reply(vec![0; OVERSIZED_ACK_BYTES])
        )
        .await,
        contact_failed_fields("oversized_ack")
    );
}

#[tokio::test]
async fn observability_decision_9_an_introduce_answered_with_a_probe_ack_is_recorded_with_the_word_unexpected_ack()
 {
    let probe_ack = seal_response(
        &keys(),
        Response::ProbeAck(ProbeAck {
            v: Version::CURRENT,
            nonce: [0; 16],
            attached: false,
            mac: ZERO_MAC,
        }),
    )
    .unwrap();

    assert_eq!(
        contact_failed_by_introduce_answered("ev-cf-unx", DirectAnswer::Reply(probe_ack)).await,
        contact_failed_fields("unexpected_ack")
    );
}
