use std::io;
use std::time::Duration;

use iroh::endpoint::BindError;
use serde_json::Value;
use tokio::task::JoinError;

use super::{AnsweredFailure, AreaEvent, CODE_FIELD, Event, EventFields, answered};
use crate::dirs::TraceFileKind;
use crate::errors::ErrorCode;
use crate::handle::Handle;
use crate::node::{ClaimedCard, NodeError, ShutdownSignal};

/// What the run record says when the machine's clock reads a time before the Unix epoch.
pub const CLOCK_BEFORE_UNIX_EPOCH: &str =
    "the system clock reads a time before 1970-01-01: set the clock and run the command again";

const CARD_FIELD: &str = "card";
const HANDLE_FIELD: &str = "handle";
const SIGNAL_FIELD: &str = "signal";
const WAIT_FIELD: &str = "wait_ms";

super::event_names! {
    /// The names of the process's events: its command, its clock and its claim.
    ProcessEventName {
        CommandFailed => "process.command_failed",
        NetworkCreated => "process.network_created",
        ClockBeforeUnixEpoch => "process.clock_before_unix_epoch",
        ClaimLockTaskFailed => "process.claim_lock_task_failed",
        ClaimNetworkRefused => "process.claim_network_refused",
        ClaimEndpointUnbound => "process.claim_endpoint_unbound",
        ClaimNodeNotStarted => "process.claim_node_not_started",
        TraceNotDrained => "process.trace_not_drained",
        TraceNotCut => "process.trace_not_cut",
        Claimed => "process.claimed",
        ClaimRefused => "process.claim_refused",
        InputEnded => "process.input_ended",
        SignalReceived => "process.signal_received",
        DepartureHandedOver => "process.departure_handed_over",
        LockReleased => "process.lock_released",
        HeldConnectionsDropped => "process.held_connections_dropped",
        DepartureWaitOver => "process.departure_wait_over",
    }
}

/// What the process records of its command, its clock and the claim init makes.
pub enum ProcessEvent {
    /// The command ended in failure, stated for whoever ran it.
    CommandFailed(Box<dyn std::error::Error + Send + Sync>),
    /// `network create` made the network, which names no host yet.
    NetworkCreated { network: Handle },
    /// The machine's clock read a time before the Unix epoch.
    ClockBeforeUnixEpoch,
    /// Init answered with the failure's own code.
    ClaimFailed(ClaimFailure),
    /// The writer of the debug trace's file named did not report within the drain bound at the stop that it took every queued line.
    TraceNotDrained {
        file: TraceFileKind,
        drain: Duration,
    },
    /// The debug trace could not cut the file named back to its last line at the stop.
    TraceNotCut {
        file: TraceFileKind,
        source: io::Error,
    },
    /// The session claimed the card, and the node assigned itself the handle.
    Claimed(ClaimedCard),
    /// The session refused a claim with the code, holding a card already or stopping.
    ClaimRefused { code: &'static str },
    /// Standard input ended, which starts Stopping.
    InputEnded,
    /// A signal arrived: the first starts Stopping, a second exits at once.
    SignalReceived { signal: ShutdownSignal },
    /// The stopping session handed its Departure to the presence loop and refuses every Message from here on.
    DepartureHandedOver,
    /// The stopping session released its card's sidecar lock.
    LockReleased,
    /// The stopping session dropped every direct connection it held.
    HeldConnectionsDropped,
    /// The stopping session waited out the departure wait, so the process may exit.
    DepartureWaitOver { waited: Duration },
}

/// Why init failed once it held or tried the card's sidecar lock, each cause answering its own code.
pub enum ClaimFailure {
    /// The sidecar lock task ended without a result.
    LockTaskFailed(JoinError),
    /// The network configuration was refused.
    NetworkRefused(NodeError),
    /// The endpoint was not bound.
    EndpointUnbound(BindError),
    /// The node did not start.
    NodeNotStarted(NodeError),
}

impl AnsweredFailure for ClaimFailure {
    fn answered_code(&self) -> ErrorCode {
        match self {
            Self::LockTaskFailed(_) => ErrorCode::SidecarInvalid,
            Self::NetworkRefused(_) | Self::EndpointUnbound(_) | Self::NodeNotStarted(_) => {
                ErrorCode::Unreachable
            }
        }
    }
}

impl From<ClaimFailure> for Event {
    fn from(failure: ClaimFailure) -> Self {
        ProcessEvent::ClaimFailed(failure).into()
    }
}

/// The name of a signal as the events file writes it.
const fn signal_name(signal: ShutdownSignal) -> &'static str {
    match signal {
        ShutdownSignal::Terminate => "SIGTERM",
        ShutdownSignal::Interrupt => "SIGINT",
        ShutdownSignal::Hangup => "SIGHUP",
    }
}

/// The word standard error names a trace file by.
const fn trace_file_word(file: TraceFileKind) -> &'static str {
    match file {
        TraceFileKind::Events => "events",
        TraceFileKind::Raw => "raw",
    }
}

impl AreaEvent for ProcessEvent {
    type Name = ProcessEventName;

    fn name(&self) -> ProcessEventName {
        match self {
            Self::CommandFailed(_) => ProcessEventName::CommandFailed,
            Self::NetworkCreated { .. } => ProcessEventName::NetworkCreated,
            Self::ClockBeforeUnixEpoch => ProcessEventName::ClockBeforeUnixEpoch,
            Self::ClaimFailed(ClaimFailure::LockTaskFailed(_)) => {
                ProcessEventName::ClaimLockTaskFailed
            }
            Self::ClaimFailed(ClaimFailure::NetworkRefused(_)) => {
                ProcessEventName::ClaimNetworkRefused
            }
            Self::ClaimFailed(ClaimFailure::EndpointUnbound(_)) => {
                ProcessEventName::ClaimEndpointUnbound
            }
            Self::ClaimFailed(ClaimFailure::NodeNotStarted(_)) => {
                ProcessEventName::ClaimNodeNotStarted
            }
            Self::TraceNotDrained { .. } => ProcessEventName::TraceNotDrained,
            Self::TraceNotCut { .. } => ProcessEventName::TraceNotCut,
            Self::Claimed(_) => ProcessEventName::Claimed,
            Self::ClaimRefused { .. } => ProcessEventName::ClaimRefused,
            Self::InputEnded => ProcessEventName::InputEnded,
            Self::SignalReceived { .. } => ProcessEventName::SignalReceived,
            Self::DepartureHandedOver => ProcessEventName::DepartureHandedOver,
            Self::LockReleased => ProcessEventName::LockReleased,
            Self::HeldConnectionsDropped => ProcessEventName::HeldConnectionsDropped,
            Self::DepartureWaitOver { .. } => ProcessEventName::DepartureWaitOver,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        match self {
            Self::Claimed(_)
            | Self::ClaimRefused { .. }
            | Self::InputEnded
            | Self::SignalReceived { .. }
            | Self::DepartureHandedOver
            | Self::LockReleased
            | Self::HeldConnectionsDropped
            | Self::DepartureWaitOver { .. } => None,
            Self::CommandFailed(failure) => Some(failure.to_string()),
            Self::NetworkCreated { network } => Some(format!(
                "created network {network}: it names no host yet, so it has no invitation until a host is added",
                network = network.as_str()
            )),
            Self::ClockBeforeUnixEpoch => Some(CLOCK_BEFORE_UNIX_EPOCH.to_owned()),
            Self::ClaimFailed(failure) => {
                let code = failure.answered_code();
                Some(match failure {
                    ClaimFailure::LockTaskFailed(source) => format!(
                        "init answered {}: the sidecar lock task did not finish: {source}",
                        code.code()
                    ),
                    ClaimFailure::NetworkRefused(source) | ClaimFailure::NodeNotStarted(source) => {
                        format!("init answered {}: {source}", code.code())
                    }
                    ClaimFailure::EndpointUnbound(source) => format!(
                        "init answered {}: the endpoint was not bound: {source}",
                        code.code()
                    ),
                })
            }
            Self::TraceNotDrained { file, drain } => Some(format!(
                "the debug trace did not finish writing its {} file within {} ms of the stop: the file's last lines may be missing",
                trace_file_word(*file),
                drain.as_millis()
            )),
            Self::TraceNotCut { file, source } => Some(format!(
                "the debug trace could not cut its {} file back to its last line at the stop, so the file ends in zero bytes: {source}",
                trace_file_word(*file)
            )),
        }
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::CommandFailed(_)
            | Self::NetworkCreated { .. }
            | Self::ClockBeforeUnixEpoch
            | Self::TraceNotDrained { .. }
            | Self::TraceNotCut { .. }
            | Self::InputEnded
            | Self::DepartureHandedOver
            | Self::LockReleased
            | Self::HeldConnectionsDropped => Vec::new(),
            Self::ClaimFailed(failure) => answered(&failure.answered_code()),
            Self::Claimed(claimed) => vec![
                (CARD_FIELD, Value::from(claimed.card.as_str())),
                (HANDLE_FIELD, Value::from(claimed.handle.as_str())),
            ],
            Self::ClaimRefused { code } => vec![(CODE_FIELD, Value::from(*code))],
            Self::SignalReceived { signal } => {
                vec![(SIGNAL_FIELD, Value::from(signal_name(*signal)))]
            }
            Self::DepartureWaitOver { waited } => vec![(
                WAIT_FIELD,
                Value::from(u64::try_from(waited.as_millis()).unwrap_or(u64::MAX)),
            )],
        }
    }
}
