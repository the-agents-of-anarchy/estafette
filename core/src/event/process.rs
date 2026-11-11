use iroh::endpoint::BindError;
use tokio::task::JoinError;

use super::{AreaEvent, EventFields, answered};
use crate::handle::Handle;
use crate::mcp::{LOCK_TASK_FAILED, NODE_NOT_STARTED};
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
    /// Init answered with the lock task's code: the sidecar lock task ended without a result.
    ClaimLockTaskFailed(JoinError),
    /// Init answered with the node's code: the network configuration was refused.
    ClaimNetworkRefused(NodeError),
    /// Init answered with the node's code: the endpoint was not bound.
    ClaimEndpointUnbound(BindError),
    /// Init answered with the node's code: the node did not start.
    ClaimNodeNotStarted(NodeError),
}

impl AreaEvent for ProcessEvent {
    type Name = ProcessEventName;

    fn name(&self) -> ProcessEventName {
        match self {
            Self::CommandFailed(_) => ProcessEventName::CommandFailed,
            Self::NetworkCreated { .. } => ProcessEventName::NetworkCreated,
            Self::ClockBeforeUnixEpoch => ProcessEventName::ClockBeforeUnixEpoch,
            Self::ClaimLockTaskFailed(_) => ProcessEventName::ClaimLockTaskFailed,
            Self::ClaimNetworkRefused(_) => ProcessEventName::ClaimNetworkRefused,
            Self::ClaimEndpointUnbound(_) => ProcessEventName::ClaimEndpointUnbound,
            Self::ClaimNodeNotStarted(_) => ProcessEventName::ClaimNodeNotStarted,
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
            Self::ClaimLockTaskFailed(source) => format!(
                "init answered {}: the sidecar lock task did not finish: {source}",
                LOCK_TASK_FAILED.code()
            ),
            Self::ClaimNetworkRefused(source) | Self::ClaimNodeNotStarted(source) => {
                format!("init answered {}: {source}", NODE_NOT_STARTED.code())
            }
            Self::ClaimEndpointUnbound(source) => format!(
                "init answered {}: the endpoint was not bound: {source}",
                NODE_NOT_STARTED.code()
            ),
        })
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::CommandFailed(_) | Self::NetworkCreated { .. } | Self::ClockBeforeUnixEpoch => {
                Vec::new()
            }
            Self::ClaimLockTaskFailed(_) => answered(&LOCK_TASK_FAILED),
            Self::ClaimNetworkRefused(_)
            | Self::ClaimEndpointUnbound(_)
            | Self::ClaimNodeNotStarted(_) => answered(&NODE_NOT_STARTED),
        }
    }
}
