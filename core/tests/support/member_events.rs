use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

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

    /// The first line of the member's events files that names the event, found already written or by a look that an events file's growth wakes and that ends inside the bound, failing with the event's name and the member's timeline so far otherwise.
    #[track_caller]
    pub fn wait_for(&self, event: &str, within: Duration) -> TraceLine {
        self.wait_for_after(event, within, Instant::now, || {})
    }

    /// As `wait_for`, reading the time from `clock` and running `once_waiting` once the first look found no such line, so a test can write the line the wait must then be woken for.
    #[track_caller]
    pub fn wait_for_after(
        &self,
        event: &str,
        within: Duration,
        mut clock: impl FnMut() -> Instant,
        once_waiting: impl FnOnce(),
    ) -> TraceLine {
        let deadline = clock().checked_add(within).unwrap();
        let mut watch = self.watch_directory();
        if let Some(line) = self.look_for(event, &mut watch) {
            return line;
        }
        once_waiting();
        loop {
            let remaining = deadline.saturating_duration_since(clock());
            match watch.notices.recv_timeout(remaining) {
                Ok(notice) => {
                    let notice = notice.unwrap_or_else(|failure| {
                        panic!("{}: the file watcher failed: {failure}", self.label)
                    });
                    assert!(clock() < deadline, "{}", self.ran_out(event, within));
                    forget_files_renamed_or_removed(&mut watch.watched_files, &notice);
                }
                Err(RecvTimeoutError::Timeout) => panic!("{}", self.ran_out(event, within)),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("{}: the file watcher ended", self.label)
                }
            }
            if let Some(line) = self.look_for(event, &mut watch) {
                assert!(clock() < deadline, "{}", self.ran_out(event, within));
                return line;
            }
        }
    }

    /// The first line of the member's events files that names the event, after watching each events file not yet watched.
    #[track_caller]
    fn look_for(&self, event: &str, watch: &mut EventsWatch) -> Option<TraceLine> {
        self.watch_new_events_files(watch);
        self.lines().into_iter().find(|line| line.event == event)
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

/// Forgets each watched file the notice says was renamed or removed, so the next look watches the file that takes its name, as an events file's does when it rolls over.
fn forget_files_renamed_or_removed(watched_files: &mut BTreeSet<PathBuf>, notice: &notify::Event) {
    if matches!(
        notice.kind,
        EventKind::Modify(ModifyKind::Name(_)) | EventKind::Remove(_)
    ) {
        for path in &notice.paths {
            watched_files.remove(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io;
    use std::path::PathBuf;

    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind,
        RenameMode,
    };
    use notify::{Config, Error, ErrorKind, Event, EventKind};

    use super::{forget_files_renamed_or_removed, wakes_the_wait};

    /// Where the kind stands in `every_event_kind`, naming every kind notify-types has, so a kind it adds breaks the build.
    fn position_among_every_kind(kind: EventKind) -> usize {
        match kind {
            EventKind::Any => 0,
            EventKind::Other => 1,
            EventKind::Access(AccessKind::Any) => 2,
            EventKind::Access(AccessKind::Read) => 3,
            EventKind::Access(AccessKind::Other) => 4,
            EventKind::Access(AccessKind::Open(AccessMode::Any)) => 5,
            EventKind::Access(AccessKind::Open(AccessMode::Execute)) => 6,
            EventKind::Access(AccessKind::Open(AccessMode::Read)) => 7,
            EventKind::Access(AccessKind::Open(AccessMode::Write)) => 8,
            EventKind::Access(AccessKind::Open(AccessMode::Other)) => 9,
            EventKind::Access(AccessKind::Close(AccessMode::Any)) => 10,
            EventKind::Access(AccessKind::Close(AccessMode::Execute)) => 11,
            EventKind::Access(AccessKind::Close(AccessMode::Read)) => 12,
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => 13,
            EventKind::Access(AccessKind::Close(AccessMode::Other)) => 14,
            EventKind::Create(CreateKind::Any) => 15,
            EventKind::Create(CreateKind::File) => 16,
            EventKind::Create(CreateKind::Folder) => 17,
            EventKind::Create(CreateKind::Other) => 18,
            EventKind::Modify(ModifyKind::Any) => 19,
            EventKind::Modify(ModifyKind::Other) => 20,
            EventKind::Modify(ModifyKind::Data(DataChange::Any)) => 21,
            EventKind::Modify(ModifyKind::Data(DataChange::Size)) => 22,
            EventKind::Modify(ModifyKind::Data(DataChange::Content)) => 23,
            EventKind::Modify(ModifyKind::Data(DataChange::Other)) => 24,
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)) => 25,
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => 26,
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime)) => 27,
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Permissions)) => 28,
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Ownership)) => 29,
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Extended)) => 30,
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Other)) => 31,
            EventKind::Modify(ModifyKind::Name(RenameMode::Any)) => 32,
            EventKind::Modify(ModifyKind::Name(RenameMode::To)) => 33,
            EventKind::Modify(ModifyKind::Name(RenameMode::From)) => 34,
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => 35,
            EventKind::Modify(ModifyKind::Name(RenameMode::Other)) => 36,
            EventKind::Remove(RemoveKind::Any) => 37,
            EventKind::Remove(RemoveKind::File) => 38,
            EventKind::Remove(RemoveKind::Folder) => 39,
            EventKind::Remove(RemoveKind::Other) => 40,
        }
    }

    /// Where the failure kind stands among the failures the filter test feeds, naming every kind notify has, so a kind it adds breaks the build.
    fn position_among_every_failure(failure: &ErrorKind) -> usize {
        match failure {
            ErrorKind::Generic(_) => 0,
            ErrorKind::Io(_) => 1,
            ErrorKind::PathNotFound => 2,
            ErrorKind::WatchNotFound => 3,
            ErrorKind::InvalidConfig(_) => 4,
            ErrorKind::MaxFilesWatch => 5,
        }
    }

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

        assert!(
            kinds
                .iter()
                .copied()
                .map(position_among_every_kind)
                .eq(0..kinds.len())
        );
        assert!(
            failures
                .iter()
                .map(position_among_every_failure)
                .eq(0..failures.len())
        );
        for kind in &kinds {
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

    #[test]
    fn observability_decision_13_a_watched_events_file_renamed_or_removed_is_forgotten_so_its_name_is_watched_again_and_no_other_notice_forgets_one()
     {
        let rolled_over =
            PathBuf::from("/traces/20261112T225913Z-6c935f82c118f7b3-estafette.events.jsonl");
        let other_run =
            PathBuf::from("/traces/20261112T225914Z-aaaaaaaaaaaaaaaa-estafette.events.jsonl");

        for kind in every_event_kind() {
            let mut watched_files = BTreeSet::from([rolled_over.clone(), other_run.clone()]);
            let notice = Event::new(kind).add_path(rolled_over.clone());

            forget_files_renamed_or_removed(&mut watched_files, &notice);

            let is_rename_or_removal = matches!(
                kind,
                EventKind::Modify(ModifyKind::Name(_)) | EventKind::Remove(_)
            );
            assert_eq!(
                watched_files.contains(&rolled_over),
                !is_rename_or_removal,
                "{kind:?}"
            );
            assert!(watched_files.contains(&other_run), "{kind:?}");
        }
    }
}
