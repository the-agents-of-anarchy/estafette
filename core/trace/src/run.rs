use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::directory::{TraceDirectory, TraceFileKind, TraceFiles};
use crate::files::{Program, remove_oldest_runs, run_name};
use crate::line::HeaderSource;
use crate::writer::{FileTrace, RollingFile, RolloverOpening};
use crate::{
    CreateTraceFile, RenameFile, RunId, SpawnThread, TraceAction, TraceBounds, TraceEffects,
    TraceRefused,
};

/// One process start of one program: its trace directories created, its oldest runs removed past the bounds, its files named, and the header every line of every file of the run begins with.
pub struct TraceRun {
    files: TraceFiles,
    header: Arc<HeaderSource>,
    create_file: CreateTraceFile,
    rename_file: RenameFile,
    spawn_thread: SpawnThread,
    rollover_bytes: u64,
    queue_lines: usize,
}

impl TraceRun {
    /// Creates the trace directories through `create_private_directory`, removes the oldest runs past the bounds, and names this run's files.
    ///
    /// # Errors
    ///
    /// `TraceRefused` naming the path that could not be created, listed or removed.
    pub fn begin(
        directory: &TraceDirectory,
        create_private_directory: &dyn Fn(&Path) -> io::Result<()>,
        program: Program,
        run: RunId,
        effects: &TraceEffects,
        bounds: TraceBounds,
    ) -> Result<Self, TraceRefused> {
        for path in directory.private_directories() {
            create_private_directory(&path)
                .map_err(|source| TraceRefused::new(TraceAction::CreateDirectory, &path, source))?;
        }
        let traces = directory.path();
        let header = Arc::new(HeaderSource::new(run, effects.clocks.clone()));
        let started_ms = header.wall_ms();
        remove_oldest_runs(
            &traces,
            started_ms,
            &bounds,
            &effects.list_files,
            &effects.remove_file,
        )?;
        Ok(Self {
            files: directory.files_of(&run_name(program, started_ms, &run.hex())),
            header,
            create_file: Arc::clone(&effects.create_file),
            rename_file: Arc::clone(&effects.rename_file),
            spawn_thread: Arc::clone(&effects.spawn_thread),
            rollover_bytes: bounds.rollover_bytes,
            queue_lines: bounds.queue_lines,
        })
    }

    /// The two files this run names, of which it creates the ones it opens.
    #[must_use]
    pub const fn files(&self) -> &TraceFiles {
        &self.files
    }

    /// Names the node, by the short form of its id, in every later line of every file of the run.
    pub fn note_node(&self, short_id: String) {
        self.header.note_node(short_id);
    }

    /// Creates the file of the kind and starts its writer, each rollover starting a file that holds `opening`.
    ///
    /// # Errors
    ///
    /// `TraceRefused` naming the file that could not be created or whose writer could not start.
    pub fn open(
        &self,
        kind: TraceFileKind,
        opening: RolloverOpening,
    ) -> Result<FileTrace, TraceRefused> {
        let path = match kind {
            TraceFileKind::Events => &self.files.events,
            TraceFileKind::Raw => &self.files.raw,
        };
        let file = (self.create_file)(path)
            .map_err(|source| TraceRefused::new(TraceAction::CreateFile, path, source))?;
        FileTrace::start(
            RollingFile::new(
                path.clone(),
                file,
                self.rollover_bytes,
                opening,
                Arc::clone(&self.create_file),
                Arc::clone(&self.rename_file),
            ),
            Arc::clone(&self.header),
            self.queue_lines,
            &self.spawn_thread,
        )
        .map_err(|source| TraceRefused::new(TraceAction::StartWriter, path, source))
    }
}
