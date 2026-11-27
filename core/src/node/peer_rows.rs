//! The peer table and the register of held connections change together under one lock, each change recorded once the lock is released, and an admission is recorded from the locked table that made it.

use std::sync::{Mutex, MutexGuard, PoisonError};

use iroh::EndpointId;

use super::direct_requests::DirectAnswer;
#[cfg(test)]
use super::direct_requests::{DirectContact, DirectExchange, RequestCause};
use super::payload_records::admission_record;
use crate::event::{PeerTableEvent, RunRecord};
use crate::net::direct::HeldConnections;
use crate::payload::{DescribedPayload, PayloadDropRule};
use crate::peers::{ContactSucceeded, Effect, FailedContactKind, Operated, PeerTable, TableChange};
use crate::wire::{self, Departure, Presence, Unreachable};

/// The peer table of a node of any kind and the register of every connection its endpoint holds, changed together by the rows of spec 3.8 every such node applies, recording what each row decided.
pub struct PeerRows {
    table: Mutex<PeerTable>,
    held_connections: HeldConnections,
    report: RunRecord,
    #[cfg(test)]
    direct_exchanges: Mutex<Vec<DirectExchange>>,
}

/// A received payload the peer table admits by the operation of its own kind.
trait AdmittedByTheTable: DescribedPayload {
    /// What the table gives when it takes the payload.
    type Taken;

    /// Admits this payload to `table` at `now_ms`, or names the rule that drops it.
    fn admit_to(
        &self,
        table: &mut PeerTable,
        now_ms: u64,
    ) -> Operated<Result<Self::Taken, PayloadDropRule>>;
}

impl AdmittedByTheTable for Presence {
    type Taken = Vec<Effect>;

    fn admit_to(
        &self,
        table: &mut PeerTable,
        now_ms: u64,
    ) -> Operated<Result<Vec<Effect>, PayloadDropRule>> {
        table.admit_presence(self, now_ms)
    }
}

impl AdmittedByTheTable for Departure {
    type Taken = Vec<Effect>;

    fn admit_to(
        &self,
        table: &mut PeerTable,
        now_ms: u64,
    ) -> Operated<Result<Vec<Effect>, PayloadDropRule>> {
        table.admit_departure(self, now_ms)
    }
}

impl AdmittedByTheTable for Unreachable {
    type Taken = ();

    fn admit_to(
        &self,
        table: &mut PeerTable,
        now_ms: u64,
    ) -> Operated<Result<(), PayloadDropRule>> {
        table.admit_unreachable(self, now_ms)
    }
}

impl PeerRows {
    /// An empty table for the node of `own_card`, closing connections through the register of the endpoint it runs on and recording each row's decision to `report`.
    pub(crate) fn new(
        own_card: wire::PublicKey,
        held_connections: HeldConnections,
        report: RunRecord,
    ) -> Self {
        Self {
            table: Mutex::new(PeerTable::new(own_card)),
            held_connections,
            report,
            #[cfg(test)]
            direct_exchanges: Mutex::new(Vec::new()),
        }
    }

    /// Notes the direct requests made and the answers applied, after every earlier one.
    #[cfg(test)]
    pub(crate) fn note_direct_exchanges(
        &self,
        exchanges: impl IntoIterator<Item = DirectExchange>,
    ) {
        self.direct_exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(exchanges);
    }

    /// Every direct request made and answer applied so far, in order.
    #[cfg(test)]
    pub(crate) fn direct_exchanges(&self) -> Vec<DirectExchange> {
        self.direct_exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many probes to the card the node has made for neighbor-downs of that card.
    #[cfg(test)]
    pub(crate) fn probes_for_neighbor_downs_of(&self, card: wire::PublicKey) -> usize {
        self.direct_exchanges
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|exchange| {
                matches!(
                    exchange,
                    DirectExchange::Made {
                        kind: DirectContact::Probe,
                        cause: Some(RequestCause::NeighborDown { card: down, .. }),
                        ..
                    } if *down == card
                )
            })
            .count()
    }

    /// The peer table, locked for one operation.
    fn table(&self) -> MutexGuard<'_, PeerTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs one query on the locked table, returning what it gave once the lock is released.
    pub(crate) fn read<Given>(&self, query: impl FnOnce(&PeerTable) -> Given) -> Given {
        query(&self.table())
    }

    /// Runs one operation on the locked table, returning what it gave once the lock is released.
    fn operate_on_locked_table<Given>(
        &self,
        operation: impl FnOnce(&mut PeerTable) -> Given,
    ) -> Given {
        operation(&mut self.table())
    }

    /// Records each change of the table.
    fn record_changes(&self, changes: Vec<TableChange>) {
        for change in changes {
            (self.report)(PeerTableEvent::of_change(change).into());
        }
    }

    /// Runs one operation on the locked table, records each change it made, and returns what it gave.
    pub(crate) fn change<Given>(
        &self,
        operation: impl FnOnce(&mut PeerTable) -> Operated<Given>,
    ) -> Given {
        let operated = self.operate_on_locked_table(operation);
        self.record_changes(operated.changes);
        operated.value
    }

    /// Runs the admission of a received payload on the locked table, records whether it was taken under its signer's handle, then each change it made, and returns what it gave.
    fn admit<Received: AdmittedByTheTable>(
        &self,
        payload: &Received,
        now_ms: u64,
    ) -> Result<Received::Taken, PayloadDropRule> {
        let (
            Operated {
                value: admitted,
                changes,
            },
            record,
        ) = self.operate_on_locked_table(|table| {
            let admitted = payload.admit_to(table, now_ms);
            let facts = payload.facts();
            let signer_handle = table.handle_of(facts.signer, now_ms);
            let record =
                admission_record(facts, signer_handle, admitted.value.as_ref().err().copied());
            (admitted, record)
        });
        (self.report)(record);
        self.record_changes(changes);
        admitted
    }

    /// Applies a Presence to the table, recording whether it was taken, and when it is taken closes every connection to or from its card, of any protocol, whose process no longer runs, and every other while iroh stays on that process's path (spec 3.8, address lookup).
    pub(crate) fn take_presence(&self, presence: &Presence, now_ms: u64) -> Vec<Effect> {
        let admitted = self.admit(presence, now_ms);
        let Ok(effects) = admitted else {
            return Vec::new();
        };
        if effects.contains(&Effect::DropHeldConnection(presence.card)) {
            self.held_connections.close_silent_of(presence.card);
        }
        effects
    }

    /// Applies a Departure to the table, returning the drop of the held connection it calls for once it is taken (spec 3.8).
    #[must_use]
    pub(crate) fn take_departure(&self, departure: &Departure, now_ms: u64) -> Vec<Effect> {
        self.admit(departure, now_ms).unwrap_or_default()
    }

    /// Applies an Unreachable report about another card to the table, recording whether it was taken (spec 3.8).
    pub(crate) fn take_unreachable(&self, report: &Unreachable, now_ms: u64) {
        let _admission = self.admit(report, now_ms);
    }

    /// Applies a gossip report of a new neighbor, returning the Introduce it calls for (spec 3.8).
    #[must_use]
    pub(crate) fn take_neighbor_up(&self, node: EndpointId, now_ms: u64) -> Vec<Effect> {
        self.table()
            .neighbor_up(wire::PublicKey::from(node), now_ms)
    }

    /// Applies a gossip report of a lost neighbor, returning the Probe it calls for (spec 3.8).
    #[must_use]
    pub(crate) fn take_neighbor_down(&self, node: EndpointId, now_ms: u64) -> Vec<Effect> {
        self.change(|table| table.neighbor_down(wire::PublicKey::from(node), now_ms))
    }

    /// Applies the answer to a direct request this node sent: an `IntroduceAck` as an introduction that spares the connection it came on, a failure as a failed contact, an acknowledged Probe as a contact that succeeded (spec 3.8).
    pub(crate) fn take_direct_answer(&self, answer: DirectAnswer, now_ms: u64) -> Vec<Effect> {
        match answer {
            DirectAnswer::Introduce {
                card,
                answer: Ok(ack),
            } => sparing_the_answering_connection(
                self.change(|table| table.receive_introduce_ack(card, &ack, now_ms)),
            ),
            DirectAnswer::Introduce {
                card,
                answer: Err(_),
            } => self.change(|table| {
                table.direct_contact_failed(card, now_ms, FailedContactKind::Introduce)
            }),
            DirectAnswer::Probe {
                card,
                answer: Err(_),
            } => self.change(|table| {
                table.direct_contact_failed(card, now_ms, FailedContactKind::Probe)
            }),
            DirectAnswer::Probe {
                card,
                answer: Ok(()),
            } => {
                self.change(|table| {
                    table.direct_contact_succeeded(card, now_ms, ContactSucceeded::Probe)
                });
                Vec::new()
            }
        }
    }
}

/// The effects of an `IntroduceAck` less the drop of the held connection, since the ack came on that connection (spec 3.8).
fn sparing_the_answering_connection(effects: Vec<Effect>) -> Vec<Effect> {
    effects
        .into_iter()
        .filter(|effect| !matches!(effect, Effect::DropHeldConnection(_)))
        .collect()
}
