use std::io::{self, Write};
use std::sync::Arc;

use serde_json::Value;

use crate::errors::ErrorCode;

const CODE_FIELD: &str = "code";

/// Declares the fixed names of one group of events: a fieldless enum, every value of it in `ALL`, and each value's name text.
macro_rules! event_names {
    ($(#[$meta:meta])* $group:ident { $($variant:ident => $text:expr),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $group {
            $($variant),*
        }

        impl crate::event::EventName for $group {
            const ALL: &'static [Self] = &[$(Self::$variant),*];

            fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),*
                }
            }
        }
    };
}

pub(crate) use event_names;

mod direct;
mod gossip;
mod inbox;
mod peer;
mod peer_table;
mod process;
mod state;
mod transport;

pub use direct::{DirectEvent, DirectEventName};
pub use gossip::{GossipEvent, GossipEventName};
pub use inbox::{InboxEvent, InboxEventName, ListCardsFailure};
pub(crate) use peer::ShortId;
pub use peer_table::{PeerTableEvent, PeerTableEventName};
pub use process::{
    CLOCK_BEFORE_UNIX_EPOCH, ClaimFailure, ConfigurationRewrite, PanicLocation, ProcessEvent,
    ProcessEventName,
};
pub use state::{StateEvent, StateEventName};
pub use transport::{TransportEvent, TransportEventName};

event_names! {
    /// The names of the lines a trace writer writes of its own: the count of events a full queue lost, the raw file's warning, and a line iroh wrote.
    TraceLineName {
        Dropped => estafette_trace::DROPPED_LINE_NAME,
        RawFileOpened => "raw_file_opened",
        IrohLine => "iroh_line",
    }
}

/// Where every event goes, built at the composition root: the run record on standard error and, under `--debug`, the events file; it must not block.
pub type RunRecord = Arc<dyn Fn(Event) + Send + Sync>;

/// The facts an event carries into the events file after its name, each a key and a JSON value.
pub type EventFields = Vec<(&'static str, Value)>;

/// The fixed names of one group of events, every one of them listed.
pub trait EventName: Copy + 'static {
    /// Every name of the group.
    const ALL: &'static [Self];

    /// The name as the events file writes it.
    fn as_str(self) -> &'static str;
}

/// A failure a tool answers with an error code of its own, recorded as an event.
pub trait AnsweredFailure: Into<Event> {
    /// The error code the tool answers with for this failure.
    fn answered_code(&self) -> ErrorCode;
}

/// What each area of events tells about one of its events: its fixed name, its run-record line if it has one, and its fields.
trait AreaEvent {
    /// The names of the area's events.
    type Name: EventName;

    /// The fixed name of this event.
    fn name(&self) -> Self::Name;

    /// The text of this event's fixed name.
    fn name_text(&self) -> &'static str {
        self.name().as_str()
    }

    /// The line this event writes to standard error, none for an event of the events file alone.
    fn run_record_line(&self) -> Option<String>;

    /// The facts this event carries into the events file.
    fn fields(&self) -> EventFields;
}

/// One event of the binary, in the area of `spec/observability.md` decision 9 it belongs to.
pub enum Event {
    Process(ProcessEvent),
    Gossip(GossipEvent),
    Direct(DirectEvent),
    PeerTable(PeerTableEvent),
    Transport(TransportEvent),
    InboxAndTools(InboxEvent),
    State(StateEvent),
}

impl Event {
    /// The fixed name the events file writes for this event.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Process(event) => event.name_text(),
            Self::Gossip(event) => event.name_text(),
            Self::Direct(event) => event.name_text(),
            Self::PeerTable(event) => event.name_text(),
            Self::Transport(event) => event.name_text(),
            Self::InboxAndTools(event) => event.name_text(),
            Self::State(event) => event.name_text(),
        }
    }

    /// The line this event writes to standard error, none for an event of the events file alone.
    #[must_use]
    pub fn run_record_line(&self) -> Option<String> {
        match self {
            Self::Process(event) => event.run_record_line(),
            Self::Gossip(event) => event.run_record_line(),
            Self::Direct(event) => event.run_record_line(),
            Self::PeerTable(event) => event.run_record_line(),
            Self::Transport(event) => event.run_record_line(),
            Self::InboxAndTools(event) => event.run_record_line(),
            Self::State(event) => event.run_record_line(),
        }
    }

    /// The facts this event carries into the events file after its name.
    #[must_use]
    pub fn fields(&self) -> EventFields {
        match self {
            Self::Process(event) => event.fields(),
            Self::Gossip(event) => event.fields(),
            Self::Direct(event) => event.fields(),
            Self::PeerTable(event) => event.fields(),
            Self::Transport(event) => event.fields(),
            Self::InboxAndTools(event) => event.fields(),
            Self::State(event) => event.fields(),
        }
    }
}

impl From<ProcessEvent> for Event {
    fn from(event: ProcessEvent) -> Self {
        Self::Process(event)
    }
}

impl From<GossipEvent> for Event {
    fn from(event: GossipEvent) -> Self {
        Self::Gossip(event)
    }
}

impl From<DirectEvent> for Event {
    fn from(event: DirectEvent) -> Self {
        Self::Direct(event)
    }
}

impl From<PeerTableEvent> for Event {
    fn from(event: PeerTableEvent) -> Self {
        Self::PeerTable(event)
    }
}

impl From<TransportEvent> for Event {
    fn from(event: TransportEvent) -> Self {
        Self::Transport(event)
    }
}

impl From<InboxEvent> for Event {
    fn from(event: InboxEvent) -> Self {
        Self::InboxAndTools(event)
    }
}

impl From<StateEvent> for Event {
    fn from(event: StateEvent) -> Self {
        Self::State(event)
    }
}

/// The one field of an event whose tool call or claim answered with an error code.
fn answered(code: &ErrorCode) -> EventFields {
    vec![(CODE_FIELD, Value::from(code.code()))]
}

/// Every name an event of the binary can carry, area by area.
pub fn every_event_name() -> impl Iterator<Item = &'static str> {
    names_of::<ProcessEventName>()
        .chain(names_of::<GossipEventName>())
        .chain(names_of::<DirectEventName>())
        .chain(names_of::<PeerTableEventName>())
        .chain(names_of::<TransportEventName>())
        .chain(names_of::<InboxEventName>())
        .chain(names_of::<StateEventName>())
}

/// Every name of one group.
pub fn names_of<Name: EventName>() -> impl Iterator<Item = &'static str> {
    Name::ALL.iter().map(|name| name.as_str())
}

/// Writes the event's run-record line to standard error, writing nothing for an event of the events file alone.
///
/// # Errors
/// `io::Error` when standard error refuses the line.
pub fn write_run_record(stderr: &mut impl Write, event: &Event) -> io::Result<()> {
    event
        .run_record_line()
        .map_or(Ok(()), |line| writeln!(stderr, "{line}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const SPEC_FORMAT_VERSION: u64 = 1;
    const PINNED_EVENT_NAMES: [&str; 67] = [
        "process.command_failed",
        "process.network_created",
        "process.clock_before_unix_epoch",
        "process.claim_lock_task_failed",
        "process.claim_network_refused",
        "process.claim_endpoint_unbound",
        "process.claim_node_not_started",
        "process.trace_not_drained",
        "process.trace_not_cut",
        "process.claimed",
        "process.claim_refused",
        "process.input_ended",
        "process.signal_received",
        "process.departure_handed_over",
        "process.lock_released",
        "process.configuration_rewrite_decided",
        "process.held_connections_dropped",
        "process.departure_wait_over",
        "process.started",
        "process.exited",
        "process.panicked",
        "gossip.broadcast_failed",
        "gossip.topic_failed",
        "gossip.topic_closed",
        "gossip.known_hosts_not_saved",
        "gossip.topic_joined",
        "gossip.neighbor_up",
        "gossip.neighbor_down",
        "gossip.payload_accepted",
        "gossip.payload_dropped",
        "gossip.payload_broadcast",
        "peers.entry_created",
        "peers.entry_changed",
        "peers.entry_removed",
        "peers.own_handle_assigned",
        "peers.lookup_replaced",
        "direct.contact_failed",
        "direct.request_sent",
        "direct.request_served",
        "transport.rebinding",
        "transport.router_shutdown_failed",
        "transport.rebind_failed",
        "transport.gate_closed",
        "transport.connection_opened",
        "transport.connection_closing",
        "transport.connection_closed",
        "transport.path_opened",
        "transport.path_closed",
        "transport.path_selected",
        "transport.path_events_missed",
        "transport.home_relay",
        "transport.relay_probed",
        "state.counters",
        "inbox.accept_failed",
        "inbox.frame_delivered",
        "inbox.client_connected",
        "inbox.client_gone",
        "tool.list_cards_registry_unreadable",
        "tool.list_cards_task_failed",
        "tool.handshake_failed",
        "tool.answering_failed",
        "tool.standard_input_failed",
        "tool.answered",
        "tool.send_answered",
        "dropped",
        "raw_file_opened",
        "iroh_line",
    ];

    #[test]
    fn observability_decision_11_every_event_name_is_pinned_and_the_format_version_is_1() {
        let named: Vec<&str> = every_event_name()
            .chain(names_of::<TraceLineName>())
            .collect();
        let distinct: BTreeSet<&str> = named.iter().copied().collect();

        assert_eq!(distinct.len(), named.len(), "{named:?}");
        assert_eq!(distinct, BTreeSet::from(PINNED_EVENT_NAMES));
        assert_eq!(crate::trace::FORMAT_VERSION, SPEC_FORMAT_VERSION);
    }
}
