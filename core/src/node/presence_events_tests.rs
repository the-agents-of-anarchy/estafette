use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use super::tests::{
    FIRST_HANDLE, FIRST_PUBLIC, FIRST_SEED, FROZEN_CLOCK_MS, HOST_RELAY, HOST_SEED, MARKER_SEED,
    MemberPlan, SECOND_HANDLE, SECOND_PUBLIC, SECOND_SEED, Scratch, WITNESS_SEED, card,
    infrastructure_of, start_members, start_witnessed, wall_clock_ms,
};
use crate::event::{Event, EventFields, RunRecord};
use crate::payload::{ZERO_MAC, ZERO_SIGNATURE};
use crate::wire::{Addr, Departure, Payload, Presence, PresenceRequest, Unreachable, Version};

const EVENT_WAIT: Duration = Duration::from_secs(10);
const SPEC_SHORT_ID_BYTES: usize = 5;
const SPEC_ANNOUNCEMENT_MAX_AGE_MS: u64 = 300_000;
const STALE_SEQ: u64 = FROZEN_CLOCK_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS - 1;
const SECOND_MARKER_SEED: u8 = 82;
const MARKER_HANDLE: &str = "mark";
const MARKER_PUBLIC: &[u8] = br#"{"role":"marker"}"#;
const REPLAYED_PUBLIC: &[u8] = br#"{"role":"replayed"}"#;
const MARKER_RELAY: &str = "https://relay.marker.invalid/";
const MARKER_DIRECT_ADDRESS: SocketAddr = SocketAddr::new(
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7)),
    4433,
);

/// The events a member records, each as its name and the fields the events file writes for it, in the order recorded.
struct RecordedEvents(mpsc::UnboundedReceiver<(&'static str, EventFields)>);

impl RecordedEvents {
    /// The fields of the next event of that name, failing with the name when none is recorded within the wait.
    async fn next_named(&mut self, name: &str) -> EventFields {
        self.next_of(&[name]).await.1
    }

    /// The name and fields of the next event of one of those names, failing with the names when none is recorded within the wait.
    async fn next_of(&mut self, names: &[&str]) -> (&'static str, EventFields) {
        tokio::time::timeout(EVENT_WAIT, async {
            loop {
                let (recorded, fields) = self.0.recv().await.unwrap();
                if names.contains(&recorded) {
                    return (recorded, fields);
                }
            }
        })
        .await
        .unwrap_or_else(|_elapsed| panic!("no event of {names:?} within {EVENT_WAIT:?}"))
    }
}

/// A run record that hands every event to the test, and the test's end of it.
fn recording() -> (RunRecord, RecordedEvents) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let report: RunRecord = Arc::new(move |event: Event| {
        drop(sender.send((event.name(), event.fields())));
    });
    (report, RecordedEvents(receiver))
}

/// The five-byte id of the seed's card as the events file writes it.
fn short_id(seed: u8) -> Value {
    Value::from(data_encoding::HEXLOWER.encode(card(seed).get(..SPEC_SHORT_ID_BYTES).unwrap()))
}

#[tokio::test]
async fn observability_decision_9_a_member_records_the_topic_joined_and_the_neighbor_up_by_its_five_byte_id()
 {
    let scratch = Scratch::new("events-join").await;
    let (report, mut recorded) = recording();

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

    assert_eq!(recorded.next_named("gossip.topic_joined").await, []);
    assert_eq!(
        recorded.next_named("gossip.neighbor_up").await,
        [("peer", short_id(SECOND_SEED))]
    );
    drop(scratch);
}

#[tokio::test]
async fn observability_decision_9_a_member_records_a_neighbor_down_by_its_five_byte_id() {
    let scratch = Scratch::new("events-down").await;
    let (report, mut recorded) = recording();
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
    recorded.next_named("gossip.neighbor_up").await;
    let second = members.pop().unwrap();

    drop(second.node);
    second.endpoint.close().await;

    assert_eq!(
        recorded.next_named("gossip.neighbor_down").await,
        [("peer", short_id(SECOND_SEED))]
    );
    drop(scratch);
}

/// A Presence of the seed's card under the handle, carrying the seq.
fn presence_of(seed: u8, handle: &str, seq: u64) -> Payload {
    Payload::Presence(Presence {
        v: Version::CURRENT,
        card: card(seed),
        handle: handle.to_owned(),
        public: MARKER_PUBLIC.to_vec(),
        attached: false,
        addr: Addr::from_endpoint_report([MARKER_RELAY.to_owned()], [MARKER_DIRECT_ADDRESS]),
        seq,
        mac: ZERO_MAC,
        sig: ZERO_SIGNATURE,
    })
}

#[tokio::test]
async fn observability_decision_9_a_member_records_a_payload_it_accepted_with_its_kind_sender_handle_and_seq()
 {
    let scratch = Scratch::new("events-accepted").await;
    let (report, mut recorded) = recording();
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
        recorded.next_named("gossip.payload_accepted").await,
        [
            ("kind", Value::from("presence")),
            ("peer", short_id(MARKER_SEED)),
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
    let (report, mut recorded) = recording();
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
    recorded.next_named("gossip.payload_accepted").await;

    witness
        .broadcast_as(
            MARKER_SEED,
            replayed(presence_of(MARKER_SEED, MARKER_HANDLE, FROZEN_CLOCK_MS)),
        )
        .await;
    let replay = recorded.next_named("gossip.payload_dropped").await;
    witness
        .broadcast_as(
            SECOND_MARKER_SEED,
            presence_of(SECOND_MARKER_SEED, MARKER_HANDLE, STALE_SEQ),
        )
        .await;
    let stale = recorded.next_named("gossip.payload_dropped").await;

    assert_eq!(
        replay,
        [
            ("kind", Value::from("presence")),
            ("peer", short_id(MARKER_SEED)),
            ("handle", Value::from(MARKER_HANDLE)),
            ("seq", Value::from(FROZEN_CLOCK_MS)),
            ("rule", Value::from("not_newer_than_held")),
        ]
    );
    assert_eq!(
        stale,
        [
            ("kind", Value::from("presence")),
            ("peer", short_id(SECOND_MARKER_SEED)),
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
    let mut fields = vec![("kind", Value::from(kind)), ("peer", short_id(signer))];
    fields.extend(handle.map(|handle| ("handle", Value::from(handle))));
    fields.push((stamp_key, Value::from(stamp)));
    fields.extend(rule.map(|rule| ("rule", Value::from(rule))));
    fields
}

const ACCEPTED: &str = "gossip.payload_accepted";
const DROPPED: &str = "gossip.payload_dropped";
const MILLIS_PER_SECOND: u64 = 1000;
const SPEC_INFRASTRUCTURE_FUTURE_BOUND_SECONDS: u64 = 300;
const PIN_SEED: u8 = 84;
const UNKNOWN_SEED: u8 = 83;
const PIN_HANDLE: &str = "pin";
const NOT_AN_ANNOUNCED_HANDLE: &str = "Bob";
const OLDER_THAN_MAX_AGE: u64 = FROZEN_CLOCK_MS - SPEC_ANNOUNCEMENT_MAX_AGE_MS - 1;
const AHEAD_OF_MAX_AGE: u64 = FROZEN_CLOCK_MS + SPEC_ANNOUNCEMENT_MAX_AGE_MS + 1;
const NOW_SECONDS: u64 = FROZEN_CLOCK_MS / MILLIS_PER_SECOND;
const PAST_FUTURE_BOUND_SECONDS: u64 = NOW_SECONDS + SPEC_INFRASTRUCTURE_FUTURE_BOUND_SECONDS + 1;

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

/// Presence request and Infrastructure steps reaching each of their rules.
fn request_and_record_steps() -> Vec<AdmissionStep> {
    let now = FROZEN_CLOCK_MS;
    let request = |seq: u64| (WITNESS_SEED, presence_request_of(WITNESS_SEED, seq));
    let record = |ts: u64| (HOST_SEED, infrastructure_of(HOST_SEED, HOST_RELAY, ts));
    vec![
        accepted(request(now), "presence_request", None, ("seq", now)),
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
    let (report, mut recorded) = recording();
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
            recorded.next_of(&[ACCEPTED, DROPPED]).await,
            (name, fields),
            "step {step}"
        );
    }
    drop(scratch);
}
