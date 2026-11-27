//! The bounded waits a test of an in-process node uses in place of a sleep, on the events its run record was handed and on a condition it can look at, and the field those events name a card by.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh::{PublicKey, SecretKey};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::crypto::KEY_LEN;
use crate::event::{Event, EventFields, RunRecord};
use crate::wire::Nonce;

/// The longest a test waits for an event when it names no bound of its own.
const RECORDED_EVENT_WITHIN: Duration = Duration::from_secs(10);

/// The bytes of a card's key that the events name it by, as spec observability decision 6 states.
const SPEC_SHORT_ID_BYTES: usize = 5;

/// The prefix of the name of every gossip event.
const GOSSIP_EVENT_PREFIX: &str = "gossip.";

/// The probe nonce every in-process node of a test fills its probes with.
pub const PROBE_NONCE: Nonce = *b"0123456789abcdef";

/// The secret key a test derives from a seed, every byte of it the seed.
pub fn secret_key(seed: u8) -> SecretKey {
    SecretKey::from_bytes(&[seed; KEY_LEN])
}

/// The wall clock's reading, in milliseconds since the Unix epoch.
pub fn wall_clock_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// The name and fields of one event a run record was handed.
pub type RecordedEvent = (&'static str, EventFields);

/// A run record that hands the name and fields of each event to the receiver.
pub fn recorded_events() -> (RunRecord, UnboundedReceiver<RecordedEvent>) {
    let (sender, receiver) = unbounded_channel();
    let record: RunRecord = Arc::new(move |event: Event| {
        drop(sender.send((event.name(), event.fields())));
    });
    (record, receiver)
}

/// Waits for the next recorded event of the name, within the bound, failing with the name and the events seen.
pub async fn next_recorded(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    name: &str,
) -> EventFields {
    next_recorded_within(receiver, name, RECORDED_EVENT_WITHIN).await
}

/// Waits for the next recorded event of the name, within the given bound, failing with the name and the events seen.
pub async fn next_recorded_within(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    name: &str,
    within: Duration,
) -> EventFields {
    next_recorded_meeting(receiver, name, |_fields| true, within).await
}

/// Waits for the next recorded event of any of the names, within the bound, failing with the names and the events seen.
pub async fn next_recorded_among(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    names: &[&str],
) -> RecordedEvent {
    recorded_until_among(receiver, names, |_fields| true, RECORDED_EVENT_WITHIN)
        .await
        .1
}

/// The fields of the next recorded event of the name whose fields meet the condition, consuming every event before it, failing with the name and the events seen when none comes within the bound.
pub async fn next_recorded_meeting(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    name: &str,
    meets: impl Fn(&EventFields) -> bool + Send + Sync,
    within: Duration,
) -> EventFields {
    recorded_until(receiver, name, meets, within).await.1
}

/// Every event recorded before the next one of the name whose fields meet the condition, in order, and that one's fields, failing with the name and the events seen when none comes within the bound.
pub async fn recorded_until(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    name: &str,
    meets: impl Fn(&EventFields) -> bool + Send + Sync,
    within: Duration,
) -> (Vec<RecordedEvent>, EventFields) {
    let (before, (_name, fields)) = recorded_until_among(receiver, &[name], meets, within).await;
    (before, fields)
}

async fn recorded_until_among(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    names: &[&str],
    meets: impl Fn(&EventFields) -> bool + Send + Sync,
    within: Duration,
) -> (Vec<RecordedEvent>, RecordedEvent) {
    let awaited = names.join(" or ");
    let mut before = Vec::new();
    let found = tokio::time::timeout(within, async {
        loop {
            let (recorded_name, fields) = receiver.recv().await?;
            if names.contains(&recorded_name) && meets(&fields) {
                return Some((recorded_name, fields));
            }
            before.push((recorded_name, fields));
        }
    })
    .await
    .unwrap_or_else(|_elapsed| {
        panic!(
            "no {awaited} meeting the condition within {within:?}; seen {:?}",
            names_of(&before)
        )
    });
    let Some(event) = found else {
        panic!(
            "the run record closed before any {awaited} meeting the condition; seen {:?}",
            names_of(&before)
        )
    };
    (before, event)
}

fn names_of(events: &[RecordedEvent]) -> Vec<&'static str> {
    events.iter().map(|(name, _fields)| *name).collect()
}

/// Every gossip event recorded, in order, until the gathered events meet the condition, failing with what was gathered when they do not within the bound.
pub async fn gossip_events_recorded_until(
    receiver: &mut UnboundedReceiver<RecordedEvent>,
    complete: impl Fn(&[RecordedEvent]) -> bool + Send + Sync,
    within: Duration,
) -> Vec<RecordedEvent> {
    let mut gossip = Vec::new();
    let arrived = tokio::time::timeout(within, async {
        while !complete(&gossip) {
            let (name, fields) = receiver.recv().await.unwrap_or_else(|| {
                panic!(
                    "the run record closed with {gossip:?} gathered before the gossip events were complete"
                )
            });
            if name.starts_with(GOSSIP_EVENT_PREFIX) {
                gossip.push((name, fields));
            }
        }
    })
    .await;
    assert!(
        arrived.is_ok(),
        "the gossip events were not complete within {within:?}; gathered {gossip:?}"
    );
    gossip
}

/// The field the events name a card by: the first bytes of its key in lowercase hex.
pub fn short_id_field(card: &PublicKey) -> serde_json::Value {
    serde_json::Value::from(data_encoding::HEXLOWER.encode(&card.as_bytes()[..SPEC_SHORT_ID_BYTES]))
}

/// The answer of the request, failing with the name of what was awaited when it does not come within the bound.
pub async fn answered_within<Answer>(
    within: Duration,
    awaited: &str,
    request: impl Future<Output = Answer>,
) -> Answer {
    tokio::time::timeout(within, request)
        .await
        .unwrap_or_else(|_elapsed| panic!("no {awaited} within {within:?}"))
}

/// What a wait that ran out of time was waiting for and for how long.
#[derive(Debug, PartialEq, Eq)]
pub struct WaitMissed {
    awaited: String,
    within: Duration,
}

impl std::fmt::Display for WaitMissed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "no {} within {:?}", self.awaited, self.within)
    }
}

impl WaitMissed {
    /// The miss of a wait for what was awaited that ran out of its bound.
    pub fn new(awaited: &str, within: Duration) -> Self {
        Self {
            awaited: awaited.to_owned(),
            within,
        }
    }

    /// Fails the test with the name of what was awaited and the bound.
    pub fn fail_the_test<Found>(self) -> Found {
        panic!("{self}")
    }
}

async fn looked_for_within<Found>(
    within: Duration,
    mut look: impl AsyncFnMut() -> Option<Found>,
) -> Option<Found> {
    tokio::time::timeout(within, async {
        loop {
            if let Some(found) = look().await {
                return found;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .ok()
}

/// What the look finds, looked for again each time the runtime has run its other tasks, or the awaited condition and the bound when the bound runs out first.
pub async fn found_within<Found>(
    within: Duration,
    awaited: &str,
    look: impl AsyncFnMut() -> Option<Found>,
) -> Result<Found, WaitMissed> {
    looked_for_within(within, look)
        .await
        .ok_or_else(|| WaitMissed::new(awaited, within))
}

/// What the look finds within the bound, for a test that asks for nothing to be found and fails on `Some`.
#[must_use]
pub async fn unexpectedly_found_within<Found>(
    within: Duration,
    look: impl AsyncFnMut() -> Option<Found>,
) -> Option<Found> {
    looked_for_within(within, look).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{GossipEvent, ProcessEvent, ShortId};

    const HEX_CHARS_PER_BYTE: usize = 2;

    #[tokio::test]
    #[should_panic(
        expected = "the run record closed before any process.started meeting the condition; seen []"
    )]
    async fn a_closed_run_record_fails_naming_what_was_awaited() {
        let (record, mut receiver) = recorded_events();
        drop(record);

        next_recorded(&mut receiver, "process.started").await;
    }

    #[test]
    fn the_events_name_a_card_by_the_first_five_bytes_of_its_key_in_lowercase_hex() {
        let card = iroh::SecretKey::from_bytes(&[7; 32]).public();

        let field = short_id_field(&card);

        let key_hex = data_encoding::HEXLOWER.encode(card.as_bytes());
        let expected: String = key_hex
            .chars()
            .take(SPEC_SHORT_ID_BYTES * HEX_CHARS_PER_BYTE)
            .collect();
        assert_eq!(field, serde_json::Value::from(expected));
    }

    #[tokio::test]
    async fn gossip_events_are_gathered_in_order_without_the_events_of_other_kinds_until_complete()
    {
        let (record, mut receiver) = recorded_events();
        record(GossipEvent::TopicJoined.into());
        record(ProcessEvent::ClockBeforeUnixEpoch.into());
        record(
            GossipEvent::NeighborUp(ShortId::from(&crate::wire::PublicKey::from_bytes([1; 32])))
                .into(),
        );
        record(GossipEvent::TopicJoined.into());

        let gossip = gossip_events_recorded_until(
            &mut receiver,
            |gathered| gathered.len() == 2,
            RECORDED_EVENT_WITHIN,
        )
        .await;

        assert_eq!(
            gossip
                .iter()
                .map(|(name, _fields)| *name)
                .collect::<Vec<_>>(),
            ["gossip.topic_joined", "gossip.neighbor_up"]
        );
    }

    #[tokio::test]
    #[should_panic(
        expected = "the run record closed with [(\"gossip.topic_joined\", [])] gathered before the gossip events were complete"
    )]
    async fn a_closed_run_record_fails_naming_the_gossip_events_gathered() {
        let (record, mut receiver) = recorded_events();
        record(GossipEvent::TopicJoined.into());
        drop(record);

        gossip_events_recorded_until(
            &mut receiver,
            |gathered| gathered.len() == 2,
            RECORDED_EVENT_WITHIN,
        )
        .await;
    }

    #[tokio::test]
    async fn the_next_event_of_any_of_the_names_is_returned_with_its_own_name() {
        let (record, mut receiver) = recorded_events();
        record(GossipEvent::TopicJoined.into());
        record(ProcessEvent::ClockBeforeUnixEpoch.into());
        drop(record);

        let (name, _fields) = next_recorded_among(
            &mut receiver,
            &["gossip.neighbor_down", "process.clock_before_unix_epoch"],
        )
        .await;

        assert_eq!(name, "process.clock_before_unix_epoch");
    }

    #[tokio::test]
    #[should_panic(
        expected = "the run record closed before any gossip.neighbor_down or gossip.neighbor_up meeting the condition; seen [\"gossip.topic_joined\"]"
    )]
    async fn a_closed_run_record_fails_naming_every_name_awaited() {
        let (record, mut receiver) = recorded_events();
        record(GossipEvent::TopicJoined.into());
        drop(record);

        next_recorded_among(
            &mut receiver,
            &["gossip.neighbor_down", "gossip.neighbor_up"],
        )
        .await;
    }

    const MISSED_WAIT_BOUND: Duration = Duration::from_millis(50);

    #[tokio::test(start_paused = true)]
    async fn a_condition_that_never_holds_fails_naming_it_and_its_bound() {
        const AWAITED: &str = "card listed";

        let (missed, ()) = tokio::join!(
            found_within(MISSED_WAIT_BOUND, AWAITED, async || None::<()>),
            tokio::time::advance(MISSED_WAIT_BOUND),
        );

        assert_eq!(
            missed.unwrap_err().to_string(),
            "no card listed within 50ms"
        );
    }

    #[tokio::test]
    async fn a_condition_that_holds_after_other_tasks_ran_returns_what_was_found() {
        let mut looks_made = 0_u8;

        let found = found_within(RECORDED_EVENT_WITHIN, "third look", async || {
            looks_made += 1;
            (looks_made == 3).then_some(looks_made)
        })
        .await;

        assert_eq!(found, Ok(3));
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "no card listed within 50ms")]
    async fn a_missed_wait_fails_the_test_with_its_text() {
        let (missed, ()) = tokio::join!(
            found_within(MISSED_WAIT_BOUND, "card listed", async || None::<()>),
            tokio::time::advance(MISSED_WAIT_BOUND),
        );

        missed.unwrap_or_else(WaitMissed::fail_the_test);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_found_unexpectedly_when_the_condition_never_holds() {
        let (found, ()) = tokio::join!(
            unexpectedly_found_within(MISSED_WAIT_BOUND, async || None::<()>),
            tokio::time::advance(MISSED_WAIT_BOUND),
        );

        assert_eq!(found, None);
    }
}
