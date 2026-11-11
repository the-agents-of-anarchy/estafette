use std::io;

use rmcp::service::ServerInitializeError;
use tokio::task::JoinError;

use super::{AreaEvent, EventFields, answered};
use crate::mcp::{LISTING_TASK_NOT_FINISHED, REGISTRY_UNREADABLE, RegistryFailure};

super::event_names! {
    /// The names of the inbox and tool events.
    InboxEventName {
        AcceptFailed => "inbox.accept_failed",
        ListCardsRegistryUnreadable => "tool.list_cards_registry_unreadable",
        ListCardsTaskFailed => "tool.list_cards_task_failed",
        HandshakeFailed => "tool.handshake_failed",
        AnsweringFailed => "tool.answering_failed",
        StandardInputFailed => "tool.standard_input_failed",
    }
}

/// What a process records of its inbox socket and of the MCP tools it serves.
pub enum InboxEvent {
    /// The inbox socket could not accept a client.
    AcceptFailed(io::Error),
    /// `list_cards` answered with the registry's code: the card registry could not be read.
    ListCardsRegistryUnreadable(RegistryFailure),
    /// `list_cards` answered with the registry's code: the listing task did not finish.
    ListCardsTaskFailed(JoinError),
    /// The MCP handshake failed.
    HandshakeFailed(Box<ServerInitializeError>),
    /// The MCP server stopped answering.
    AnsweringFailed(JoinError),
    /// Reading standard input failed and is treated as its end.
    StandardInputFailed(io::Error),
}

impl AreaEvent for InboxEvent {
    type Name = InboxEventName;

    fn name(&self) -> InboxEventName {
        match self {
            Self::AcceptFailed(_) => InboxEventName::AcceptFailed,
            Self::ListCardsRegistryUnreadable(_) => InboxEventName::ListCardsRegistryUnreadable,
            Self::ListCardsTaskFailed(_) => InboxEventName::ListCardsTaskFailed,
            Self::HandshakeFailed(_) => InboxEventName::HandshakeFailed,
            Self::AnsweringFailed(_) => InboxEventName::AnsweringFailed,
            Self::StandardInputFailed(_) => InboxEventName::StandardInputFailed,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        Some(match self {
            Self::AcceptFailed(source) => {
                format!("the inbox socket could not accept a client: {source}")
            }
            Self::ListCardsRegistryUnreadable(source) => format!(
                "list_cards answered {}: {source}",
                REGISTRY_UNREADABLE.code()
            ),
            Self::ListCardsTaskFailed(source) => format!(
                "list_cards answered {}: the listing task did not finish: {source}",
                LISTING_TASK_NOT_FINISHED.code()
            ),
            Self::HandshakeFailed(source) => format!(
                "the MCP handshake failed: {}",
                handshake_failure_reason(source)
            ),
            Self::AnsweringFailed(source) => {
                format!("the MCP server stopped answering: {source}")
            }
            Self::StandardInputFailed(source) => {
                format!("reading standard input failed and is treated as its end: {source}")
            }
        })
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::ListCardsRegistryUnreadable(_) => answered(&REGISTRY_UNREADABLE),
            Self::ListCardsTaskFailed(_) => answered(&LISTING_TASK_NOT_FINISHED),
            Self::AcceptFailed(_)
            | Self::HandshakeFailed(_)
            | Self::AnsweringFailed(_)
            | Self::StandardInputFailed(_) => Vec::new(),
        }
    }
}

/// Why a handshake failed, said without the client's message that the library's own text prints in full.
fn handshake_failure_reason(failure: &ServerInitializeError) -> String {
    match failure {
        ServerInitializeError::ExpectedInitializeRequest(_) => {
            "the first message was not an initialize request".to_owned()
        }
        other => other.to_string(),
    }
}
