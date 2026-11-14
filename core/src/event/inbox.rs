use std::io;

use rmcp::service::ServerInitializeError;
use serde_json::Value;
use tokio::task::JoinError;

use super::{AnsweredFailure, AreaEvent, Event, EventFields, answered};
use crate::errors::ErrorCode;
use crate::mcp::RegistryFailure;
use crate::net::direct::message_id_text;
use crate::wire::MessageId;

const MESSAGE_FIELD: &str = "message";
const FRAME_FIELD: &str = "frame";
const ATTACHED_FIELD: &str = "attached";
const CLIENTS_FIELD: &str = "clients";

super::event_names! {
    /// The names of the inbox and tool events.
    InboxEventName {
        AcceptFailed => "inbox.accept_failed",
        FrameDelivered => "inbox.frame_delivered",
        ClientConnected => "inbox.client_connected",
        ClientGone => "inbox.client_gone",
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
    /// A message entered the inbox as the numbered frame, with a client attached or none, its id absent where its text is no message id.
    FrameDelivered {
        message: Option<MessageId>,
        frame: u64,
        attached: bool,
    },
    /// A client connected to the inbox socket, leaving this many connected.
    ClientConnected { clients: usize },
    /// A client of the inbox socket went, leaving this many connected.
    ClientGone { clients: usize },
    /// `list_cards` answered with the failure's own code.
    ListCardsFailed(ListCardsFailure),
    /// The MCP handshake failed.
    HandshakeFailed(Box<ServerInitializeError>),
    /// The MCP server stopped answering.
    AnsweringFailed(JoinError),
    /// Reading standard input failed and is treated as its end.
    StandardInputFailed(io::Error),
}

/// Why `list_cards` could not list the registry, each cause answering the code a card file that cannot be read gets at init.
pub enum ListCardsFailure {
    /// The card registry could not be read.
    RegistryUnreadable(RegistryFailure),
    /// The listing task did not finish.
    TaskFailed(JoinError),
}

impl AnsweredFailure for ListCardsFailure {
    fn answered_code(&self) -> ErrorCode {
        match self {
            Self::RegistryUnreadable(_) | Self::TaskFailed(_) => ErrorCode::CardInvalid,
        }
    }
}

impl From<ListCardsFailure> for Event {
    fn from(failure: ListCardsFailure) -> Self {
        InboxEvent::ListCardsFailed(failure).into()
    }
}

impl InboxEvent {
    /// A message entered the inbox as the numbered frame, its id read from the text the frame carries it as.
    #[must_use]
    pub fn frame_delivered(message_id: &str, frame: u64, attached: bool) -> Self {
        Self::FrameDelivered {
            message: message_id_of(message_id),
            frame,
            attached,
        }
    }
}

/// The message id the text names, none for a text that names none.
fn message_id_of(text: &str) -> Option<MessageId> {
    uuid::Uuid::try_parse(text).ok().map(uuid::Uuid::into_bytes)
}

/// The message id as the events file writes it, under the key every event of a message's send and receipt shares.
fn message_field(message: MessageId) -> (&'static str, Value) {
    (MESSAGE_FIELD, Value::from(message_id_text(message)))
}

impl AreaEvent for InboxEvent {
    type Name = InboxEventName;

    fn name(&self) -> InboxEventName {
        match self {
            Self::AcceptFailed(_) => InboxEventName::AcceptFailed,
            Self::FrameDelivered { .. } => InboxEventName::FrameDelivered,
            Self::ClientConnected { .. } => InboxEventName::ClientConnected,
            Self::ClientGone { .. } => InboxEventName::ClientGone,
            Self::ListCardsFailed(ListCardsFailure::RegistryUnreadable(_)) => {
                InboxEventName::ListCardsRegistryUnreadable
            }
            Self::ListCardsFailed(ListCardsFailure::TaskFailed(_)) => {
                InboxEventName::ListCardsTaskFailed
            }
            Self::HandshakeFailed(_) => InboxEventName::HandshakeFailed,
            Self::AnsweringFailed(_) => InboxEventName::AnsweringFailed,
            Self::StandardInputFailed(_) => InboxEventName::StandardInputFailed,
        }
    }

    fn run_record_line(&self) -> Option<String> {
        match self {
            Self::FrameDelivered { .. }
            | Self::ClientConnected { .. }
            | Self::ClientGone { .. } => None,
            Self::AcceptFailed(source) => Some(format!(
                "the inbox socket could not accept a client: {source}"
            )),
            Self::ListCardsFailed(failure) => {
                let code = failure.answered_code();
                Some(match failure {
                    ListCardsFailure::RegistryUnreadable(source) => {
                        format!("list_cards answered {}: {source}", code.code())
                    }
                    ListCardsFailure::TaskFailed(source) => format!(
                        "list_cards answered {}: the listing task did not finish: {source}",
                        code.code()
                    ),
                })
            }
            Self::HandshakeFailed(source) => Some(format!(
                "the MCP handshake failed: {}",
                handshake_failure_reason(source)
            )),
            Self::AnsweringFailed(source) => {
                Some(format!("the MCP server stopped answering: {source}"))
            }
            Self::StandardInputFailed(source) => Some(format!(
                "reading standard input failed and is treated as its end: {source}"
            )),
        }
    }

    fn fields(&self) -> EventFields {
        match self {
            Self::ListCardsFailed(failure) => answered(&failure.answered_code()),
            Self::FrameDelivered {
                message,
                frame,
                attached,
            } => message
                .map(message_field)
                .into_iter()
                .chain([
                    (FRAME_FIELD, Value::from(*frame)),
                    (ATTACHED_FIELD, Value::from(*attached)),
                ])
                .collect(),
            Self::ClientConnected { clients } | Self::ClientGone { clients } => {
                vec![(CLIENTS_FIELD, Value::from(*clients))]
            }
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
