use std::io;
use std::path::PathBuf;
use std::time::Duration;

use estafette_bridge_claude::discover::{Pid, binary_pid_of_socket};
use estafette_bridge_claude::inbox_client::{InboxClientError, InboxFrame};
use estafette_bridge_claude::inject::{InjectError, NoticeOutcome};
use estafette_bridge_claude::permission_class::PermissionClass;
use serde_json::Value;

use crate::{ClassSource, CommandError, LiveClassError, USAGE};

const BRIDGE_VERSION: &str = env!("CARGO_PKG_VERSION");
const VERSION_FIELD: &str = "version";
const BRIDGE_PID_FIELD: &str = "bridge_pid";
const HARNESS_PID_FIELD: &str = "harness_pid";
const BINARY_PID_FIELD: &str = "binary_pid";
const MESSAGE_ID_FIELD: &str = "message_id";
const FRAME_FIELD: &str = "frame";
const REASON_FIELD: &str = "reason";
const CLASS_FIELD: &str = "class";
const SOURCE_FIELD: &str = "source";

/// The facts an event carries into the events file after its name, each a key and a JSON value.
pub type EventFields = Vec<(&'static str, Value)>;

/// Declares the fixed names of the bridge's events: a fieldless enum, every value of it in `ALL`, and each value's name text.
macro_rules! bridge_event_names {
    ($($variant:ident => $text:literal),* $(,)?) => {
        /// The fixed name of each event of the bridge, as the events file writes it.
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum BridgeEventName {
            $($variant),*
        }

        impl BridgeEventName {
            /// Every name an event of the bridge can carry.
            #[cfg(test)]
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// The name as the events file writes it.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text),*
                }
            }
        }
    };
}

bridge_event_names! {
    Started => "process.started",
    UsageRefused => "process.usage_refused",
    AlreadyServed => "process.already_served",
    CommandFailed => "process.command_failed",
    TraceNotDrained => "process.trace_not_drained",
    TraceNotCut => "process.trace_not_cut",
    InboxConnected => "inbox.connected",
    InboxClosed => "inbox.closed",
    InboxLost => "inbox.lost",
    FrameReceived => "frame.received",
    FrameDroppedByRateLimit => "frame.dropped_by_rate_limit",
    FrameDroppedSenderNameRefused => "frame.dropped_sender_name_refused",
    PermissionClassRead => "permission_class.read",
    PermissionClassUnreadable => "permission_class.unreadable",
    PermissionClassAttested => "permission_class.attested",
    PermissionClassNotWritten => "permission_class.not_written",
    NoticeWritten => "notice.written",
    NoticeConnectFailed => "notice.connect_failed",
    NoticeWriteFailed => "notice.write_failed",
    DroppedFramesNotWritten => "dropped_frames.not_written",
}

/// What names one inbox frame in the events file: its message id and its number on its socket, never its sender or body.
#[derive(Debug)]
pub struct FrameIds {
    message_id: String,
    seq: u64,
}

impl FrameIds {
    /// The ids of the frame.
    pub fn of(frame: &InboxFrame) -> Self {
        Self {
            message_id: frame.id.clone(),
            seq: frame.seq,
        }
    }

    /// The frame's message id and number as fields of the events file.
    fn fields(&self) -> EventFields {
        vec![
            (MESSAGE_ID_FIELD, Value::from(self.message_id.as_str())),
            (FRAME_FIELD, Value::from(self.seq)),
        ]
    }
}

/// One event of the bridge: those of `spec/observability.md` decision 10 and every line it writes to standard error.
#[derive(Debug)]
pub enum BridgeEvent {
    /// The bridge started serving the harness process.
    Started { harness_pid: Pid },
    /// The command line was neither `run` nor `doctor` in a form either takes.
    UsageRefused,
    /// Another bridge already serves the harness process, so this one ends at once.
    AlreadyServed { bridge_pid: Pid, harness_pid: Pid },
    /// The command ended in failure, stated for whoever ran it.
    CommandFailed(CommandError),
    /// The writer of the events file did not report within the drain bound at the stop that it took every queued line.
    TraceNotDrained { drain: Duration },
    /// The events file could not be cut back to its last line at the stop.
    TraceNotCut(io::Error),
    /// The inbox socket took the bridge's connection.
    InboxConnected { socket_path: PathBuf },
    /// The inbox socket closed the bridge's connection, which the bridge opens again.
    InboxClosed { socket_path: PathBuf },
    /// The following of one inbox socket ended on an error; the next poll that finds its process follows it again.
    InboxLost {
        socket_path: PathBuf,
        error: InboxClientError,
    },
    /// A frame arrived from an inbox socket.
    FrameReceived {
        socket_path: PathBuf,
        frame: FrameIds,
    },
    /// The rate limit dropped the frame, so no notice was written.
    FrameDroppedByRateLimit { frame: FrameIds },
    /// The session's permission class was read from the source named.
    PermissionClassRead {
        class: PermissionClass,
        source: ClassSource,
    },
    /// The session's permission mode could not be read, so the class attested before stays.
    PermissionClassUnreadable(LiveClassError),
    /// The class attested on the notices changed to this one.
    PermissionClassAttested(PermissionClass),
    /// The attested class could not be written where the doctor reads it.
    PermissionClassNotWritten(io::Error),
    /// The frame's notice was written into the session, attesting the class.
    NoticeWritten {
        frame: FrameIds,
        class: PermissionClass,
    },
    /// The frame's notice was not written into the session.
    NoticeNotWritten { frame: FrameIds, error: InjectError },
    /// The count of dropped frames could not be written where the doctor reads it.
    DroppedFramesNotWritten(io::Error),
}

impl BridgeEvent {
    /// The event of what became of the frame's notice.
    pub fn of_notice(frame: FrameIds, outcome: Result<NoticeOutcome, InjectError>) -> Self {
        match outcome {
            Ok(NoticeOutcome::Written { class }) => Self::NoticeWritten { frame, class },
            Ok(NoticeOutcome::DroppedByRateLimit) => Self::FrameDroppedByRateLimit { frame },
            Err(error) => Self::NoticeNotWritten { frame, error },
        }
    }

    /// The fixed name the events file writes for this event.
    pub const fn name(&self) -> BridgeEventName {
        match self {
            Self::Started { .. } => BridgeEventName::Started,
            Self::UsageRefused => BridgeEventName::UsageRefused,
            Self::AlreadyServed { .. } => BridgeEventName::AlreadyServed,
            Self::CommandFailed(_) => BridgeEventName::CommandFailed,
            Self::TraceNotDrained { .. } => BridgeEventName::TraceNotDrained,
            Self::TraceNotCut(_) => BridgeEventName::TraceNotCut,
            Self::InboxConnected { .. } => BridgeEventName::InboxConnected,
            Self::InboxClosed { .. } => BridgeEventName::InboxClosed,
            Self::InboxLost { .. } => BridgeEventName::InboxLost,
            Self::FrameReceived { .. } => BridgeEventName::FrameReceived,
            Self::FrameDroppedByRateLimit { .. } => BridgeEventName::FrameDroppedByRateLimit,
            Self::PermissionClassRead { .. } => BridgeEventName::PermissionClassRead,
            Self::PermissionClassUnreadable(_) => BridgeEventName::PermissionClassUnreadable,
            Self::PermissionClassAttested(_) => BridgeEventName::PermissionClassAttested,
            Self::PermissionClassNotWritten(_) => BridgeEventName::PermissionClassNotWritten,
            Self::NoticeWritten { .. } => BridgeEventName::NoticeWritten,
            Self::NoticeNotWritten {
                error: InjectError::ConnectFailed(_),
                ..
            } => BridgeEventName::NoticeConnectFailed,
            Self::NoticeNotWritten {
                error: InjectError::WriteFailed(_),
                ..
            } => BridgeEventName::NoticeWriteFailed,
            Self::NoticeNotWritten {
                error: InjectError::SenderNameRefused(_),
                ..
            } => BridgeEventName::FrameDroppedSenderNameRefused,
            Self::DroppedFramesNotWritten(_) => BridgeEventName::DroppedFramesNotWritten,
        }
    }

    /// The facts this event carries into the events file, never a message body, sender, token, path or error text.
    pub fn fields(&self) -> EventFields {
        match self {
            Self::Started { harness_pid } => vec![
                (VERSION_FIELD, Value::from(BRIDGE_VERSION)),
                (HARNESS_PID_FIELD, Value::from(harness_pid.0)),
            ],
            Self::AlreadyServed {
                bridge_pid,
                harness_pid,
            } => vec![
                (BRIDGE_PID_FIELD, Value::from(bridge_pid.0)),
                (HARNESS_PID_FIELD, Value::from(harness_pid.0)),
            ],
            Self::InboxConnected { socket_path } | Self::InboxClosed { socket_path } => {
                binary_pid_field(socket_path)
            }
            Self::InboxLost { socket_path, error } => {
                let mut fields = binary_pid_field(socket_path);
                fields.push((REASON_FIELD, Value::from(inbox_loss_reason(error))));
                fields
            }
            Self::FrameReceived { socket_path, frame } => {
                let mut fields = binary_pid_field(socket_path);
                fields.extend(frame.fields());
                fields
            }
            Self::FrameDroppedByRateLimit { frame } | Self::NoticeNotWritten { frame, .. } => {
                frame.fields()
            }
            Self::PermissionClassRead { class, source } => vec![
                (CLASS_FIELD, Value::from(class.to_string())),
                (SOURCE_FIELD, Value::from(class_source_word(*source))),
            ],
            Self::PermissionClassUnreadable(error) => {
                vec![(REASON_FIELD, Value::from(class_unreadable_reason(error)))]
            }
            Self::PermissionClassAttested(class) => {
                vec![(CLASS_FIELD, Value::from(class.to_string()))]
            }
            Self::NoticeWritten { frame, class } => {
                let mut fields = frame.fields();
                fields.push((CLASS_FIELD, Value::from(class.to_string())));
                fields
            }
            Self::UsageRefused
            | Self::CommandFailed(_)
            | Self::TraceNotDrained { .. }
            | Self::TraceNotCut(_)
            | Self::PermissionClassNotWritten(_)
            | Self::DroppedFramesNotWritten(_) => Vec::new(),
        }
    }

    /// The line this event writes to standard error, none for an event of the events file alone.
    pub fn run_record_line(&self) -> Option<String> {
        match self {
            Self::Started { .. }
            | Self::InboxConnected { .. }
            | Self::InboxClosed { .. }
            | Self::FrameReceived { .. }
            | Self::FrameDroppedByRateLimit { .. }
            | Self::PermissionClassRead { .. }
            | Self::NoticeWritten { .. } => None,
            Self::UsageRefused => Some(USAGE.to_owned()),
            Self::AlreadyServed {
                bridge_pid,
                harness_pid,
            } => Some(format!(
                "bridge {bridge_pid} already serves process {harness_pid}"
            )),
            Self::CommandFailed(error) => Some(error.to_string()),
            Self::TraceNotDrained { drain } => Some(format!(
                "the debug trace did not finish writing its events file within {} ms of the stop: the file's last lines may be missing",
                drain.as_millis()
            )),
            Self::TraceNotCut(error) => Some(format!(
                "the debug trace could not cut its events file back to its last line at the stop, so the file ends in zero bytes: {error}"
            )),
            Self::InboxLost { socket_path, error } => {
                Some(format!("{}: {error}", socket_path.display()))
            }
            Self::PermissionClassUnreadable(error) => Some(format!(
                "cannot read the session's permission mode, keeping the class attested before: {error}"
            )),
            Self::PermissionClassAttested(class) => {
                Some(format!("attested permission class: {class}"))
            }
            Self::PermissionClassNotWritten(error) => {
                Some(format!("cannot write the permission class: {error}"))
            }
            Self::NoticeNotWritten { error, .. } => Some(error.to_string()),
            Self::DroppedFramesNotWritten(error) => {
                Some(format!("cannot write the dropped-frame count: {error}"))
            }
        }
    }
}

/// The pid of the binary process whose inbox socket this is, as a field of the events file, none for a socket not named for a pid.
fn binary_pid_field(socket_path: &std::path::Path) -> EventFields {
    binary_pid_of_socket(socket_path)
        .map(|pid| (BINARY_PID_FIELD, Value::from(pid.0)))
        .into_iter()
        .collect()
}

/// The word the events file names an inbox socket's loss by.
const fn inbox_loss_reason(error: &InboxClientError) -> &'static str {
    match error {
        InboxClientError::ConnectFailed(_) => "connect_failed",
        InboxClientError::ResumeRequestFailed(_) => "resume_request_failed",
        InboxClientError::ReadFailed(_) => "read_failed",
        InboxClientError::MalformedFrame(_) => "malformed_frame",
        InboxClientError::FrameLineTooLong => "frame_line_too_long",
    }
}

/// The word the events file names where the session's permission class was read from.
const fn class_source_word(source: ClassSource) -> &'static str {
    match source {
        ClassSource::TranscriptTail => "transcript_tail",
        ClassSource::WholeTranscript => "whole_transcript",
        ClassSource::KeptFromBefore => "kept_from_before",
    }
}

/// The word the events file names why the session's permission mode could not be read.
const fn class_unreadable_reason(error: &LiveClassError) -> &'static str {
    match error {
        LiveClassError::RegistryUnreadable { .. } => "registry_unreadable",
        LiveClassError::NoSessionId(_) => "no_session_id",
        LiveClassError::ProjectsUnreadable { .. } => "projects_unreadable",
        LiveClassError::TranscriptMissing(_) => "transcript_missing",
        LiveClassError::TranscriptUnreadable { .. } => "transcript_unreadable",
        LiveClassError::NoModeEntry(_) => "no_mode_entry",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const SPEC_FORMAT_VERSION: u64 = 1;
    const SPEC_DROPPED_LINE_NAME: &str = "dropped";
    const PINNED_EVENT_NAMES: [&str; 20] = [
        "process.started",
        "process.usage_refused",
        "process.already_served",
        "process.command_failed",
        "process.trace_not_drained",
        "process.trace_not_cut",
        "inbox.connected",
        "inbox.closed",
        "inbox.lost",
        "frame.received",
        "frame.dropped_by_rate_limit",
        "frame.dropped_sender_name_refused",
        "permission_class.read",
        "permission_class.unreadable",
        "permission_class.attested",
        "permission_class.not_written",
        "notice.written",
        "notice.connect_failed",
        "notice.write_failed",
        "dropped_frames.not_written",
    ];

    #[test]
    fn observability_decision_11_every_bridge_event_name_is_pinned_apart_from_the_writers_own_line_and_the_format_version_is_1()
     {
        let named: Vec<&str> = BridgeEventName::ALL
            .iter()
            .map(|name| name.as_str())
            .collect();
        let distinct: BTreeSet<&str> = named.iter().copied().collect();

        assert_eq!(distinct.len(), named.len(), "{named:?}");
        assert_eq!(distinct, BTreeSet::from(PINNED_EVENT_NAMES));
        assert_eq!(estafette_trace::DROPPED_LINE_NAME, SPEC_DROPPED_LINE_NAME);
        assert!(!distinct.contains(estafette_trace::DROPPED_LINE_NAME));
        assert_eq!(estafette_trace::FORMAT_VERSION, SPEC_FORMAT_VERSION);
    }
}
