use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::trace_reader::{TraceLine, events_lines_in, timeline_in};

/// What the operating system reports about a change under a watched directory.
type Notice = notify::Result<notify::Event>;

/// The events one member of a test has recorded, in the trace directory of its own, awaited on the operating system's notice that its events file grew.
pub struct MemberEvents {
    label: String,
    traces_directory: PathBuf,
    notices: Receiver<Notice>,
    _watcher: RecommendedWatcher,
}

impl MemberEvents {
    /// Watches the member's trace directory, which must exist, for growth of its events files.
    #[track_caller]
    pub fn watch(label: &str, traces_directory: &Path) -> Self {
        let (sender, notices) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |notice: Notice| {
            if wakes_the_wait(&notice) {
                drop(sender.send(notice));
            }
        })
        .unwrap();
        watcher
            .watch(traces_directory, RecursiveMode::NonRecursive)
            .unwrap();
        Self {
            label: label.to_owned(),
            traces_directory: traces_directory.to_path_buf(),
            notices,
            _watcher: watcher,
        }
    }

    /// The first line of the member's events files that names the event, found already written or awaited until the events file grows, failing with the event's name and the member's timeline so far when the bound runs out.
    #[track_caller]
    pub fn wait_for(&self, event: &str, within: Duration) -> TraceLine {
        let deadline = Instant::now().checked_add(within).unwrap();
        loop {
            if let Some(line) = self.lines().into_iter().find(|line| line.event == event) {
                return line;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.notices.recv_timeout(remaining) {
                Ok(notice) => {
                    notice.unwrap_or_else(|failure| {
                        panic!("{}: the file watcher failed: {failure}", self.label)
                    });
                }
                Err(RecvTimeoutError::Timeout) => panic!(
                    "{}: no {event} within {within:?}; its timeline so far:\n{}",
                    self.label,
                    timeline_in(&self.traces_directory)
                ),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("{}: the file watcher ended", self.label)
                }
            }
        }
    }

    /// Every line of the member's events files written so far.
    #[track_caller]
    fn lines(&self) -> Vec<TraceLine> {
        events_lines_in(&self.traces_directory)
            .unwrap_or_else(|failure| panic!("{}: {failure}", self.label))
    }
}

/// Whether the notice may mean an events file grew or says the watch failed, which every notice but a read, an open or a close does.
fn wakes_the_wait(notice: &Notice) -> bool {
    match notice {
        Ok(event) => !event.kind.is_access(),
        Err(_failure) => true,
    }
}

#[cfg(test)]
mod tests {
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind,
        RenameMode,
    };
    use notify::{Event, EventKind};

    use super::wakes_the_wait;

    #[test]
    fn observability_decision_14_a_read_open_or_close_of_the_events_file_does_not_wake_the_wait_and_every_other_notice_does()
     {
        let reads_opens_and_closes = [
            EventKind::Access(AccessKind::Any),
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            EventKind::Access(AccessKind::Open(AccessMode::Read)),
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            EventKind::Access(AccessKind::Other),
        ];
        let writes_and_everything_else = [
            EventKind::Any,
            EventKind::Create(CreateKind::File),
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)),
            EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
            EventKind::Remove(RemoveKind::File),
            EventKind::Other,
        ];

        for kind in reads_opens_and_closes {
            assert!(!wakes_the_wait(&Ok(Event::new(kind))), "{kind:?}");
        }
        for kind in writes_and_everything_else {
            assert!(wakes_the_wait(&Ok(Event::new(kind))), "{kind:?}");
        }
        assert!(wakes_the_wait(&Err(notify::Error::generic(
            "the watch failed"
        ))));
    }
}
