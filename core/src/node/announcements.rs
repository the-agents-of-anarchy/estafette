//! When this node announces and under which seq: a seq above the last whatever the wall clock does, a changed address announced no sooner than the re-announce interval, and which presence requests and rumors call for a Presence.

use std::collections::BTreeMap;
use std::time::Duration;

use tokio::time::Instant;

use crate::clock::later_by;
use crate::payload::PayloadDropRule;
use crate::peers::{is_within_announcement_max_age, is_within_announcement_max_age_ahead};
use crate::wire::{self, Addr, PresenceRequest};

const REANNOUNCE_INTERVAL: Duration = Duration::from_millis(2000);

/// The seq a payload this node signs carries after the last of its kind: `max(now_ms, last + 1)`, rising whatever the wall clock does.
fn next_seq_after(last: Option<u64>, now_ms: u64) -> u64 {
    last.map_or(now_ms, |last| now_ms.max(last.saturating_add(1)))
}

/// Takes an Unreachable report about this node as calling for a new Presence when it is within the announcement max age and not below the last Presence's seq, or names the rule that drops it.
pub(super) fn rumor_admission(
    report_ts: u64,
    last_presence_seq: Option<u64>,
    now_ms: u64,
) -> Result<(), PayloadDropRule> {
    if !is_within_announcement_max_age(report_ts, now_ms) {
        return Err(PayloadDropRule::OlderThanMaxAge);
    }
    if last_presence_seq.is_some_and(|seq| report_ts < seq) {
        return Err(PayloadDropRule::ReportOlderThanLastPresence);
    }
    Ok(())
}

/// The last Presence this node sent: its seq, its address, and when it went out.
struct Announced {
    seq: u64,
    addr: Addr,
    at: Instant,
}

/// What a change of this node's address calls for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReannounceStep {
    Unchanged,
    AnnounceNow,
    AnnounceAt(Instant),
}

/// The seq and address of this node's announcements.
#[derive(Default)]
pub(super) struct Announcer {
    last: Option<Announced>,
}

impl Announcer {
    /// The seq of the next Presence, above the last one this node sent.
    pub(super) fn next_seq(&self, now_ms: u64) -> u64 {
        next_seq_after(self.last_seq(), now_ms)
    }

    /// The seq of the last Presence this node sent, if any.
    pub(super) fn last_seq(&self) -> Option<u64> {
        self.last.as_ref().map(|last| last.seq)
    }

    /// Remembers the Presence just sent as the last one.
    pub(super) fn record(&mut self, seq: u64, addr: Addr, at: Instant) {
        self.last = Some(Announced { seq, addr, at });
    }

    /// Whether the current address differs from the last announced one, and when it may be announced.
    pub(super) fn reannounce_step(&self, current: &Addr, now: Instant) -> ReannounceStep {
        let Some(last) = &self.last else {
            return ReannounceStep::Unchanged;
        };
        if last.addr == *current {
            return ReannounceStep::Unchanged;
        }
        let earliest = later_by(last.at, REANNOUNCE_INTERVAL);
        if now >= earliest {
            ReannounceStep::AnnounceNow
        } else {
            ReannounceStep::AnnounceAt(earliest)
        }
    }
}

/// The seq of the last presence request this node sent.
#[derive(Default)]
pub(super) struct RequestSeqs {
    last: Option<u64>,
}

impl RequestSeqs {
    /// The seq of the next request, above the last one sent, recorded as sent.
    pub(super) fn advance(&mut self, now_ms: u64) -> u64 {
        let seq = next_seq_after(self.last, now_ms);
        self.last = Some(seq);
        seq
    }
}

/// The highest seq heard from each node that asked for Presence within the announcement max age.
#[derive(Default)]
pub(super) struct HeardPresenceRequests {
    highest_seq: BTreeMap<wire::PublicKey, u64>,
}

impl HeardPresenceRequests {
    /// Admits and remembers a request dated within the announcement max age of the clock, behind or ahead, whose seq is above the last heard from its requester, or names the rule that drops it.
    pub(super) fn admit(
        &mut self,
        request: &PresenceRequest,
        now_ms: u64,
    ) -> Result<(), PayloadDropRule> {
        self.highest_seq
            .retain(|_requester, seq| is_within_announcement_max_age(*seq, now_ms));
        if !is_within_announcement_max_age(request.seq, now_ms) {
            return Err(PayloadDropRule::OlderThanMaxAge);
        }
        if !is_within_announcement_max_age_ahead(request.seq, now_ms) {
            return Err(PayloadDropRule::AheadOfMaxAge);
        }
        if self
            .highest_seq
            .get(&request.requester)
            .is_some_and(|heard| request.seq <= *heard)
        {
            return Err(PayloadDropRule::NotAboveRequesterSeq);
        }
        self.highest_seq.insert(request.requester, request.seq);
        Ok(())
    }
}
