use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use super::tests::{
    FIRST_HANDLE, FIRST_PUBLIC, FIRST_SEED, MARKER_SEED, MemberPlan, SECOND_HANDLE, SECOND_PUBLIC,
    SECOND_SEED, Scratch, card, start_members, start_witnessed, wall_clock_ms,
};
use crate::event::{Event, EventFields, RunRecord};
use crate::payload::{ZERO_MAC, ZERO_SIGNATURE};
use crate::wire::{Addr, Payload, Presence, Version};

const EVENT_WAIT: Duration = Duration::from_secs(10);
const SPEC_SHORT_ID_BYTES: usize = 5;
const MARKER_HANDLE: &str = "mark";
const MARKER_PUBLIC: &[u8] = br#"{"role":"marker"}"#;
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
        tokio::time::timeout(EVENT_WAIT, async {
            loop {
                let (recorded, fields) = self.0.recv().await.unwrap();
                if recorded == name {
                    return fields;
                }
            }
        })
        .await
        .unwrap_or_else(|_elapsed| panic!("no {name} event within {EVENT_WAIT:?}"))
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
