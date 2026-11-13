use serde_json::Value;

use super::gossip::ShortId;
use super::{AreaEvent, EventFields};
use crate::handle::{AnnouncedHandle, LocalHandle};
use crate::peers::{ContactFailed, ContactSucceeded, Status, TableCause, TableChange};
use crate::wire;

const PEER_FIELD: &str = "peer";
const HANDLE_FIELD: &str = "handle";
const REQUESTED_FIELD: &str = "requested";
const STATUS_FIELD: &str = "status";
const FROM_FIELD: &str = "from";
const TO_FIELD: &str = "to";
const CAUSE_FIELD: &str = "cause";
const DIRECT_ADDRESSES_FIELD: &str = "direct_addresses";
const RELAY_FIELD: &str = "relay";
const EXPIRY_CAUSE_WORD: &str = "expiry";

super::event_names! {
    /// The names of the peer table events.
    PeerTableEventName {
        EntryCreated => "peers.entry_created",
        EntryChanged => "peers.entry_changed",
        EntryRemoved => "peers.entry_removed",
        OwnHandleAssigned => "peers.own_handle_assigned",
        LookupReplaced => "peers.lookup_replaced",
    }
}

/// An entry of the table as the events file names it: its card's five-byte id and the handle this node names it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedEntry {
    card: ShortId,
    handle: LocalHandle,
}

impl NamedEntry {
    /// The entry's fields in the events file: its peer and its handle.
    fn fields(&self) -> EventFields {
        vec![
            (PEER_FIELD, self.card.field()),
            (HANDLE_FIELD, Value::from(self.handle.as_str())),
        ]
    }
}

/// What a node records of its peer table's entries, its own handle, and the lookup entries the table's effects replace.
pub enum PeerTableEvent {
    /// An entry was created under the handle assigned for the one its card requested.
    EntryCreated {
        entry: NamedEntry,
        requested: AnnouncedHandle,
        status: Status,
        cause: TableCause,
    },
    /// An entry's status moved.
    EntryChanged {
        entry: NamedEntry,
        from: Status,
        to: Status,
        cause: TableCause,
    },
    /// An entry was removed once its time ran out.
    EntryRemoved { entry: NamedEntry, status: Status },
    /// This node's own handle was assigned for the one it requested.
    OwnHandleAssigned {
        requested: AnnouncedHandle,
        handle: LocalHandle,
    },
    /// A card's lookup entry was replaced and now holds this many direct addresses and a relay or none.
    LookupReplaced {
        card: ShortId,
        handle: Option<LocalHandle>,
        direct_addresses: usize,
        relay: bool,
    },
}

impl PeerTableEvent {
    /// The event of one change the table made.
    #[must_use]
    pub fn of_change(change: TableChange) -> Self {
        match change {
            TableChange::Created {
                card,
                requested,
                handle,
                status,
                cause,
            } => Self::EntryCreated {
                entry: named(&card, handle),
                requested,
                status,
                cause,
            },
            TableChange::Moved {
                card,
                handle,
                from,
                to,
                cause,
            } => Self::EntryChanged {
                entry: named(&card, handle),
                from,
                to,
                cause,
            },
            TableChange::Removed {
                card,
                handle,
                status,
            } => Self::EntryRemoved {
                entry: named(&card, handle),
                status,
            },
            TableChange::OwnHandleAssigned { requested, handle } => {
                Self::OwnHandleAssigned { requested, handle }
            }
        }
    }

    /// The event of a replaced lookup entry, naming the card, the handle the table holds for it, and what the entry now holds.
    #[must_use]
    pub fn lookup_replaced(
        card: &wire::PublicKey,
        handle: Option<LocalHandle>,
        direct_addresses: usize,
        relay: bool,
    ) -> Self {
        Self::LookupReplaced {
            card: ShortId::from(card),
            handle,
            direct_addresses,
            relay,
        }
    }
}

/// The entry of `card` named by `handle`.
fn named(card: &wire::PublicKey, handle: LocalHandle) -> NamedEntry {
    NamedEntry {
        card: ShortId::from(card),
        handle,
    }
}

/// The name the events file gives a cause.
const fn cause_name(cause: TableCause) -> &'static str {
    match cause {
        TableCause::Presence => "presence",
        TableCause::Departure => "departure",
        TableCause::Unreachable => "unreachable",
        TableCause::Introduce => "introduce",
        TableCause::IntroduceAck => "introduce_ack",
        TableCause::ContactFailed(ContactFailed::Introduce) => "introduce_failed",
        TableCause::Message => "message",
        TableCause::ContactSucceeded(ContactSucceeded::Probe) => "probe_answered",
        TableCause::ContactFailed(ContactFailed::Probe) => "probe_failed",
        TableCause::ContactSucceeded(ContactSucceeded::Send) => "send_acknowledged",
        TableCause::ContactFailed(ContactFailed::Send) => "send_failed",
    }
}

impl AreaEvent for PeerTableEvent {
    type Name = PeerTableEventName;

    fn name(&self) -> PeerTableEventName {
        match self {
            Self::EntryCreated { .. } => PeerTableEventName::EntryCreated,
            Self::EntryChanged { .. } => PeerTableEventName::EntryChanged,
            Self::EntryRemoved { .. } => PeerTableEventName::EntryRemoved,
            Self::OwnHandleAssigned { .. } => PeerTableEventName::OwnHandleAssigned,
            Self::LookupReplaced { .. } => PeerTableEventName::LookupReplaced,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        match self {
            Self::EntryCreated { .. }
            | Self::EntryChanged { .. }
            | Self::EntryRemoved { .. }
            | Self::OwnHandleAssigned { .. }
            | Self::LookupReplaced { .. } => None,
        }
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::EntryCreated {
                entry,
                requested,
                status,
                cause,
            } => {
                let mut fields = entry.fields();
                fields.extend([
                    (REQUESTED_FIELD, Value::from(requested.as_str())),
                    (STATUS_FIELD, Value::from(status.wire_word())),
                    (CAUSE_FIELD, Value::from(cause_name(*cause))),
                ]);
                fields
            }
            Self::EntryChanged {
                entry,
                from,
                to,
                cause,
            } => {
                let mut fields = entry.fields();
                fields.extend([
                    (FROM_FIELD, Value::from(from.wire_word())),
                    (TO_FIELD, Value::from(to.wire_word())),
                    (CAUSE_FIELD, Value::from(cause_name(*cause))),
                ]);
                fields
            }
            Self::EntryRemoved { entry, status } => {
                let mut fields = entry.fields();
                fields.extend([
                    (STATUS_FIELD, Value::from(status.wire_word())),
                    (CAUSE_FIELD, Value::from(EXPIRY_CAUSE_WORD)),
                ]);
                fields
            }
            Self::OwnHandleAssigned { requested, handle } => vec![
                (REQUESTED_FIELD, Value::from(requested.as_str())),
                (HANDLE_FIELD, Value::from(handle.as_str())),
            ],
            Self::LookupReplaced {
                card,
                handle,
                direct_addresses,
                relay,
            } => {
                let mut fields = vec![(PEER_FIELD, card.field())];
                fields.extend(
                    handle
                        .as_ref()
                        .map(|handle| (HANDLE_FIELD, Value::from(handle.as_str()))),
                );
                fields.extend([
                    (DIRECT_ADDRESSES_FIELD, Value::from(*direct_addresses)),
                    (RELAY_FIELD, Value::from(*relay)),
                ]);
                fields
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, write_run_record};

    const PEER_ID: wire::PublicKey = [7; 32];
    const HANDLE: &str = "bob";

    #[test]
    fn observability_decision_3_the_peer_table_events_add_no_line_to_standard_error() {
        let handle: LocalHandle = HANDLE.parse().unwrap();
        let requested: AnnouncedHandle = HANDLE.parse().unwrap();
        let events: [Event; 5] = [
            PeerTableEvent::of_change(TableChange::Created {
                card: PEER_ID,
                requested: requested.clone(),
                handle: handle.clone(),
                status: Status::Online,
                cause: TableCause::Presence,
            })
            .into(),
            PeerTableEvent::of_change(TableChange::Moved {
                card: PEER_ID,
                handle: handle.clone(),
                from: Status::Online,
                to: Status::Unreachable,
                cause: TableCause::ContactFailed(ContactFailed::Send),
            })
            .into(),
            PeerTableEvent::of_change(TableChange::Removed {
                card: PEER_ID,
                handle: handle.clone(),
                status: Status::Unreachable,
            })
            .into(),
            PeerTableEvent::of_change(TableChange::OwnHandleAssigned {
                requested,
                handle: handle.clone(),
            })
            .into(),
            PeerTableEvent::lookup_replaced(&PEER_ID, Some(handle), 0, true).into(),
        ];

        for event in events {
            let mut standard_error = Vec::new();
            write_run_record(&mut standard_error, &event).unwrap();
            assert_eq!(
                String::from_utf8(standard_error).unwrap(),
                "",
                "{}",
                event.name()
            );
        }
    }

    const OBSERVABILITY_DECISION_9_CAUSE_WORDS: [(TableCause, &str); 11] = [
        (TableCause::Presence, "presence"),
        (TableCause::Departure, "departure"),
        (TableCause::Unreachable, "unreachable"),
        (TableCause::Introduce, "introduce"),
        (TableCause::IntroduceAck, "introduce_ack"),
        (
            TableCause::ContactFailed(ContactFailed::Introduce),
            "introduce_failed",
        ),
        (TableCause::Message, "message"),
        (
            TableCause::ContactSucceeded(ContactSucceeded::Probe),
            "probe_answered",
        ),
        (
            TableCause::ContactFailed(ContactFailed::Probe),
            "probe_failed",
        ),
        (
            TableCause::ContactSucceeded(ContactSucceeded::Send),
            "send_acknowledged",
        ),
        (
            TableCause::ContactFailed(ContactFailed::Send),
            "send_failed",
        ),
    ];
    const OBSERVABILITY_DECISION_9_REMOVAL_CAUSE_WORD: &str = "expiry";

    #[test]
    fn observability_decision_9_a_changed_entry_writes_the_word_of_its_own_cause_and_of_no_other() {
        let handle: LocalHandle = HANDLE.parse().unwrap();
        for (cause, word) in OBSERVABILITY_DECISION_9_CAUSE_WORDS {
            let event: Event = PeerTableEvent::of_change(TableChange::Moved {
                card: PEER_ID,
                handle: handle.clone(),
                from: Status::Online,
                to: Status::Unreachable,
                cause,
            })
            .into();

            assert_eq!(
                event.fields().last(),
                Some(&(CAUSE_FIELD, Value::from(word))),
                "{cause:?}"
            );
        }
        let removed: Event = PeerTableEvent::of_change(TableChange::Removed {
            card: PEER_ID,
            handle,
            status: Status::Gone { since_ms: 0 },
        })
        .into();
        assert_eq!(
            removed.fields().last(),
            Some(&(
                CAUSE_FIELD,
                Value::from(OBSERVABILITY_DECISION_9_REMOVAL_CAUSE_WORD)
            ))
        );
        let mut words: Vec<&str> = OBSERVABILITY_DECISION_9_CAUSE_WORDS
            .map(|(_cause, word)| word)
            .into_iter()
            .chain([OBSERVABILITY_DECISION_9_REMOVAL_CAUSE_WORD])
            .collect();
        words.sort_unstable();
        assert!(
            words.windows(2).all(|pair| pair.first() != pair.last()),
            "{words:?}"
        );
    }
}
