use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// What the operating system reports about a change under a watched path.
type Notice = notify::Result<notify::Event>;

/// The operating system's notices about the paths a wait watches, with the files already watched.
pub struct GrowthWatch {
    watcher: RecommendedWatcher,
    notices: Receiver<Notice>,
    watched_files: BTreeSet<PathBuf>,
}

/// Why a wait on growing files ended with nothing found.
pub enum WaitEnded {
    RanOut,
    WatcherFailed(notify::Error),
    WatcherEnded,
}

impl GrowthWatch {
    /// Starts watching the path, a directory for the files that appear in it or a file for its growth.
    ///
    /// # Errors
    ///
    /// Fails with the watcher's error, naming the path, when the watcher cannot start or cannot watch it.
    pub fn of_path(path: &Path) -> Result<Self, String> {
        let (sender, notices) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |notice: Notice| {
            if wakes_the_wait(&notice) {
                drop(sender.send(notice));
            }
        })
        .map_err(|failure| format!("cannot start the file watcher: {failure}"))?;
        let mut watch = Self {
            watcher,
            notices,
            watched_files: BTreeSet::new(),
        };
        watch.watch_file(path)?;
        Ok(watch)
    }

    /// Starts watching the path, a directory for the files that appear in it or a file for its growth, unless it is watched already.
    pub fn watch_file(&mut self, file: &Path) -> Result<(), String> {
        if self.watched_files.insert(file.to_path_buf()) {
            self.watcher
                .watch(file, RecursiveMode::NonRecursive)
                .map_err(|failure| format!("cannot watch {}: {failure}", file.display()))?;
        }
        Ok(())
    }
}

/// The first value `look` finds, looking at once and again on each notice that a watched path may have grown, with `once_waiting` run once the first look found nothing; why it ended otherwise, the bound read from `clock` running out among the reasons.
pub fn first_found_as_files_grow<Found>(
    watch: &mut GrowthWatch,
    within: Duration,
    mut clock: impl FnMut() -> Instant,
    once_waiting: impl FnOnce(),
    mut look: impl FnMut(&mut GrowthWatch) -> Option<Found>,
) -> Result<Found, WaitEnded> {
    let deadline = clock().checked_add(within).unwrap();
    if let Some(found) = look(watch) {
        return Ok(found);
    }
    once_waiting();
    loop {
        let remaining = deadline.saturating_duration_since(clock());
        match watch.notices.recv_timeout(remaining) {
            Ok(Ok(notice)) => {
                if clock() >= deadline {
                    return Err(WaitEnded::RanOut);
                }
                forget_files_renamed_or_removed(&mut watch.watched_files, &notice);
            }
            Ok(Err(failure)) => return Err(WaitEnded::WatcherFailed(failure)),
            Err(RecvTimeoutError::Timeout) => return Err(WaitEnded::RanOut),
            Err(RecvTimeoutError::Disconnected) => return Err(WaitEnded::WatcherEnded),
        }
        if let Some(found) = look(watch) {
            return if clock() < deadline {
                Ok(found)
            } else {
                Err(WaitEnded::RanOut)
            };
        }
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

    /// The kind the exhaustive match places last, whose position plus one is how many kinds the list must hold.
    const LAST_KIND_OF_THE_MATCH: EventKind = EventKind::Remove(RemoveKind::Other);

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
        assert_eq!(
            kinds.len(),
            position_among_every_kind(LAST_KIND_OF_THE_MATCH)
                .checked_add(1)
                .unwrap()
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
