use std::io;
use std::path::PathBuf;

use estafette_bridge_claude::discover::Pid;
use estafette_bridge_claude::inbox_client::InboxClientError;
use estafette_bridge_claude::inject::InjectError;
use estafette_bridge_claude::permission_class::PermissionClass;

use crate::{CommandError, LiveClassError, USAGE};

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
}

impl BridgeEvent {
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
        }
    }
}
