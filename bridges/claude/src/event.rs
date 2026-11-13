use std::io;
use std::path::PathBuf;
use std::time::Duration;

use estafette_bridge_claude::discover::Pid;
use estafette_bridge_claude::inbox_client::InboxClientError;
use estafette_bridge_claude::inject::InjectError;
use estafette_bridge_claude::permission_class::PermissionClass;
use serde_json::Value;

use crate::{CommandError, LiveClassError, USAGE};

const BRIDGE_PID_FIELD: &str = "bridge_pid";
const HARNESS_PID_FIELD: &str = "harness_pid";
const REASON_FIELD: &str = "reason";
const CLASS_FIELD: &str = "class";

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
    UsageRefused => "process.usage_refused",
    AlreadyServed => "process.already_served",
    CommandFailed => "process.command_failed",
    InboxLost => "inbox.lost",
    PermissionClassUnreadable => "permission_class.unreadable",
    PermissionClassAttested => "permission_class.attested",
    PermissionClassNotWritten => "permission_class.not_written",
    NoticeConnectFailed => "notice.connect_failed",
    NoticeWriteFailed => "notice.write_failed",
    FrameDroppedSenderNameRefused => "frame.dropped_sender_name_refused",
    DroppedFramesNotWritten => "dropped_frames.not_written",
    TraceNotDrained => "process.trace_not_drained",
    TraceNotCut => "process.trace_not_cut",
}

/// One event of the bridge, each line it writes to standard error among them.
#[derive(Debug)]
pub enum BridgeEvent {
    /// The command line was neither `run` nor `doctor` in a form either takes.
    UsageRefused,
    /// Another bridge already serves the harness process, so this one ends at once.
    AlreadyServed { bridge_pid: Pid, harness_pid: Pid },
    /// The command ended in failure, stated for whoever ran it.
    CommandFailed(CommandError),
    /// The following of one inbox socket ended on an error; the next poll that finds its process follows it again.
    InboxLost {
        socket_path: PathBuf,
        error: InboxClientError,
    },
    /// The session's permission mode could not be read, so the class attested before stays.
    PermissionClassUnreadable(LiveClassError),
    /// The class attested on the notices changed to this one.
    PermissionClassAttested(PermissionClass),
    /// The attested class could not be written where the doctor reads it.
    PermissionClassNotWritten(io::Error),
    /// A frame's notice was not written into the session.
    NoticeNotWritten(InjectError),
    /// The count of dropped frames could not be written where the doctor reads it.
    DroppedFramesNotWritten(io::Error),
    /// The writer of the events file did not report within the drain bound at the stop that it took every queued line.
    TraceNotDrained { drain: Duration },
    /// The events file could not be cut back to its last line at the stop.
    TraceNotCut(io::Error),
}

impl BridgeEvent {
    /// The fixed name the events file writes for this event.
    pub const fn name(&self) -> BridgeEventName {
        match self {
            Self::UsageRefused => BridgeEventName::UsageRefused,
            Self::AlreadyServed { .. } => BridgeEventName::AlreadyServed,
            Self::CommandFailed(_) => BridgeEventName::CommandFailed,
            Self::InboxLost { .. } => BridgeEventName::InboxLost,
            Self::PermissionClassUnreadable(_) => BridgeEventName::PermissionClassUnreadable,
            Self::PermissionClassAttested(_) => BridgeEventName::PermissionClassAttested,
            Self::PermissionClassNotWritten(_) => BridgeEventName::PermissionClassNotWritten,
            Self::NoticeNotWritten(InjectError::ConnectFailed(_)) => {
                BridgeEventName::NoticeConnectFailed
            }
            Self::NoticeNotWritten(InjectError::WriteFailed(_)) => {
                BridgeEventName::NoticeWriteFailed
            }
            Self::NoticeNotWritten(InjectError::SenderNameRefused(_)) => {
                BridgeEventName::FrameDroppedSenderNameRefused
            }
            Self::DroppedFramesNotWritten(_) => BridgeEventName::DroppedFramesNotWritten,
            Self::TraceNotDrained { .. } => BridgeEventName::TraceNotDrained,
            Self::TraceNotCut(_) => BridgeEventName::TraceNotCut,
        }
    }

    /// The facts this event carries into the events file, never a message body, token, path or error text.
    pub fn fields(&self) -> EventFields {
        match self {
            Self::AlreadyServed {
                bridge_pid,
                harness_pid,
            } => vec![
                (BRIDGE_PID_FIELD, Value::from(bridge_pid.0)),
                (HARNESS_PID_FIELD, Value::from(harness_pid.0)),
            ],
            Self::InboxLost { error, .. } => {
                vec![(REASON_FIELD, Value::from(inbox_loss_reason(error)))]
            }
            Self::PermissionClassUnreadable(error) => {
                vec![(REASON_FIELD, Value::from(class_unreadable_reason(error)))]
            }
            Self::PermissionClassAttested(class) => {
                vec![(CLASS_FIELD, Value::from(class.to_string()))]
            }
            Self::UsageRefused
            | Self::CommandFailed(_)
            | Self::PermissionClassNotWritten(_)
            | Self::NoticeNotWritten(_)
            | Self::DroppedFramesNotWritten(_)
            | Self::TraceNotDrained { .. }
            | Self::TraceNotCut(_) => Vec::new(),
        }
    }

    /// The line this event writes to standard error.
    pub fn run_record_line(&self) -> String {
        match self {
            Self::UsageRefused => USAGE.to_owned(),
            Self::AlreadyServed {
                bridge_pid,
                harness_pid,
            } => format!("bridge {bridge_pid} already serves process {harness_pid}"),
            Self::CommandFailed(error) => error.to_string(),
            Self::InboxLost { socket_path, error } => {
                format!("{}: {error}", socket_path.display())
            }
            Self::PermissionClassUnreadable(error) => format!(
                "cannot read the session's permission mode, keeping the class attested before: {error}"
            ),
            Self::PermissionClassAttested(class) => format!("attested permission class: {class}"),
            Self::PermissionClassNotWritten(error) => {
                format!("cannot write the permission class: {error}")
            }
            Self::NoticeNotWritten(error) => error.to_string(),
            Self::DroppedFramesNotWritten(error) => {
                format!("cannot write the dropped-frame count: {error}")
            }
            Self::TraceNotDrained { drain } => format!(
                "the debug trace did not finish writing its events file within {} ms of the stop: the file's last lines may be missing",
                drain.as_millis()
            ),
            Self::TraceNotCut(error) => format!(
                "the debug trace could not cut its events file back to its last line at the stop, so the file ends in zero bytes: {error}"
            ),
        }
    }
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
    const PINNED_EVENT_NAMES: [&str; 13] = [
        "process.usage_refused",
        "process.already_served",
        "process.command_failed",
        "process.trace_not_drained",
        "process.trace_not_cut",
        "inbox.lost",
        "permission_class.unreadable",
        "permission_class.attested",
        "permission_class.not_written",
        "notice.connect_failed",
        "notice.write_failed",
        "frame.dropped_sender_name_refused",
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
