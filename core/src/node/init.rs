//! When the handle is assigned and which Introduces wait for it; the observer wait outlasts the silence by which a peer judges the old process's connections gone.

use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::clock::later_by;
use crate::handle::LocalHandle;
use crate::net::direct::{GONE_PROCESS_SILENCE, HEARING_SAMPLE_INTERVAL};
use crate::net::gossip::JOIN_TIMEOUT;
use crate::wire;

const OBSERVER_WAIT: Duration = Duration::from_millis(3000);

/// A restarted member sends its first Introduce and Presence only after a wait for its handle, so each wait must outlast the silence by which a peer judges the old process's connections gone.
const _: () = assert!(
    outlasts_gone_process_silence(OBSERVER_WAIT) && outlasts_gone_process_silence(JOIN_TIMEOUT)
);

/// Whether a wait is longer than the silence that marks a connection gone, read one hearing sample late.
const fn outlasts_gone_process_silence(wait: Duration) -> bool {
    GONE_PROCESS_SILENCE
        .saturating_add(HEARING_SAMPLE_INTERVAL)
        .as_nanos()
        < wait.as_nanos()
}

/// Where init stands: awaiting the join, waiting the observer wait after it, or done.
pub(super) enum Init {
    AwaitingJoin(PendingHandle),
    ObserverWait {
        until: Instant,
        pending: PendingHandle,
    },
    Done,
}

/// What init holds until it assigns the handle: where the handle goes, and each card this node would have introduced itself to meanwhile, once, in the order first asked.
pub(super) struct PendingHandle {
    pub(super) assigned: oneshot::Sender<LocalHandle>,
    pub(super) held_introduces: Vec<wire::PublicKey>,
}

/// What moves init on: a join of the topic at a monotonic instant, a join that gained no neighbor, or the end of the observer wait.
pub(super) enum InitEvent {
    Joined { at: Instant },
    JoinTimedOut,
    ObserverWaitOver,
}

/// What a step of init asks the presence loop to do.
pub(super) enum InitAction {
    Announce,
    AssignHandle(PendingHandle),
    AssignHandleThenAnnounce(PendingHandle),
}

impl Init {
    /// The state init moves to on the event, with what the loop does for the step: a first join starts the observer wait, a later one announces, and the handle is assigned at a first join that gained no neighbor or at the end of the observer wait.
    pub(super) fn after(self, event: InitEvent) -> (Self, Option<InitAction>) {
        match (self, event) {
            (Self::AwaitingJoin(pending), InitEvent::Joined { at }) => (
                Self::ObserverWait {
                    until: observer_wait_over_at(at),
                    pending,
                },
                None,
            ),
            (Self::AwaitingJoin(pending), InitEvent::JoinTimedOut) => {
                (Self::Done, Some(InitAction::AssignHandle(pending)))
            }
            (Self::ObserverWait { pending, .. }, InitEvent::ObserverWaitOver) => (
                Self::Done,
                Some(InitAction::AssignHandleThenAnnounce(pending)),
            ),
            (Self::Done, InitEvent::Joined { .. }) => (Self::Done, Some(InitAction::Announce)),
            (unchanged @ Self::AwaitingJoin(_), InitEvent::ObserverWaitOver)
            | (
                unchanged @ Self::ObserverWait { .. },
                InitEvent::Joined { .. } | InitEvent::JoinTimedOut,
            )
            | (unchanged @ Self::Done, InitEvent::JoinTimedOut | InitEvent::ObserverWaitOver) => {
                (unchanged, None)
            }
        }
    }

    /// Holds an Introduce to the card while the handle is unassigned, a card already held not held again, returning the card to introduce this node to now once it is assigned.
    pub(super) fn introduce_now_or_hold(
        &mut self,
        card: wire::PublicKey,
    ) -> Option<wire::PublicKey> {
        match self {
            Self::AwaitingJoin(pending) | Self::ObserverWait { pending, .. } => {
                if !pending.held_introduces.contains(&card) {
                    pending.held_introduces.push(card);
                }
                None
            }
            Self::Done => Some(card),
        }
    }
}

/// When the observer wait that starts at `joined` is over.
pub fn observer_wait_over_at(joined: Instant) -> Instant {
    later_by(joined, OBSERVER_WAIT)
}
