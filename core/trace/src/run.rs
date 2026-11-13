use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::directory::{TraceDirectory, TraceFileKind, TraceFiles};
use crate::files::{Program, remove_oldest_runs, run_name};
use crate::line::HeaderSource;
use crate::writer::{FileTrace, RollingFile};
use crate::{
    CreateTraceFile, RenameFile, RunId, SpawnThread, TraceAction, TraceBounds, TraceEffects,
    TraceRefused,
};

/// What a run of either program holds: its files named, the header every line of every file begins with, and what each file's writer is started with.
struct TraceRun {
    files: TraceFiles,
    header: Arc<HeaderSource>,
    create_file: CreateTraceFile,
    rename_file: RenameFile,
    spawn_thread: SpawnThread,
    rollover_bytes: u64,
    queue_lines: usize,
    drain: Duration,
}

impl TraceRun {
    /// Creates the trace directories through `create_private_directory`, removes the oldest runs past the bounds, and names this run's files for `program`.
    fn begin(
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
            drain: bounds.drain,
        })
    }

    /// Creates the file of the kind and starts its writer.
    fn open(&self, kind: TraceFileKind) -> Result<FileTrace, TraceRefused> {
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
                kind,
                Arc::clone(&self.create_file),
                Arc::clone(&self.rename_file),
            ),
            self.drain,
            Arc::clone(&self.header),
            self.queue_lines,
            &self.spawn_thread,
        )
        .map_err(|source| TraceRefused::new(TraceAction::StartWriter, path, source))
    }
}

/// One start of the endpoint program: its events file and its raw file named, and the header every line of both begins with.
pub struct EstafetteRun {
    run: TraceRun,
}

impl EstafetteRun {
    /// Creates the trace directories through `create_private_directory`, removes the oldest runs past the bounds, and names this run's two files.
    ///
    /// # Errors
    ///
    /// `TraceRefused` naming the path that could not be created, listed or removed.
    pub fn begin(
        directory: &TraceDirectory,
        create_private_directory: &dyn Fn(&Path) -> io::Result<()>,
        run: RunId,
        effects: &TraceEffects,
        bounds: TraceBounds,
    ) -> Result<Self, TraceRefused> {
        TraceRun::begin(
            directory,
            create_private_directory,
            Program::Estafette,
            run,
            effects,
            bounds,
        )
        .map(|trace_run| Self { run: trace_run })
    }

    /// The two files this run names, of which it creates the ones it opens.
    #[must_use]
    pub const fn files(&self) -> &TraceFiles {
        &self.run.files
    }

    /// Names the node, by the short form of its id, in every later line of both files.
    pub fn note_node(&self, short_id: String) {
        self.run.header.note_node(short_id);
    }

    /// Creates the events file and starts its writer.
    ///
    /// # Errors
    ///
    /// `TraceRefused` naming the file that could not be created or whose writer could not start.
    pub fn open_events(&self) -> Result<FileTrace, TraceRefused> {
        self.run.open(TraceFileKind::Events)
    }

    /// Creates the raw file and starts its writer, which repeats the file's first line at the head of each file a rollover starts.
    ///
    /// # Errors
    ///
    /// `TraceRefused` naming the file that could not be created or whose writer could not start.
    pub fn open_raw(&self) -> Result<FileTrace, TraceRefused> {
        self.run.open(TraceFileKind::Raw)
    }
}

/// One start of a bridge: its events file named, the only trace file a bridge writes, and the header every line of it begins with.
pub struct BridgeRun {
    run: TraceRun,
}

impl BridgeRun {
    /// Creates the trace directories through `create_private_directory`, removes the oldest runs past the bounds, and names this run's events file.
    ///
    /// # Errors
    ///
    /// `TraceRefused` naming the path that could not be created, listed or removed.
    pub fn begin(
        directory: &TraceDirectory,
        create_private_directory: &dyn Fn(&Path) -> io::Result<()>,
        run: RunId,
        effects: &TraceEffects,
        bounds: TraceBounds,
    ) -> Result<Self, TraceRefused> {
        TraceRun::begin(
            directory,
            create_private_directory,
            Program::Bridge,
            run,
            effects,
            bounds,
        )
        .map(|trace_run| Self { run: trace_run })
    }

    /// The events file this run names, which it creates when it opens it.
    #[must_use]
    pub fn events_file(&self) -> &Path {
        &self.run.files.events
    }

    /// Creates the events file and starts its writer.
    ///
    /// # Errors
    ///
    /// `TraceRefused` naming the file that could not be created or whose writer could not start.
    pub fn open_events(&self) -> Result<FileTrace, TraceRefused> {
        self.run.open(TraceFileKind::Events)
    }
}
