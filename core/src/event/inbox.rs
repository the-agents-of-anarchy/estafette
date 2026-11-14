use std::io;

use rmcp::service::ServerInitializeError;
use serde_json::Value;
use tokio::task::JoinError;

use super::{AnsweredFailure, AreaEvent, Event, EventFields, answered};
use crate::errors::ErrorCode;
use crate::handle::LocalHandle;
use crate::mcp::{RegistryFailure, ToolName};
use crate::net::direct::message_id_text;
use crate::node::{Delivery, Undelivered};
use crate::wire::MessageId;

const MESSAGE_FIELD: &str = "message";
const FRAME_FIELD: &str = "frame";
const ATTACHED_FIELD: &str = "attached";
const CLIENTS_FIELD: &str = "clients";
const TOOL_FIELD: &str = "tool";
const TO_FIELD: &str = "to";
const OUTCOME_FIELD: &str = "outcome";
const OK_OUTCOME: &str = "ok";
const ERROR_OUTCOME: &str = "error";

super::event_names! {
    /// The names of the inbox and tool events.
    InboxEventName {
        AcceptFailed => "inbox.accept_failed",
        FrameDelivered => "inbox.frame_delivered",
        ClientConnected => "inbox.client_connected",
        ClientGone => "inbox.client_gone",
        ToolAnswered => "tool.answered",
        SendAnswered => "tool.send_answered",
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
    /// A message entered the inbox as the numbered frame, with a client attached or none.
    FrameDelivered {
        message: MessageId,
        frame: u64,
        attached: bool,
    },
    /// A client connected to the inbox socket, leaving this many connected.
    ClientConnected { clients: usize },
    /// A client of the inbox socket went, leaving this many connected.
    ClientGone { clients: usize },
    /// A tool call was answered: the tool by name, none for a name no tool has, and the error code it failed with, none when it answered ok.
    ToolAnswered {
        tool: Option<ToolName>,
        code: Option<ErrorCode>,
    },
    /// One send of a send or broadcast call was answered: the handle it went to, none for a `to` that is no handle, and the id of the message it delivered or why it was not delivered.
    SendAnswered {
        to: Option<LocalHandle>,
        outcome: Result<MessageId, Undelivered>,
    },
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
    /// A call of the tool, none for a name no tool has, answered with the outcome.
    #[must_use]
    pub fn tool_answered<Answer>(
        tool: Option<ToolName>,
        outcome: &Result<Answer, ErrorCode>,
    ) -> Self {
        Self::ToolAnswered {
            tool,
            code: outcome.as_ref().err().cloned(),
        }
    }

    /// A message entered the inbox as the numbered frame.
    #[must_use]
    pub const fn frame_delivered(message: MessageId, frame: u64, attached: bool) -> Self {
        Self::FrameDelivered {
            message,
            frame,
            attached,
        }
    }

    /// One send to `to` answered with the outcome.
    #[must_use]
    pub fn send_answered(to: &str, outcome: &Result<Delivery, Undelivered>) -> Self {
        Self::SendAnswered {
            to: to.parse().ok(),
            outcome: outcome
                .as_ref()
                .map(|delivery| delivery.id)
                .map_err(Clone::clone),
        }
    }
}

/// Whether a tool or a send answered ok or failed, and with which code.
fn outcome_fields(code: Option<&ErrorCode>) -> EventFields {
    code.map_or_else(
        || vec![(OUTCOME_FIELD, Value::from(OK_OUTCOME))],
        |code| {
            [
                vec![(OUTCOME_FIELD, Value::from(ERROR_OUTCOME))],
                answered(code),
            ]
            .concat()
        },
    )
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
            Self::ToolAnswered { .. } => InboxEventName::ToolAnswered,
            Self::SendAnswered { .. } => InboxEventName::SendAnswered,
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
            | Self::ClientGone { .. }
            | Self::ToolAnswered { .. }
            | Self::SendAnswered { .. } => None,
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
            } => vec![
                message_field(*message),
                (FRAME_FIELD, Value::from(*frame)),
                (ATTACHED_FIELD, Value::from(*attached)),
            ],
            Self::ClientConnected { clients } | Self::ClientGone { clients } => {
                vec![(CLIENTS_FIELD, Value::from(*clients))]
            }
            Self::ToolAnswered { tool, code } => [
                vec![(
                    TOOL_FIELD,
                    tool.map_or(Value::Null, |tool| Value::from(tool.name())),
                )],
                outcome_fields(code.as_ref()),
            ]
            .concat(),
            Self::SendAnswered { to, outcome } => {
                let (code, message) = match outcome {
                    Ok(message) => (None, Some(*message)),
                    Err(undelivered) => (Some(undelivered.code()), undelivered.id()),
                };
                [
                    vec![(
                        TO_FIELD,
                        to.as_ref()
                            .map_or(Value::Null, |handle| Value::from(handle.as_str())),
                    )],
                    outcome_fields(code),
                    message.map(message_field).into_iter().collect(),
                ]
                .concat()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::CODE_FIELD;

    const MESSAGE_ID_TEXT: &str = "0193a5d2-7c41-7e0a-9b3f-4d5e6f708192";
    const FRAME: u64 = 7;

    fn message_id() -> MessageId {
        uuid::Uuid::parse_str(MESSAGE_ID_TEXT).unwrap().into_bytes()
    }

    #[test]
    fn observability_decision_9_a_delivered_frame_and_a_delivered_send_each_hold_and_write_the_id_of_the_message_they_carry()
     {
        let frame = InboxEvent::FrameDelivered {
            message: message_id(),
            frame: FRAME,
            attached: false,
        }
        .fields();
        let send = InboxEvent::SendAnswered {
            to: None,
            outcome: Ok(message_id()),
        }
        .fields();

        assert_eq!(
            frame,
            vec![
                (MESSAGE_FIELD, Value::from(MESSAGE_ID_TEXT)),
                (FRAME_FIELD, Value::from(FRAME)),
                (ATTACHED_FIELD, Value::from(false)),
            ]
        );
        assert_eq!(
            send,
            vec![
                (TO_FIELD, Value::Null),
                (OUTCOME_FIELD, Value::from(OK_OUTCOME)),
                (MESSAGE_FIELD, Value::from(MESSAGE_ID_TEXT)),
            ]
        );
    }

    #[test]
    fn observability_decision_9_a_send_that_failed_after_minting_writes_its_message_id_and_one_that_failed_before_writes_none()
     {
        let after_mint = InboxEvent::SendAnswered {
            to: None,
            outcome: Err(Undelivered::AfterMint {
                code: ErrorCode::Timeout,
                id: message_id(),
            }),
        }
        .fields();
        let before_mint = InboxEvent::SendAnswered {
            to: None,
            outcome: Err(Undelivered::BeforeMint(ErrorCode::TooLarge)),
        }
        .fields();

        assert_eq!(
            after_mint,
            vec![
                (TO_FIELD, Value::Null),
                (OUTCOME_FIELD, Value::from(ERROR_OUTCOME)),
                (CODE_FIELD, Value::from("timeout")),
                (MESSAGE_FIELD, Value::from(MESSAGE_ID_TEXT)),
            ]
        );
        assert_eq!(
            before_mint,
            vec![
                (TO_FIELD, Value::Null),
                (OUTCOME_FIELD, Value::from(ERROR_OUTCOME)),
                (CODE_FIELD, Value::from("too_large")),
            ]
        );
    }

    #[test]
    fn observability_decision_9_a_tool_call_holds_its_tool_and_its_error_code_and_writes_their_names()
     {
        let fields = InboxEvent::ToolAnswered {
            tool: Some(ToolName::Broadcast),
            code: Some(ErrorCode::NotClaimed),
        }
        .fields();

        assert_eq!(
            fields,
            vec![
                (TOOL_FIELD, Value::from("broadcast")),
                (OUTCOME_FIELD, Value::from(ERROR_OUTCOME)),
                (CODE_FIELD, Value::from("not_claimed")),
            ]
        );
    }
}
