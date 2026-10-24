use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};
use serde_json::Value;
use std::borrow::Cow;
use std::fmt;

const STATUS_UNCLAIMED: &str = "unclaimed";
const STATUS_CLAIMED: &str = "claimed";
const AMBIGUOUS_HANDLE_MESSAGE: &str = "More than one member matches that handle. Retry shortly.";
const CANDIDATES_LABEL: &str = "Candidates:";
const ENVELOPE_FIELD_COUNT: usize = 3;

/// Who the caller is as the server knows it: nobody yet, or a claimed card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum You {
    Unclaimed,
    Claimed { card: String, handle: String },
}

/// What every tool answers with, success or failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Envelope {
    Success { you: You, result: Value },
    Failure { you: You, error: ErrorCode },
}

/// The closed list of failures a tool may report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    NotClaimed,
    CardNotFound,
    CardHeld,
    CardInvalid,
    SidecarInvalid,
    UnknownHandle,
    AmbiguousHandle { candidates: Vec<String> },
    Unreachable,
    Timeout,
    Refused,
    TooLarge,
    BadRequest,
}

/// How the command line program ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    Success,
    Failure,
    UsageError,
    DoctorFoundFailedCheck,
}

impl From<ExitStatus> for u8 {
    fn from(status: ExitStatus) -> Self {
        match status {
            ExitStatus::Success => 0,
            ExitStatus::Failure => 1,
            ExitStatus::UsageError => 2,
            ExitStatus::DoctorFoundFailedCheck => 3,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for ErrorCode {}

#[derive(Serialize)]
struct YouView<'a> {
    card: Option<&'a str>,
    handle: Option<&'a str>,
    status: &'static str,
}

#[derive(Serialize)]
struct ErrorView {
    code: &'static str,
    message: Cow<'static, str>,
}

impl ErrorCode {
    const fn code(&self) -> &'static str {
        match self {
            Self::NotClaimed => "not_claimed",
            Self::CardNotFound => "card_not_found",
            Self::CardHeld => "card_held",
            Self::CardInvalid => "card_invalid",
            Self::SidecarInvalid => "sidecar_invalid",
            Self::UnknownHandle => "unknown_handle",
            Self::AmbiguousHandle { .. } => "ambiguous_handle",
            Self::Unreachable => "unreachable",
            Self::Timeout => "timeout",
            Self::Refused => "refused",
            Self::TooLarge => "too_large",
            Self::BadRequest => "bad_request",
        }
    }

    fn message(&self) -> Cow<'static, str> {
        match self {
            Self::NotClaimed => Cow::Borrowed("Claim a card with init first."),
            Self::CardNotFound => Cow::Borrowed("No card with that name in the registry."),
            Self::CardHeld => Cow::Borrowed("That card has a live holder."),
            Self::CardInvalid => Cow::Borrowed("The card file did not pass validation."),
            Self::SidecarInvalid => Cow::Borrowed(
                "The sidecar is absent, unreadable, has wrong permissions, or does not match the card.",
            ),
            Self::UnknownHandle => Cow::Borrowed("No present member has that handle."),
            Self::AmbiguousHandle { candidates } => Cow::Owned(format!(
                "{AMBIGUOUS_HANDLE_MESSAGE} {CANDIDATES_LABEL} {}.",
                candidates.join(", ")
            )),
            Self::Unreachable => Cow::Borrowed("Could not connect to that member."),
            Self::Timeout => Cow::Borrowed("The member did not acknowledge in time."),
            Self::Refused => Cow::Borrowed("The member refused the message."),
            Self::TooLarge => Cow::Borrowed("The body exceeds the size limit."),
            Self::BadRequest => Cow::Borrowed("The input did not match the tool schema."),
        }
    }
}

impl Serialize for You {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let view = match self {
            Self::Unclaimed => YouView {
                card: None,
                handle: None,
                status: STATUS_UNCLAIMED,
            },
            Self::Claimed { card, handle } => YouView {
                card: Some(card),
                handle: Some(handle),
                status: STATUS_CLAIMED,
            },
        };
        view.serialize(serializer)
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ErrorView {
            code: self.code(),
            message: self.message(),
        }
        .serialize(serializer)
    }
}

impl Serialize for Envelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut envelope = serializer.serialize_struct("Envelope", ENVELOPE_FIELD_COUNT)?;
        match self {
            Self::Success { you, result } => {
                envelope.serialize_field("ok", &true)?;
                envelope.serialize_field("you", you)?;
                envelope.serialize_field("result", result)?;
            }
            Self::Failure { you, error } => {
                envelope.serialize_field("ok", &false)?;
                envelope.serialize_field("you", you)?;
                envelope.serialize_field("error", error)?;
            }
        }
        envelope.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render(envelope: &Envelope) -> Value {
        serde_json::to_value(envelope).unwrap()
    }

    #[test]
    fn success_envelope_matches_spec_3_10_example() {
        let envelope = Envelope::Success {
            you: You::Claimed {
                card: "ana".to_owned(),
                handle: "ana-2".to_owned(),
            },
            result: json!({ "handle": "ana-2", "inbox_path": "/x/inbox.sock" }),
        };
        assert_eq!(
            render(&envelope),
            json!({
                "ok": true,
                "you": { "card": "ana", "handle": "ana-2", "status": "claimed" },
                "result": { "handle": "ana-2", "inbox_path": "/x/inbox.sock" }
            })
        );
    }

    #[test]
    fn failure_envelope_matches_spec_3_10_example() {
        let envelope = Envelope::Failure {
            you: You::Unclaimed,
            error: ErrorCode::NotClaimed,
        };
        assert_eq!(
            render(&envelope),
            json!({
                "ok": false,
                "you": { "card": null, "handle": null, "status": "unclaimed" },
                "error": { "code": "not_claimed", "message": "Claim a card with init first." }
            })
        );
    }

    #[test]
    fn failure_envelope_keeps_a_claimed_you_for_an_unknown_tool_of_spec_3_10() {
        let envelope = Envelope::Failure {
            you: You::Claimed {
                card: "ana".to_owned(),
                handle: "ana-2".to_owned(),
            },
            error: ErrorCode::BadRequest,
        };
        assert_eq!(
            render(&envelope),
            json!({
                "ok": false,
                "you": { "card": "ana", "handle": "ana-2", "status": "claimed" },
                "error": { "code": "bad_request", "message": "The input did not match the tool schema." }
            })
        );
    }

    #[test]
    fn an_error_prints_its_message_alone_for_the_operator() {
        assert_eq!(
            ErrorCode::CardHeld.to_string(),
            "That card has a live holder."
        );
    }

    #[test]
    fn exit_codes_are_those_of_spec_3_13() {
        let expected = [
            (ExitStatus::Success, 0_u8),
            (ExitStatus::Failure, 1),
            (ExitStatus::UsageError, 2),
            (ExitStatus::DoctorFoundFailedCheck, 3),
        ];
        for (status, code) in expected {
            assert_eq!(u8::from(status), code);
        }
    }

    #[test]
    fn every_error_renders_its_code_and_message_of_spec_3_10() {
        let expected = [
            (
                ErrorCode::NotClaimed,
                "not_claimed",
                "Claim a card with init first.",
            ),
            (
                ErrorCode::CardNotFound,
                "card_not_found",
                "No card with that name in the registry.",
            ),
            (
                ErrorCode::CardHeld,
                "card_held",
                "That card has a live holder.",
            ),
            (
                ErrorCode::CardInvalid,
                "card_invalid",
                "The card file did not pass validation.",
            ),
            (
                ErrorCode::SidecarInvalid,
                "sidecar_invalid",
                "The sidecar is absent, unreadable, has wrong permissions, or does not match the card.",
            ),
            (
                ErrorCode::UnknownHandle,
                "unknown_handle",
                "No present member has that handle.",
            ),
            (
                ErrorCode::AmbiguousHandle {
                    candidates: vec!["ana-2".to_owned(), "bob-7".to_owned()],
                },
                "ambiguous_handle",
                "More than one member matches that handle. Retry shortly. Candidates: ana-2, bob-7.",
            ),
            (
                ErrorCode::Unreachable,
                "unreachable",
                "Could not connect to that member.",
            ),
            (
                ErrorCode::Timeout,
                "timeout",
                "The member did not acknowledge in time.",
            ),
            (
                ErrorCode::Refused,
                "refused",
                "The member refused the message.",
            ),
            (
                ErrorCode::TooLarge,
                "too_large",
                "The body exceeds the size limit.",
            ),
            (
                ErrorCode::BadRequest,
                "bad_request",
                "The input did not match the tool schema.",
            ),
        ];
        for (error, code, message) in expected {
            let envelope = Envelope::Failure {
                you: You::Unclaimed,
                error,
            };
            assert_eq!(
                render(&envelope)["error"],
                json!({ "code": code, "message": message })
            );
        }
    }
}
