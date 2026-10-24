//! The peer table of spec 3.8: one entry per card, changed by pure operations that return the network effects they call for.

use std::collections::BTreeMap;

use crate::handle::{AnnouncedHandle, LocalHandle};
use crate::wire::{
    Addr, Departure, Introduce, IntroduceAck, Message, Presence, PublicKey, Unreachable,
};

const ANNOUNCEMENT_MAX_AGE_MS: u64 = 300_000;
const GONE_ENTRY_RETENTION_MS: u64 = 3_600_000;

/// Where a peer stands at this node, as the peers tool reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Online,
    Unreachable,
    /// Departed at `since_ms` by this node's clock; the entry is removed once the gone entry retention passes.
    Gone {
        since_ms: u64,
    },
}

/// One member as this node knows it, keyed by its card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub card: PublicKey,
    pub effective_handle: LocalHandle,
    /// The `public` mapping as JSON bytes, `None` until a Presence, Introduce or `IntroduceAck` carries it.
    pub public: Option<Vec<u8>>,
    pub attached: bool,
    /// The address last carried, `None` until a Presence, Introduce or `IntroduceAck` carries one.
    pub addr: Option<Addr>,
    pub status: Status,
    /// When this node, by its own clock, last took a Presence, Introduce, `IntroduceAck` or Message from the card.
    pub last_seen: u64,
    seq: Option<u64>,
    last_direct_contact_ms: Option<u64>,
}

/// A network action an operation calls for, performed by the node, never by the table.
#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    /// Send Introduce directly to the card.
    SendIntroduce(PublicKey),
    /// Replace the card's entry in the address lookup with `addr`, never merging the two.
    ReplaceLookupEntry { card: PublicKey, addr: Addr },
    /// Drop the held connection to the card, sparing the connection the event arrived on.
    DropHeldConnection(PublicKey),
    /// Send Probe to the node.
    SendProbe(PublicKey),
    /// Broadcast an Unreachable report about the card, observed now.
    BroadcastUnreachable(PublicKey),
}

/// The seq of an accepted Departure, and when this node recorded it.
#[derive(Debug)]
struct Tombstone {
    seq: u64,
    recorded_ms: u64,
}

/// Every other member this node knows, with the effects each event on them calls for.
#[derive(Debug)]
pub struct PeerTable {
    own_card: PublicKey,
    own_handle: Option<LocalHandle>,
    peers: BTreeMap<PublicKey, Peer>,
    tombstones: BTreeMap<PublicKey, Tombstone>,
}

impl PeerTable {
    /// An empty table for the node that holds `own_card`.
    #[must_use]
    pub const fn new(own_card: PublicKey) -> Self {
        Self {
            own_card,
            own_handle: None,
            peers: BTreeMap::new(),
            tombstones: BTreeMap::new(),
        }
    }

    /// This node's own effective handle, assigned against the table at the first call and never changed afterward.
    #[must_use]
    pub fn assign_own_handle(&mut self, requested: &AnnouncedHandle, now_ms: u64) -> LocalHandle {
        self.forget_expired(now_ms);
        if let Some(own) = &self.own_handle {
            return own.clone();
        }
        let own = self.free_handle_for(requested);
        self.own_handle = Some(own.clone());
        own
    }

    /// Takes a Presence whose signature verified, accepting it only when it is newer than all this node holds for its card.
    #[must_use]
    pub fn receive_presence(&mut self, presence: &Presence, now_ms: u64) -> Vec<Effect> {
        self.forget_expired(now_ms);
        if !self.accepts_announcement(presence.card, presence.seq, now_ms) {
            return Vec::new();
        }
        let Ok(handle) = presence.handle.parse::<AnnouncedHandle>() else {
            return Vec::new();
        };
        let mut effects = lookup_refresh_effects(presence.card, &presence.addr);
        effects.extend(self.insert_if_unseen(presence.card, &handle, now_ms));
        if let Some(peer) = self.peers.get_mut(&presence.card) {
            peer.take_presence(presence, now_ms);
        }
        effects
    }

    /// Takes a Departure whose signature verified, setting its card Gone when it is newer than all this node holds for the card.
    pub fn receive_departure(&mut self, departure: &Departure, now_ms: u64) {
        self.forget_expired(now_ms);
        if !self.accepts_announcement(departure.card, departure.seq, now_ms) {
            return;
        }
        self.tombstones.insert(
            departure.card,
            Tombstone {
                seq: departure.seq,
                recorded_ms: now_ms,
            },
        );
        if let Some(peer) = self.peers.get_mut(&departure.card) {
            peer.take_departure(departure.seq, now_ms);
        }
    }

    /// Takes an Unreachable report whose signature verified, setting the peer it names Unreachable unless it is Gone, the report is stale, or a direct contact came after it.
    pub fn receive_unreachable(&mut self, report: &Unreachable, now_ms: u64) {
        self.forget_expired(now_ms);
        if !is_within_announcement_max_age(report.ts, now_ms) {
            return;
        }
        if let Some(peer) = self.present_entry(report.card) {
            peer.take_unreachable_report(report.ts);
        }
    }

    /// Takes a gossip report of `node` as a new neighbor, asking to introduce this node to it unless it is an Online entry.
    #[must_use]
    pub fn neighbor_up(&self, node: PublicKey) -> Vec<Effect> {
        if self
            .peers
            .get(&node)
            .is_some_and(|peer| peer.status == Status::Online)
        {
            return Vec::new();
        }
        vec![Effect::SendIntroduce(node)]
    }

    /// Takes a gossip report that `node` is no longer a neighbor, asking to probe it when it is in the table.
    #[must_use]
    pub fn neighbor_down(&mut self, node: PublicKey, now_ms: u64) -> Vec<Effect> {
        self.forget_expired(now_ms);
        if !self.peers.contains_key(&node) {
            return Vec::new();
        }
        vec![Effect::SendProbe(node)]
    }

    /// Takes a probe or send to `card` that failed at connect or at ack, setting it Unreachable and asking to announce that, unless it is Gone.
    #[must_use]
    pub fn direct_contact_failed(&mut self, card: PublicKey, now_ms: u64) -> Vec<Effect> {
        self.forget_expired(now_ms);
        let Some(peer) = self.present_entry(card) else {
            return Vec::new();
        };
        peer.status = Status::Unreachable;
        vec![Effect::BroadcastUnreachable(card)]
    }

    /// Takes a direct contact with `card` that succeeded, setting it Online unless it is Gone.
    pub fn direct_contact_succeeded(&mut self, card: PublicKey, now_ms: u64) {
        self.forget_expired(now_ms);
        if let Some(peer) = self.present_entry(card) {
            peer.take_direct_contact(now_ms);
        }
    }

    /// Takes an Introduce that arrived on a connection from `card`, as an accepted Presence without the seq check.
    #[must_use]
    pub fn receive_introduce(
        &mut self,
        card: PublicKey,
        introduce: &Introduce,
        now_ms: u64,
    ) -> Vec<Effect> {
        self.receive_introduction(
            card,
            introduce.handle.as_deref(),
            &introduce.public,
            introduce.attached,
            &introduce.addr,
            now_ms,
        )
    }

    /// Takes an `IntroduceAck` that arrived on a connection to `card`, as an accepted Presence without the seq check.
    #[must_use]
    pub fn receive_introduce_ack(
        &mut self,
        card: PublicKey,
        ack: &IntroduceAck,
        now_ms: u64,
    ) -> Vec<Effect> {
        self.receive_introduction(
            card,
            ack.handle.as_deref(),
            &ack.public,
            ack.attached,
            &ack.addr,
            now_ms,
        )
    }

    /// Takes a validated Message from `card`, inserting an unseen sender Online and asking to introduce this node to it.
    #[must_use]
    pub fn receive_message(
        &mut self,
        card: PublicKey,
        message: &Message,
        now_ms: u64,
    ) -> Vec<Effect> {
        self.forget_expired(now_ms);
        let Ok(handle) = message.handle.parse::<AnnouncedHandle>() else {
            return Vec::new();
        };
        let effects = self.insert_if_unseen(card, &handle, now_ms);
        if let Some(peer) = self.peers.get_mut(&card) {
            peer.take_message(message.attached, now_ms);
        }
        effects
    }

    /// Every entry still kept at `now_ms`, in card order.
    pub fn peers(&mut self, now_ms: u64) -> impl Iterator<Item = &Peer> {
        self.forget_expired(now_ms);
        self.peers.values()
    }

    /// Takes what an Introduce or `IntroduceAck` from `card` carries.
    fn receive_introduction(
        &mut self,
        card: PublicKey,
        handle: Option<&str>,
        public: &[u8],
        attached: bool,
        addr: &Addr,
        now_ms: u64,
    ) -> Vec<Effect> {
        self.forget_expired(now_ms);
        let Some(handle) = handle else {
            return lookup_refresh_effects(card, addr);
        };
        let Ok(handle) = handle.parse::<AnnouncedHandle>() else {
            return Vec::new();
        };
        let mut effects = lookup_refresh_effects(card, addr);
        effects.extend(self.insert_if_unseen(card, &handle, now_ms));
        if let Some(peer) = self.peers.get_mut(&card) {
            peer.take_introduction(public, attached, addr, now_ms);
        }
        effects
    }

    /// The entry of `card` unless it is Gone, since a Gone entry leaves Gone only through an accepted Presence, Introduce, `IntroduceAck` or Message.
    fn present_entry(&mut self, card: PublicKey) -> Option<&mut Peer> {
        self.peers.get_mut(&card).filter(|peer| !peer.is_gone())
    }

    /// Whether a Presence or Departure for `card` carrying `seq` is accepted at `now_ms`.
    fn accepts_announcement(&self, card: PublicKey, seq: u64, now_ms: u64) -> bool {
        card != self.own_card
            && is_within_announcement_max_age(seq, now_ms)
            && self
                .peers
                .get(&card)
                .and_then(|peer| peer.seq)
                .is_none_or(|held| seq > held)
            && self
                .tombstones
                .get(&card)
                .is_none_or(|tombstone| seq > tombstone.seq)
    }

    /// Drops every tombstone kept for the announcement max age, and every entry kept Gone for the gone entry retention, by `now_ms`.
    fn forget_expired(&mut self, now_ms: u64) {
        self.peers.retain(|_, peer| match peer.status {
            Status::Gone { since_ms } => is_still_kept(GONE_ENTRY_RETENTION_MS, since_ms, now_ms),
            Status::Online | Status::Unreachable => true,
        });
        self.tombstones.retain(|_, tombstone| {
            is_still_kept(ANNOUNCEMENT_MAX_AGE_MS, tombstone.recorded_ms, now_ms)
        });
    }

    /// Inserts `card` Online under a free effective handle when unseen, returning the Introduce that a newcomer calls for.
    fn insert_if_unseen(
        &mut self,
        card: PublicKey,
        handle: &AnnouncedHandle,
        now_ms: u64,
    ) -> Vec<Effect> {
        if self.peers.contains_key(&card) {
            return Vec::new();
        }
        let effective_handle = self.free_handle_for(handle);
        self.peers
            .insert(card, Peer::first_seen(card, effective_handle, now_ms));
        vec![Effect::SendIntroduce(card)]
    }

    /// The handle a new entry announcing `announced` takes: free beside every entry seen before it.
    fn free_handle_for(&self, announced: &AnnouncedHandle) -> LocalHandle {
        let taken = self
            .peers
            .values()
            .map(|peer| &peer.effective_handle)
            .chain(&self.own_handle)
            .cloned()
            .collect();
        LocalHandle::assign(announced, &taken)
    }
}

/// Whether `stamp` is no older than the announcement max age by the clock reading `now_ms`.
const fn is_within_announcement_max_age(stamp: u64, now_ms: u64) -> bool {
    stamp >= now_ms.saturating_sub(ANNOUNCEMENT_MAX_AGE_MS)
}

/// Whether something kept for `period_ms` from `since_ms` is still kept at `now_ms`.
const fn is_still_kept(period_ms: u64, since_ms: u64, now_ms: u64) -> bool {
    now_ms.saturating_sub(since_ms) < period_ms
}

/// The effects every accepted Presence and every Introduce or `IntroduceAck` calls for: replace the lookup entry, drop the held connection.
fn lookup_refresh_effects(card: PublicKey, addr: &Addr) -> Vec<Effect> {
    vec![
        Effect::ReplaceLookupEntry {
            card,
            addr: addr.clone(),
        },
        Effect::DropHeldConnection(card),
    ]
}

impl Peer {
    /// A newly seen entry, Online and seen now, before the event that brought it fills in what it carried.
    const fn first_seen(card: PublicKey, effective_handle: LocalHandle, now_ms: u64) -> Self {
        Self {
            card,
            effective_handle,
            public: None,
            attached: false,
            addr: None,
            status: Status::Online,
            last_seen: now_ms,
            seq: None,
            last_direct_contact_ms: None,
        }
    }

    /// Whether the entry departed and awaits its removal.
    const fn is_gone(&self) -> bool {
        matches!(self.status, Status::Gone { .. })
    }

    /// Records a Presence: what it carries, seen now, Online.
    fn take_presence(&mut self, presence: &Presence, now_ms: u64) {
        self.public = Some(presence.public.clone());
        self.attached = presence.attached;
        self.addr = Some(presence.addr.clone());
        self.seq = Some(presence.seq);
        self.last_seen = now_ms;
        self.status = Status::Online;
    }

    /// Records an Introduce or `IntroduceAck`: what it carries, seen now, a direct contact.
    fn take_introduction(&mut self, public: &[u8], attached: bool, addr: &Addr, now_ms: u64) {
        self.public = Some(public.to_vec());
        self.attached = attached;
        self.addr = Some(addr.clone());
        self.last_seen = now_ms;
        self.take_direct_contact(now_ms);
    }

    /// Records an Unreachable report observed at `ts`: Unreachable, unless the held seq is above `ts` or a direct contact came after it.
    fn take_unreachable_report(&mut self, ts: u64) {
        if self.seq.is_some_and(|held| ts < held)
            || self
                .last_direct_contact_ms
                .is_some_and(|contact_ms| contact_ms > ts)
        {
            return;
        }
        self.status = Status::Unreachable;
    }

    /// Records a direct contact now: Online.
    const fn take_direct_contact(&mut self, now_ms: u64) {
        self.last_direct_contact_ms = Some(now_ms);
        self.status = Status::Online;
    }

    /// Records a Departure: its seq as the highest held, Gone from now.
    const fn take_departure(&mut self, seq: u64, now_ms: u64) {
        self.seq = Some(seq);
        self.status = Status::Gone { since_ms: now_ms };
    }

    /// Records a Message: its attached flag, seen now, a direct contact.
    const fn take_message(&mut self, attached: bool, now_ms: u64) {
        self.attached = attached;
        self.last_seen = now_ms;
        self.take_direct_contact(now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{
        Departure, Introduce, IntroduceAck, Message, MessageId, Presence, SignatureBytes,
        Unreachable, Version,
    };

    const NOW_MS: u64 = 1_700_000_000_000;
    const OWN_CARD: PublicKey = [1; 32];
    const PEER_CARD: PublicKey = [2; 32];
    const PEER_HANDLE: &str = "bob";
    const FIRST_SEQ: u64 = 1_699_999_999_000;
    const FIRST_PUBLIC: &[u8] = br#"{"role":"reviewer"}"#;
    const FIRST_RELAY: &str = "https://relay-first.example";
    const LATER_MS: u64 = 1_700_000_060_000;
    const SECOND_SEQ: u64 = 1_700_000_030_000;
    const SECOND_HANDLE: &str = "carol";
    const SECOND_PUBLIC: &[u8] = br#"{"role":"author"}"#;
    const SECOND_RELAY: &str = "https://relay-second.example";
    const SECOND_DIRECT: &str = "192.0.2.7:4433";
    const DEPARTURE_SEQ: u64 = 1_700_000_040_000;
    const OLDEST_ACCEPTED_SEQ: u64 = NOW_MS - ANNOUNCEMENT_MAX_AGE_MS;
    const DEPARTURE_SEQ_FROM_A_CLOCK_AHEAD: u64 = NOW_MS + 400_000;
    const PRESENCE_SEQ_FROM_A_CLOCK_AHEAD: u64 = NOW_MS + 350_000;
    const TOMBSTONE_LAST_KEPT_MS: u64 = NOW_MS + ANNOUNCEMENT_MAX_AGE_MS - 1;
    const TOMBSTONE_FORGOTTEN_MS: u64 = NOW_MS + ANNOUNCEMENT_MAX_AGE_MS;
    const FIRST_MEMBER_CARD: PublicKey = [4; 32];
    const SECOND_MEMBER_CARD: PublicKey = [5; 32];
    const THIRD_NODE_CARD: PublicKey = [6; 32];
    const REQUESTED_HANDLE: &str = "hal";
    const NOT_AN_ANNOUNCED_HANDLE: &str = "Hal";
    const SECOND_MEMBER_EFFECTIVE_HANDLE: &str = "hal-2";
    const RESTART_SEQ: u64 = 1_700_000_050_000;
    const REPORTER_CARD: PublicKey = [7; 32];
    const OBSERVED_TS: u64 = 1_700_000_001_000;
    const SEQ_BEFORE_OBSERVATION: u64 = 1_700_000_000_500;
    const REPORT_RECEIVED_MS: u64 = NOW_MS + 1_000;
    const OLDEST_TAKEN_REPORT_TS: u64 = REPORT_RECEIVED_MS - ANNOUNCEMENT_MAX_AGE_MS;
    const GONE_LAST_KEPT_MS: u64 = NOW_MS + GONE_ENTRY_RETENTION_MS - 1;
    const GONE_REMOVED_MS: u64 = NOW_MS + GONE_ENTRY_RETENTION_MS;
    const MESSAGE_ID: MessageId = [3; 16];
    const BODY: &str = "hello";

    fn message_from(handle: &str, attached: bool) -> Message {
        Message {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            to: OWN_CARD,
            handle: handle.to_owned(),
            attached,
            sent_at: NOW_MS,
            body: BODY.to_owned(),
            mac: [0; 32],
        }
    }

    fn first_addr() -> Addr {
        Addr::from_endpoint_report([FIRST_RELAY.to_owned()], [])
    }

    fn second_addr() -> Addr {
        Addr::from_endpoint_report([SECOND_RELAY.to_owned()], [SECOND_DIRECT.parse().unwrap()])
    }

    fn second_session_presence(card: PublicKey) -> Presence {
        Presence {
            handle: SECOND_HANDLE.to_owned(),
            public: SECOND_PUBLIC.to_vec(),
            attached: false,
            addr: second_addr(),
            ..presence(card, SECOND_HANDLE, SECOND_SEQ)
        }
    }

    fn departure(card: PublicKey, seq: u64) -> Departure {
        Departure {
            v: Version::CURRENT,
            card,
            seq,
            mac: [0; 32],
            sig: SignatureBytes([0; 64]),
        }
    }

    fn introduce(handle: Option<&str>) -> Introduce {
        Introduce {
            v: Version::CURRENT,
            handle: handle.map(str::to_owned),
            public: SECOND_PUBLIC.to_vec(),
            attached: true,
            addr: second_addr(),
            mac: [0; 32],
        }
    }

    fn introduce_ack(handle: Option<&str>) -> IntroduceAck {
        IntroduceAck {
            v: Version::CURRENT,
            handle: handle.map(str::to_owned),
            public: SECOND_PUBLIC.to_vec(),
            attached: true,
            addr: second_addr(),
            mac: [0; 32],
        }
    }

    fn introduction_effects(card: PublicKey) -> Vec<Effect> {
        vec![
            Effect::ReplaceLookupEntry {
                card,
                addr: second_addr(),
            },
            Effect::DropHeldConnection(card),
            Effect::SendIntroduce(card),
        ]
    }

    fn unreachable_report(card: PublicKey, ts: u64) -> Unreachable {
        Unreachable {
            v: Version::CURRENT,
            card,
            reporter: REPORTER_CARD,
            ts,
            mac: [0; 32],
            sig: SignatureBytes([0; 64]),
        }
    }

    fn snapshot(table: &mut PeerTable, now_ms: u64) -> Vec<Peer> {
        table.peers(now_ms).cloned().collect()
    }

    fn presence(card: PublicKey, handle: &str, seq: u64) -> Presence {
        Presence {
            v: Version::CURRENT,
            card,
            handle: handle.to_owned(),
            public: FIRST_PUBLIC.to_vec(),
            attached: true,
            addr: first_addr(),
            seq,
            mac: [0; 32],
            sig: SignatureBytes([0; 64]),
        }
    }

    fn only_peer(table: &mut PeerTable, now_ms: u64) -> Peer {
        let peers: Vec<Peer> = table.peers(now_ms).cloned().collect();
        assert_eq!(peers.len(), 1, "{peers:?}");
        peers.into_iter().next().unwrap()
    }

    fn accept_presence(table: &mut PeerTable, presence: &Presence, now_ms: u64) {
        assert_ne!(
            table.receive_presence(presence, now_ms),
            vec![],
            "the table refused a setup Presence"
        );
    }

    fn listed(table: &mut PeerTable, now_ms: u64) -> Vec<(PublicKey, String, Status, bool, u64)> {
        table
            .peers(now_ms)
            .map(|peer| {
                (
                    peer.card,
                    peer.effective_handle.as_str().to_owned(),
                    peer.status,
                    peer.attached,
                    peer.last_seen,
                )
            })
            .collect()
    }

    #[test]
    fn a_message_from_an_unseen_card_lists_its_sender_online_and_asks_for_an_introduce_test_8() {
        let mut table = PeerTable::new(OWN_CARD);
        let effects = table.receive_message(PEER_CARD, &message_from(PEER_HANDLE, true), NOW_MS);
        assert_eq!(effects, vec![Effect::SendIntroduce(PEER_CARD)]);
        assert_eq!(
            listed(&mut table, NOW_MS),
            vec![(
                PEER_CARD,
                PEER_HANDLE.to_owned(),
                Status::Online,
                true,
                NOW_MS
            )]
        );
    }

    #[test]
    fn a_presence_from_an_unknown_card_inserts_it_online_and_calls_for_lookup_drop_and_introduce_spec_3_8()
     {
        let mut table = PeerTable::new(OWN_CARD);
        let effects = table.receive_presence(&presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ), NOW_MS);
        assert_eq!(
            effects,
            vec![
                Effect::ReplaceLookupEntry {
                    card: PEER_CARD,
                    addr: first_addr()
                },
                Effect::DropHeldConnection(PEER_CARD),
                Effect::SendIntroduce(PEER_CARD),
            ]
        );
        let peer = only_peer(&mut table, NOW_MS);
        assert_eq!(
            (
                peer.card,
                peer.effective_handle.as_str(),
                peer.public.as_deref(),
                peer.attached,
                peer.addr.as_ref(),
                peer.status,
                peer.last_seen
            ),
            (
                PEER_CARD,
                PEER_HANDLE,
                Some(FIRST_PUBLIC),
                true,
                Some(&first_addr()),
                Status::Online,
                NOW_MS
            )
        );
    }

    #[test]
    fn a_replayed_presence_with_a_lower_or_equal_seq_changes_nothing_test_20() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(&mut table, &second_session_presence(PEER_CARD), NOW_MS);
        let before = snapshot(&mut table, NOW_MS);
        for replayed_seq in [FIRST_SEQ, SECOND_SEQ] {
            let replay = presence(PEER_CARD, PEER_HANDLE, replayed_seq);
            assert_eq!(table.receive_presence(&replay, LATER_MS), vec![]);
            assert_eq!(snapshot(&mut table, LATER_MS), before);
        }
    }

    #[test]
    fn a_newer_presence_of_a_known_card_replaces_what_it_carries_and_keeps_the_handle_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        let effects = table.receive_presence(&second_session_presence(PEER_CARD), LATER_MS);
        assert_eq!(
            effects,
            vec![
                Effect::ReplaceLookupEntry {
                    card: PEER_CARD,
                    addr: second_addr()
                },
                Effect::DropHeldConnection(PEER_CARD),
            ]
        );
        let peer = only_peer(&mut table, LATER_MS);
        assert_eq!(
            (
                peer.effective_handle.as_str(),
                peer.public.as_deref(),
                peer.attached,
                peer.addr.as_ref(),
                peer.status,
                peer.last_seen
            ),
            (
                PEER_HANDLE,
                Some(SECOND_PUBLIC),
                false,
                Some(&second_addr()),
                Status::Online,
                LATER_MS
            )
        );
    }

    #[test]
    fn a_presence_older_than_the_announcement_max_age_is_dropped_and_one_exactly_that_old_accepted_spec_3_8()
     {
        let mut table = PeerTable::new(OWN_CARD);
        let too_old = presence(PEER_CARD, PEER_HANDLE, OLDEST_ACCEPTED_SEQ - 1);
        assert_eq!(table.receive_presence(&too_old, NOW_MS), vec![]);
        assert_eq!(snapshot(&mut table, NOW_MS), vec![]);
        let oldest_accepted = presence(PEER_CARD, PEER_HANDLE, OLDEST_ACCEPTED_SEQ);
        assert_ne!(table.receive_presence(&oldest_accepted, NOW_MS), vec![]);
        assert_eq!(only_peer(&mut table, NOW_MS).card, PEER_CARD);
    }

    #[test]
    fn announcement_max_age_is_the_spec_figure_of_300_seconds_spec_3_14() {
        assert_eq!(ANNOUNCEMENT_MAX_AGE_MS, 300_000);
    }

    #[test]
    fn a_presence_for_the_own_card_is_ignored_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        let own = presence(OWN_CARD, PEER_HANDLE, FIRST_SEQ);
        assert_eq!(table.receive_presence(&own, NOW_MS), vec![]);
        assert_eq!(snapshot(&mut table, NOW_MS), vec![]);
    }

    #[test]
    fn after_a_departure_a_replay_of_any_earlier_presence_leaves_the_peer_gone_test_20() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        table.receive_departure(&departure(PEER_CARD, DEPARTURE_SEQ), LATER_MS);
        let departed = snapshot(&mut table, LATER_MS);
        assert_eq!(
            departed.iter().map(|peer| peer.status).collect::<Vec<_>>(),
            vec![Status::Gone { since_ms: LATER_MS }]
        );
        for replayed_seq in [FIRST_SEQ, SECOND_SEQ, DEPARTURE_SEQ] {
            let replay = presence(PEER_CARD, PEER_HANDLE, replayed_seq);
            assert_eq!(table.receive_presence(&replay, LATER_MS), vec![]);
            assert_eq!(snapshot(&mut table, LATER_MS), departed);
        }
    }

    #[test]
    fn a_departure_of_an_unknown_card_blocks_presences_up_to_its_seq_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        table.receive_departure(&departure(PEER_CARD, DEPARTURE_SEQ), NOW_MS);
        assert_eq!(snapshot(&mut table, NOW_MS), vec![]);
        for blocked_seq in [SECOND_SEQ, DEPARTURE_SEQ] {
            let blocked = presence(PEER_CARD, PEER_HANDLE, blocked_seq);
            assert_eq!(table.receive_presence(&blocked, NOW_MS), vec![]);
            assert_eq!(snapshot(&mut table, NOW_MS), vec![]);
        }
        let newer = presence(PEER_CARD, PEER_HANDLE, DEPARTURE_SEQ + 1);
        assert_ne!(table.receive_presence(&newer, NOW_MS), vec![]);
        assert_eq!(only_peer(&mut table, NOW_MS).status, Status::Online);
    }

    #[test]
    fn a_tombstone_is_kept_for_the_announcement_max_age_by_the_receivers_clock_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        table.receive_departure(
            &departure(PEER_CARD, DEPARTURE_SEQ_FROM_A_CLOCK_AHEAD),
            NOW_MS,
        );
        let below_tombstone = presence(PEER_CARD, PEER_HANDLE, PRESENCE_SEQ_FROM_A_CLOCK_AHEAD);
        assert_eq!(
            table.receive_presence(&below_tombstone, TOMBSTONE_LAST_KEPT_MS),
            vec![]
        );
        assert_ne!(
            table.receive_presence(&below_tombstone, TOMBSTONE_FORGOTTEN_MS),
            vec![]
        );
    }

    #[test]
    fn a_gone_entry_is_kept_for_the_gone_entry_retention_then_removed_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        table.receive_departure(&departure(PEER_CARD, DEPARTURE_SEQ), NOW_MS);
        assert_eq!(
            only_peer(&mut table, GONE_LAST_KEPT_MS).status,
            Status::Gone { since_ms: NOW_MS }
        );
        assert_eq!(snapshot(&mut table, GONE_REMOVED_MS), vec![]);
    }

    #[test]
    fn gone_entry_retention_is_the_spec_figure_of_3600_seconds_spec_3_14() {
        assert_eq!(GONE_ENTRY_RETENTION_MS, 3_600_000);
    }

    fn announced(text: &str) -> AnnouncedHandle {
        text.parse().unwrap()
    }

    fn handles_at(table: &mut PeerTable, now_ms: u64) -> Vec<(PublicKey, String)> {
        table
            .peers(now_ms)
            .map(|peer| (peer.card, peer.effective_handle.as_str().to_owned()))
            .collect()
    }

    #[test]
    fn the_second_member_with_a_taken_handle_is_h_2_at_every_node_and_keeps_it_after_the_first_restarts_test_7()
     {
        let first_member = presence(FIRST_MEMBER_CARD, REQUESTED_HANDLE, FIRST_SEQ);
        let second_member = presence(
            SECOND_MEMBER_CARD,
            SECOND_MEMBER_EFFECTIVE_HANDLE,
            SECOND_SEQ,
        );
        let first_member_restarted = presence(FIRST_MEMBER_CARD, REQUESTED_HANDLE, RESTART_SEQ);
        let expected = vec![
            (FIRST_MEMBER_CARD, REQUESTED_HANDLE.to_owned()),
            (
                SECOND_MEMBER_CARD,
                SECOND_MEMBER_EFFECTIVE_HANDLE.to_owned(),
            ),
        ];

        let mut at_second = PeerTable::new(SECOND_MEMBER_CARD);
        accept_presence(&mut at_second, &first_member, NOW_MS);
        let own = at_second.assign_own_handle(&announced(REQUESTED_HANDLE), NOW_MS);
        assert_eq!(own.as_str(), SECOND_MEMBER_EFFECTIVE_HANDLE);
        accept_presence(&mut at_second, &first_member_restarted, LATER_MS);
        assert_eq!(
            at_second
                .assign_own_handle(&announced(REQUESTED_HANDLE), LATER_MS)
                .as_str(),
            SECOND_MEMBER_EFFECTIVE_HANDLE
        );
        assert_eq!(handles_at(&mut at_second, LATER_MS), expected[..1]);

        let mut at_first = PeerTable::new(FIRST_MEMBER_CARD);
        let own = at_first.assign_own_handle(&announced(REQUESTED_HANDLE), NOW_MS);
        assert_eq!(own.as_str(), REQUESTED_HANDLE);
        accept_presence(&mut at_first, &second_member, NOW_MS);
        assert_eq!(handles_at(&mut at_first, NOW_MS), expected[1..]);

        let mut at_third = PeerTable::new(THIRD_NODE_CARD);
        accept_presence(&mut at_third, &first_member, NOW_MS);
        accept_presence(&mut at_third, &second_member, NOW_MS);
        accept_presence(&mut at_third, &first_member_restarted, LATER_MS);
        assert_eq!(handles_at(&mut at_third, LATER_MS), expected);
    }

    #[test]
    fn a_newcomer_announcing_the_own_handle_yields_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        assert_eq!(
            table
                .assign_own_handle(&announced(REQUESTED_HANDLE), NOW_MS)
                .as_str(),
            REQUESTED_HANDLE
        );
        accept_presence(
            &mut table,
            &presence(PEER_CARD, REQUESTED_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        assert_eq!(
            handles_at(&mut table, NOW_MS),
            vec![(PEER_CARD, SECOND_MEMBER_EFFECTIVE_HANDLE.to_owned())]
        );
    }

    #[test]
    fn a_gone_entry_keeps_its_handle_until_removed_and_a_newcomer_yields_to_it_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(FIRST_MEMBER_CARD, REQUESTED_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        table.receive_departure(&departure(FIRST_MEMBER_CARD, DEPARTURE_SEQ), NOW_MS);
        let newcomer_while_gone = presence(SECOND_MEMBER_CARD, REQUESTED_HANDLE, GONE_LAST_KEPT_MS);
        accept_presence(&mut table, &newcomer_while_gone, GONE_LAST_KEPT_MS);
        assert_eq!(
            handles_at(&mut table, GONE_LAST_KEPT_MS),
            vec![
                (FIRST_MEMBER_CARD, REQUESTED_HANDLE.to_owned()),
                (
                    SECOND_MEMBER_CARD,
                    SECOND_MEMBER_EFFECTIVE_HANDLE.to_owned()
                ),
            ]
        );
        let newcomer_after_removal = presence(PEER_CARD, REQUESTED_HANDLE, GONE_REMOVED_MS);
        accept_presence(&mut table, &newcomer_after_removal, GONE_REMOVED_MS);
        assert_eq!(
            handles_at(&mut table, GONE_REMOVED_MS),
            vec![
                (PEER_CARD, REQUESTED_HANDLE.to_owned()),
                (
                    SECOND_MEMBER_CARD,
                    SECOND_MEMBER_EFFECTIVE_HANDLE.to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_presence_introduce_ack_or_message_whose_announced_handle_does_not_match_is_dropped_spec_3_1()
     {
        let mut table = PeerTable::new(OWN_CARD);
        let presence = presence(PEER_CARD, NOT_AN_ANNOUNCED_HANDLE, FIRST_SEQ);
        assert_eq!(table.receive_presence(&presence, NOW_MS), vec![]);
        let introduce = introduce(Some(NOT_AN_ANNOUNCED_HANDLE));
        assert_eq!(
            table.receive_introduce(PEER_CARD, &introduce, NOW_MS),
            vec![]
        );
        let ack = introduce_ack(Some(NOT_AN_ANNOUNCED_HANDLE));
        assert_eq!(table.receive_introduce_ack(PEER_CARD, &ack, NOW_MS), vec![]);
        let message = message_from(NOT_AN_ANNOUNCED_HANDLE, true);
        assert_eq!(table.receive_message(PEER_CARD, &message, NOW_MS), vec![]);
        assert_eq!(snapshot(&mut table, NOW_MS), vec![]);
    }

    #[test]
    fn a_message_from_a_known_card_updates_attached_and_last_seen_and_sets_it_online_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        table.receive_departure(&departure(PEER_CARD, DEPARTURE_SEQ), NOW_MS);
        let message = message_from(SECOND_HANDLE, false);
        assert_eq!(table.receive_message(PEER_CARD, &message, LATER_MS), vec![]);
        let peer = only_peer(&mut table, LATER_MS);
        assert_eq!(
            (
                peer.effective_handle.as_str(),
                peer.public.as_deref(),
                peer.attached,
                peer.addr.as_ref(),
                peer.status,
                peer.last_seen
            ),
            (
                PEER_HANDLE,
                Some(FIRST_PUBLIC),
                false,
                Some(&first_addr()),
                Status::Online,
                LATER_MS
            )
        );
    }

    #[test]
    fn an_introduce_or_ack_with_a_handle_from_an_unknown_card_inserts_it_like_an_accepted_presence_spec_3_8()
     {
        let mut table = PeerTable::new(OWN_CARD);
        assert_eq!(
            table.receive_introduce(FIRST_MEMBER_CARD, &introduce(Some(PEER_HANDLE)), NOW_MS),
            introduction_effects(FIRST_MEMBER_CARD)
        );
        assert_eq!(
            table.receive_introduce_ack(
                SECOND_MEMBER_CARD,
                &introduce_ack(Some(SECOND_HANDLE)),
                NOW_MS
            ),
            introduction_effects(SECOND_MEMBER_CARD)
        );
        let carried: Vec<_> = table
            .peers(NOW_MS)
            .map(|peer| {
                (
                    peer.card,
                    peer.effective_handle.as_str().to_owned(),
                    peer.public.clone(),
                    peer.attached,
                    peer.addr.clone(),
                    peer.status,
                    peer.last_seen,
                )
            })
            .collect();
        let expected = |card, handle: &str| {
            (
                card,
                handle.to_owned(),
                Some(SECOND_PUBLIC.to_vec()),
                true,
                Some(second_addr()),
                Status::Online,
                NOW_MS,
            )
        };
        assert_eq!(
            carried,
            vec![
                expected(FIRST_MEMBER_CARD, PEER_HANDLE),
                expected(SECOND_MEMBER_CARD, SECOND_HANDLE)
            ]
        );
    }

    #[test]
    fn an_introduce_or_ack_without_a_handle_refreshes_the_lookup_and_never_enters_the_table_spec_3_8()
     {
        let mut table = PeerTable::new(OWN_CARD);
        let lookup_only = |card| {
            vec![
                Effect::ReplaceLookupEntry {
                    card,
                    addr: second_addr(),
                },
                Effect::DropHeldConnection(card),
            ]
        };
        assert_eq!(
            table.receive_introduce(THIRD_NODE_CARD, &introduce(None), NOW_MS),
            lookup_only(THIRD_NODE_CARD)
        );
        assert_eq!(
            table.receive_introduce_ack(PEER_CARD, &introduce_ack(None), NOW_MS),
            lookup_only(PEER_CARD)
        );
        assert_eq!(snapshot(&mut table, NOW_MS), vec![]);
    }

    #[test]
    fn an_introduce_from_a_known_card_updates_it_without_the_seq_check_and_keeps_the_handle_spec_3_8()
     {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        table.receive_departure(&departure(PEER_CARD, DEPARTURE_SEQ), NOW_MS);
        let mut lookup_only = introduction_effects(PEER_CARD);
        lookup_only.pop();
        assert_eq!(
            table.receive_introduce(PEER_CARD, &introduce(Some(SECOND_HANDLE)), LATER_MS),
            lookup_only
        );
        let peer = only_peer(&mut table, LATER_MS);
        assert_eq!(
            (
                peer.effective_handle.as_str(),
                peer.public.as_deref(),
                peer.attached,
                peer.addr.as_ref(),
                peer.status,
                peer.last_seen
            ),
            (
                PEER_HANDLE,
                Some(SECOND_PUBLIC),
                true,
                Some(&second_addr()),
                Status::Online,
                LATER_MS
            )
        );
    }

    #[test]
    fn an_unreachable_report_about_a_peer_sets_it_unreachable_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        table.receive_unreachable(&unreachable_report(PEER_CARD, OBSERVED_TS), LATER_MS);
        assert_eq!(only_peer(&mut table, LATER_MS).status, Status::Unreachable);
    }

    fn status_after_report(presence_seq: u64, report_ts: u64, report_received_ms: u64) -> Status {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, presence_seq),
            NOW_MS,
        );
        table.receive_unreachable(
            &unreachable_report(PEER_CARD, report_ts),
            report_received_ms,
        );
        only_peer(&mut table, report_received_ms).status
    }

    #[test]
    fn an_unreachable_report_older_than_the_announcement_max_age_is_dropped_spec_3_8() {
        assert_eq!(
            status_after_report(
                OLDEST_ACCEPTED_SEQ,
                OLDEST_TAKEN_REPORT_TS - 1,
                REPORT_RECEIVED_MS
            ),
            Status::Online
        );
        assert_eq!(
            status_after_report(
                OLDEST_ACCEPTED_SEQ,
                OLDEST_TAKEN_REPORT_TS,
                REPORT_RECEIVED_MS
            ),
            Status::Unreachable
        );
    }

    #[test]
    fn an_unreachable_report_below_the_held_seq_is_dropped_spec_3_8() {
        assert_eq!(
            status_after_report(SECOND_SEQ, SECOND_SEQ - 1, LATER_MS),
            Status::Online
        );
        assert_eq!(
            status_after_report(SECOND_SEQ, SECOND_SEQ, LATER_MS),
            Status::Unreachable
        );
    }

    fn status_after_contact_then_report(
        contact: impl Fn(&mut PeerTable, u64),
        contact_ms: u64,
    ) -> Status {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        contact(&mut table, contact_ms);
        table.receive_unreachable(&unreachable_report(PEER_CARD, OBSERVED_TS), LATER_MS);
        only_peer(&mut table, LATER_MS).status
    }

    #[test]
    fn a_direct_contact_more_recent_than_the_report_keeps_the_peer_and_a_presence_is_no_direct_contact_spec_3_8()
     {
        let by_message = |table: &mut PeerTable, at_ms| {
            assert_eq!(
                table.receive_message(PEER_CARD, &message_from(PEER_HANDLE, true), at_ms),
                vec![]
            );
        };
        let by_introduce = |table: &mut PeerTable, at_ms| {
            assert_ne!(
                table.receive_introduce(PEER_CARD, &introduce(Some(PEER_HANDLE)), at_ms),
                vec![]
            );
        };
        let by_presence = |table: &mut PeerTable, at_ms| {
            accept_presence(
                table,
                &presence(PEER_CARD, PEER_HANDLE, SEQ_BEFORE_OBSERVATION),
                at_ms,
            );
        };
        assert_eq!(
            status_after_contact_then_report(by_message, OBSERVED_TS + 1),
            Status::Online
        );
        assert_eq!(
            status_after_contact_then_report(by_introduce, OBSERVED_TS + 1),
            Status::Online
        );
        assert_eq!(
            status_after_contact_then_report(by_message, OBSERVED_TS),
            Status::Unreachable
        );
        assert_eq!(
            status_after_contact_then_report(by_presence, OBSERVED_TS + 1),
            Status::Unreachable
        );
    }

    #[test]
    fn a_neighbor_up_calls_for_an_introduce_unless_it_is_an_online_entry_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        accept_presence(
            &mut table,
            &presence(FIRST_MEMBER_CARD, REQUESTED_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        accept_presence(
            &mut table,
            &presence(SECOND_MEMBER_CARD, SECOND_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        table.receive_unreachable(
            &unreachable_report(FIRST_MEMBER_CARD, OBSERVED_TS),
            LATER_MS,
        );
        table.receive_departure(&departure(SECOND_MEMBER_CARD, DEPARTURE_SEQ), LATER_MS);
        assert_eq!(table.neighbor_up(PEER_CARD), vec![]);
        for not_online in [FIRST_MEMBER_CARD, SECOND_MEMBER_CARD, THIRD_NODE_CARD] {
            assert_eq!(
                table.neighbor_up(not_online),
                vec![Effect::SendIntroduce(not_online)]
            );
        }
    }

    #[test]
    fn a_neighbor_down_calls_for_a_probe_only_when_it_is_in_the_table_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        accept_presence(
            &mut table,
            &presence(FIRST_MEMBER_CARD, REQUESTED_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        table.receive_departure(&departure(FIRST_MEMBER_CARD, DEPARTURE_SEQ), NOW_MS);
        assert_eq!(
            table.neighbor_down(PEER_CARD, GONE_LAST_KEPT_MS),
            vec![Effect::SendProbe(PEER_CARD)]
        );
        assert_eq!(
            table.neighbor_down(FIRST_MEMBER_CARD, GONE_LAST_KEPT_MS),
            vec![Effect::SendProbe(FIRST_MEMBER_CARD)]
        );
        assert_eq!(
            table.neighbor_down(THIRD_NODE_CARD, GONE_LAST_KEPT_MS),
            vec![]
        );
        assert_eq!(
            table.neighbor_down(FIRST_MEMBER_CARD, GONE_REMOVED_MS),
            vec![]
        );
    }

    #[test]
    fn a_failed_probe_or_send_sets_the_peer_unreachable_and_calls_for_an_unreachable_broadcast_spec_3_8()
     {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        assert_eq!(
            table.direct_contact_failed(PEER_CARD, LATER_MS),
            vec![Effect::BroadcastUnreachable(PEER_CARD)]
        );
        assert_eq!(only_peer(&mut table, LATER_MS).status, Status::Unreachable);
        assert_eq!(
            table.direct_contact_failed(THIRD_NODE_CARD, LATER_MS),
            vec![]
        );
        assert_eq!(only_peer(&mut table, LATER_MS).card, PEER_CARD);
    }

    #[test]
    fn a_successful_direct_contact_sets_the_peer_online_and_outweighs_an_older_report_spec_3_8() {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        assert_eq!(
            table.direct_contact_failed(PEER_CARD, NOW_MS),
            vec![Effect::BroadcastUnreachable(PEER_CARD)]
        );
        table.direct_contact_succeeded(PEER_CARD, OBSERVED_TS + 1);
        assert_eq!(
            only_peer(&mut table, OBSERVED_TS + 1).status,
            Status::Online
        );
        table.receive_unreachable(&unreachable_report(PEER_CARD, OBSERVED_TS), LATER_MS);
        assert_eq!(only_peer(&mut table, LATER_MS).status, Status::Online);
        table.direct_contact_succeeded(THIRD_NODE_CARD, LATER_MS);
        assert_eq!(only_peer(&mut table, LATER_MS).card, PEER_CARD);
    }

    fn table_with_peer_gone_since_now() -> PeerTable {
        let mut table = PeerTable::new(OWN_CARD);
        accept_presence(
            &mut table,
            &presence(PEER_CARD, PEER_HANDLE, FIRST_SEQ),
            NOW_MS,
        );
        table.receive_departure(&departure(PEER_CARD, DEPARTURE_SEQ), NOW_MS);
        table
    }

    #[test]
    fn a_message_or_departure_after_the_gone_entry_retention_meets_the_entry_removed_spec_3_8() {
        let mut table = table_with_peer_gone_since_now();
        assert_eq!(
            table.receive_message(PEER_CARD, &message_from(PEER_HANDLE, true), GONE_REMOVED_MS),
            vec![Effect::SendIntroduce(PEER_CARD)]
        );
        let mut table = table_with_peer_gone_since_now();
        table.receive_departure(&departure(PEER_CARD, GONE_REMOVED_MS), GONE_REMOVED_MS);
        assert_eq!(snapshot(&mut table, GONE_REMOVED_MS), vec![]);
    }

    fn assert_still_gone_then_removed(table: &mut PeerTable) {
        assert_eq!(
            only_peer(table, GONE_LAST_KEPT_MS).status,
            Status::Gone { since_ms: NOW_MS }
        );
        assert_eq!(snapshot(table, GONE_REMOVED_MS), vec![]);
    }

    #[test]
    fn a_failed_contact_leaves_a_gone_entry_gone_without_a_broadcast_until_its_removal_spec_3_8() {
        let mut table = table_with_peer_gone_since_now();
        assert_eq!(table.direct_contact_failed(PEER_CARD, LATER_MS), vec![]);
        assert_still_gone_then_removed(&mut table);
    }

    #[test]
    fn an_unreachable_report_leaves_a_gone_entry_gone_until_its_removal_spec_3_8() {
        let mut table = table_with_peer_gone_since_now();
        table.receive_unreachable(&unreachable_report(PEER_CARD, LATER_MS), LATER_MS);
        assert_still_gone_then_removed(&mut table);
    }

    #[test]
    fn a_successful_contact_leaves_a_gone_entry_gone_until_its_removal_spec_3_8() {
        let mut table = table_with_peer_gone_since_now();
        table.direct_contact_succeeded(PEER_CARD, LATER_MS);
        assert_still_gone_then_removed(&mut table);
    }

    const PROPERTY_CASES: u64 = 2000;
    const EVENTS_PER_CASE: u64 = 24;
    const LARGEST_CLOCK_STEP_MS: u64 = 8_000;
    const SEQ_SPREAD_BELOW_NOW_MS: u64 = 400_000;
    const SEQ_SPREAD_MS: u64 = 500_000;
    const DEPARTURE_ODDS_AGAINST: u64 = 4;
    const REPLAY_OF_HIGHEST_ODDS_AGAINST: u64 = 5;
    const SPLITMIX_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;
    const SPLITMIX_FIRST_MULTIPLIER: u64 = 0xBF58_476D_1CE4_E5B9;
    const SPLITMIX_SECOND_MULTIPLIER: u64 = 0x94D0_49BB_1331_11EB;
    const SPLITMIX_FIRST_SHIFT: u32 = 30;
    const SPLITMIX_SECOND_SHIFT: u32 = 27;
    const SPLITMIX_THIRD_SHIFT: u32 = 31;

    struct Draws(u64);

    impl Draws {
        fn below(&mut self, bound: u64) -> u64 {
            self.0 = self.0.wrapping_add(SPLITMIX_GAMMA);
            let mut mixed = self.0;
            mixed =
                (mixed ^ (mixed >> SPLITMIX_FIRST_SHIFT)).wrapping_mul(SPLITMIX_FIRST_MULTIPLIER);
            mixed =
                (mixed ^ (mixed >> SPLITMIX_SECOND_SHIFT)).wrapping_mul(SPLITMIX_SECOND_MULTIPLIER);
            (mixed ^ (mixed >> SPLITMIX_THIRD_SHIFT))
                .checked_rem(bound)
                .unwrap()
        }
    }

    #[test]
    fn a_presence_is_accepted_exactly_when_its_seq_tops_every_accepted_seq_and_the_max_age_spec_3_8()
     {
        for case in 0..PROPERTY_CASES {
            let mut draws = Draws(case);
            let mut table = PeerTable::new(OWN_CARD);
            let mut highest_accepted: Option<u64> = None;
            let mut expected_status: Option<Status> = None;
            let mut now_ms = NOW_MS;
            for _ in 0..EVENTS_PER_CASE {
                now_ms = now_ms.saturating_add(draws.below(LARGEST_CLOCK_STEP_MS));
                let fresh_seq = now_ms
                    .saturating_sub(SEQ_SPREAD_BELOW_NOW_MS)
                    .saturating_add(draws.below(SEQ_SPREAD_MS));
                let seq = highest_accepted
                    .filter(|_| draws.below(REPLAY_OF_HIGHEST_ODDS_AGAINST) == 0)
                    .unwrap_or(fresh_seq);
                let acceptable = highest_accepted.is_none_or(|highest| seq > highest)
                    && seq >= now_ms.saturating_sub(ANNOUNCEMENT_MAX_AGE_MS);
                if draws.below(DEPARTURE_ODDS_AGAINST) == 0 {
                    table.receive_departure(&departure(PEER_CARD, seq), now_ms);
                    if acceptable {
                        expected_status =
                            expected_status.map(|_| Status::Gone { since_ms: now_ms });
                    }
                } else {
                    let effects =
                        table.receive_presence(&presence(PEER_CARD, PEER_HANDLE, seq), now_ms);
                    assert_eq!(!effects.is_empty(), acceptable, "case {case} seq {seq}");
                    if acceptable {
                        expected_status = Some(Status::Online);
                    }
                }
                if acceptable {
                    highest_accepted = Some(seq);
                }
                let statuses: Vec<Status> = table.peers(now_ms).map(|peer| peer.status).collect();
                assert_eq!(statuses, Vec::from_iter(expected_status), "case {case}");
            }
        }
    }
}
