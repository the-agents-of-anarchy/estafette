use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use super::tests::{
    FIRST_HANDLE, FIRST_PUBLIC, FIRST_SEED, MemberPlan, SECOND_HANDLE, SECOND_PUBLIC, SECOND_SEED,
    Scratch, card, start_members,
};
use crate::event::{Event, EventFields, RunRecord};

const EVENT_WAIT: Duration = Duration::from_secs(10);
const SPEC_SHORT_ID_BYTES: usize = 5;

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
