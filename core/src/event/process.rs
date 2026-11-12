use std::time::Duration;

use iroh::endpoint::BindError;
use tokio::task::JoinError;

use super::{AnsweredFailure, AreaEvent, Event, EventFields, answered};
use crate::errors::ErrorCode;
use crate::handle::Handle;
use crate::node::NodeError;

/// What the run record says when the machine's clock reads a time before the Unix epoch.
pub const CLOCK_BEFORE_UNIX_EPOCH: &str =
    "the system clock reads a time before 1970-01-01: set the clock and run the command again";

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
    /// The debug trace was still writing when its drain bound ran out at the stop.
    TraceNotDrained { drain: Duration },
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
        }
    }

    fn run_record_line(&self) -> Option<String> {
        Some(match self {
            Self::CommandFailed(failure) => failure.to_string(),
            Self::NetworkCreated { network } => format!(
                "created network {network}: it names no host yet, so it has no invitation until a host is added",
                network = network.as_str()
            ),
            Self::ClockBeforeUnixEpoch => CLOCK_BEFORE_UNIX_EPOCH.to_owned(),
            Self::ClaimFailed(failure) => {
                let code = failure.answered_code();
                match failure {
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
                }
            }
            Self::TraceNotDrained { drain } => format!(
                "the debug trace did not finish writing within {} ms of the stop: its last lines may be missing from its files",
                drain.as_millis()
            ),
        })
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::CommandFailed(_)
            | Self::NetworkCreated { .. }
            | Self::ClockBeforeUnixEpoch
            | Self::TraceNotDrained { .. } => Vec::new(),
            Self::ClaimFailed(failure) => answered(&failure.answered_code()),
        }
    }
}
