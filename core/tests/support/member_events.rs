use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::trace_reader::{TraceLine, events_files_in, events_lines_in, timeline_in};

/// What the operating system reports about a change under a watched path.
type Notice = notify::Result<notify::Event>;

/// The events one member of a test records in the trace directory of its own, awaited on the operating system's notice that an events file grew.
pub struct MemberEvents {
    label: String,
    traces_directory: PathBuf,
}

/// The operating system's notices about a trace directory and the events files in it, with the files already watched.
struct EventsWatch {
    watcher: RecommendedWatcher,
    notices: Receiver<Notice>,
    watched_files: BTreeSet<PathBuf>,
}

impl MemberEvents {
    /// The events the member of that label records in the trace directory, which must exist when a wait begins.
    pub fn new(label: &str, traces_directory: &Path) -> Self {
        Self {
            label: label.to_owned(),
            traces_directory: traces_directory.to_path_buf(),
        }
    }

    /// The first line of the member's events files that names the event, found already written or awaited until an events file grows, failing with the event's name and the member's timeline so far when the bound runs out.
    #[track_caller]
    pub fn wait_for(&self, event: &str, within: Duration) -> TraceLine {
        self.wait_for_after(event, within, || {})
    }

    /// As `wait_for`, running `once_waiting` once the first look found no such line, so a test can write the line the wait must then be woken for.
    #[track_caller]
    pub fn wait_for_after(
        &self,
        event: &str,
        within: Duration,
        once_waiting: impl FnOnce(),
    ) -> TraceLine {
        let deadline = Instant::now().checked_add(within).unwrap();
        let mut watch = self.watch_directory();
        let mut once_waiting = Some(once_waiting);
        loop {
            self.watch_new_events_files(&mut watch);
            if let Some(line) = self.lines().into_iter().find(|line| line.event == event) {
                return line;
            }
            if let Some(write_while_waiting) = once_waiting.take() {
                write_while_waiting();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match watch.notices.recv_timeout(remaining) {
                Ok(notice) => {
                    notice.unwrap_or_else(|failure| {
                        panic!("{}: the file watcher failed: {failure}", self.label)
                    });
                    assert!(Instant::now() < deadline, "{}", self.ran_out(event, within));
                }
                Err(RecvTimeoutError::Timeout) => panic!("{}", self.ran_out(event, within)),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("{}: the file watcher ended", self.label)
                }
            }
        }
    }

    /// What a wait says when its bound ran out: the event awaited and the member's timeline so far.
    fn ran_out(&self, event: &str, within: Duration) -> String {
        format!(
            "{}: no {event} within {within:?}; its timeline so far:\n{}",
            self.label,
            timeline_in(&self.traces_directory)
        )
    }

    /// Starts watching the trace directory for the files that appear in it.
    #[track_caller]
    fn watch_directory(&self) -> EventsWatch {
        let (sender, notices) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |notice: Notice| {
            if wakes_the_wait(&notice) {
                drop(sender.send(notice));
            }
        })
        .unwrap_or_else(|failure| {
            panic!("{}: cannot start the file watcher: {failure}", self.label)
        });
        watcher
            .watch(&self.traces_directory, RecursiveMode::NonRecursive)
            .unwrap_or_else(|failure| {
                panic!(
                    "{}: cannot watch {}: {failure}",
                    self.label,
                    self.traces_directory.display()
                )
            });
        EventsWatch {
            watcher,
            notices,
            watched_files: BTreeSet::new(),
        }
    }

    /// Starts watching each events file of the directory not yet watched, because a kqueue reports a file's growth to a watch on the file alone.
    #[track_caller]
    fn watch_new_events_files(&self, watch: &mut EventsWatch) {
        let events_files = events_files_in(&self.traces_directory)
            .unwrap_or_else(|failure| panic!("{}: {failure}", self.label));
        for file in events_files {
            if watch.watched_files.insert(file.clone()) {
                watch
                    .watcher
                    .watch(&file, RecursiveMode::NonRecursive)
                    .unwrap_or_else(|failure| {
                        panic!("{}: cannot watch {}: {failure}", self.label, file.display())
                    });
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
    use std::io;

    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind,
        RenameMode,
    };
    use notify::{Config, Error, ErrorKind, Event, EventKind};

    use super::wakes_the_wait;

    /// How many kinds of notice the pinned notify-types names, counted from its enums.
    const EVERY_KIND_COUNT: usize = 41;

    /// Every kind of notice notify-types can name, each once: the cross product of its kinds and their sub-kinds.
    fn every_event_kind() -> Vec<EventKind> {
        let access_modes = [
            AccessMode::Any,
            AccessMode::Execute,
            AccessMode::Read,
            AccessMode::Write,
            AccessMode::Other,
        ];
        let accesses = [AccessKind::Any, AccessKind::Read, AccessKind::Other]
            .into_iter()
            .chain(access_modes.map(AccessKind::Open))
            .chain(access_modes.map(AccessKind::Close))
            .map(EventKind::Access);
        let creations = [
            CreateKind::Any,
            CreateKind::File,
            CreateKind::Folder,
            CreateKind::Other,
        ]
        .map(EventKind::Create);
        let data_changes = [
            DataChange::Any,
            DataChange::Size,
            DataChange::Content,
            DataChange::Other,
        ]
        .map(ModifyKind::Data);
        let metadata_changes = [
            MetadataKind::Any,
            MetadataKind::AccessTime,
            MetadataKind::WriteTime,
            MetadataKind::Permissions,
            MetadataKind::Ownership,
            MetadataKind::Extended,
            MetadataKind::Other,
        ]
        .map(ModifyKind::Metadata);
        let renames = [
            RenameMode::Any,
            RenameMode::To,
            RenameMode::From,
            RenameMode::Both,
            RenameMode::Other,
        ]
        .map(ModifyKind::Name);
        let modifications = [ModifyKind::Any, ModifyKind::Other]
            .into_iter()
            .chain(data_changes)
            .chain(metadata_changes)
            .chain(renames)
            .map(EventKind::Modify);
        let removals = [
            RemoveKind::Any,
            RemoveKind::File,
            RemoveKind::Folder,
            RemoveKind::Other,
        ]
        .map(EventKind::Remove);
        [EventKind::Any, EventKind::Other]
            .into_iter()
            .chain(accesses)
            .chain(creations)
            .chain(modifications)
            .chain(removals)
            .collect()
    }

    #[test]
    fn observability_decision_14_a_read_open_or_close_of_the_events_file_does_not_wake_the_wait_and_every_other_notice_does()
     {
        let kinds = every_event_kind();
        let failures = [
            ErrorKind::Generic("the watch failed".to_owned()),
            ErrorKind::Io(io::Error::other("the watch failed")),
            ErrorKind::PathNotFound,
            ErrorKind::WatchNotFound,
            ErrorKind::InvalidConfig(Config::default()),
            ErrorKind::MaxFilesWatch,
        ];

        assert_eq!(kinds.len(), EVERY_KIND_COUNT);
        for kind in &kinds {
            assert_eq!(
                kinds.iter().filter(|other| *other == kind).count(),
                1,
                "{kind:?}"
            );
            let is_read_open_or_close = matches!(kind, EventKind::Access(_));
            assert_eq!(
                wakes_the_wait(&Ok(Event::new(*kind))),
                !is_read_open_or_close,
                "{kind:?}"
            );
        }
        for failure in failures {
            let notice = Err(Error::new(failure));
            assert!(wakes_the_wait(&notice), "{notice:?}");
        }
    }
}
