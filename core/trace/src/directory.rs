use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::files::PRODUCT_NAME;

const HOME_VARIABLE: &str = "HOME";
const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
const DATA_HOME_UNDER_HOME: &str = ".local/share";
const TRACES_SUBDIRECTORY: &str = "traces";
const EVENTS_TRACE_SUFFIX: &str = ".events.jsonl";
const RAW_TRACE_SUFFIX: &str = ".raw.jsonl";
/// The suffix of the file a trace file rolls over into.
pub const ROLLED_OVER_TRACE_SUFFIX: &str = ".1";

/// Neither the variable a base directory is read from nor the home directory variable names a directory.
#[derive(Debug)]
pub struct BaseDirectoryUnresolvable {
    variable: &'static str,
}

impl fmt::Display for BaseDirectoryUnresolvable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} and {HOME_VARIABLE} are both unset or empty: set one of them",
            self.variable
        )
    }
}

impl std::error::Error for BaseDirectoryUnresolvable {}

/// The value of the environment variable as a path, an empty value counting as unset.
#[must_use]
pub fn non_empty_variable(
    environment: &impl Fn(&str) -> Option<OsString>,
    name: &str,
) -> Option<PathBuf> {
    environment(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The directory the variable names, else the directory under the home directory, creating nothing.
///
/// # Errors
///
/// `BaseDirectoryUnresolvable` when neither the variable nor the home directory variable is set and not empty.
pub fn xdg_base_directory(
    environment: &impl Fn(&str) -> Option<OsString>,
    variable: &'static str,
    default_under_home: &str,
) -> Result<PathBuf, BaseDirectoryUnresolvable> {
    non_empty_variable(environment, variable)
        .or_else(|| {
            non_empty_variable(environment, HOME_VARIABLE).map(|home| home.join(default_under_home))
        })
        .ok_or(BaseDirectoryUnresolvable { variable })
}

/// The data base directory the environment names, creating nothing.
///
/// # Errors
///
/// `BaseDirectoryUnresolvable` when neither the data home variable nor the home directory variable is set and not empty.
pub fn data_base_directory(
    environment: &impl Fn(&str) -> Option<OsString>,
) -> Result<PathBuf, BaseDirectoryUnresolvable> {
    xdg_base_directory(environment, DATA_HOME_VARIABLE, DATA_HOME_UNDER_HOME)
        .map(|data_home| data_home.join(PRODUCT_NAME))
}

/// The two trace files of one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFiles {
    /// The typed events of the run, safe to post.
    pub events: PathBuf,
    /// The transport library's own lines of the run, not safe to post.
    pub raw: PathBuf,
}

/// The trace directory under the data base directory, resolved without creating anything, and the files of each run in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceDirectory {
    data: PathBuf,
}

impl TraceDirectory {
    /// The trace directory under the data base directory the environment names, creating nothing.
    ///
    /// # Errors
    ///
    /// Fails when the data base directory cannot be resolved.
    pub fn resolve(
        environment: &impl Fn(&str) -> Option<OsString>,
    ) -> Result<Self, BaseDirectoryUnresolvable> {
        Ok(Self {
            data: data_base_directory(environment)?,
        })
    }

    /// The directories the trace needs, outermost first: the data base directory, then the trace directory.
    #[must_use]
    pub fn private_directories(&self) -> [PathBuf; 2] {
        [self.data.clone(), self.path()]
    }

    /// The directory holding the trace files of every run.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.data.join(TRACES_SUBDIRECTORY)
    }

    /// The events file and the raw file of the run named `run_name`.
    #[must_use]
    pub fn files_of(&self, run_name: &str) -> TraceFiles {
        let traces = self.path();
        TraceFiles {
            events: traces.join(events_trace_file_name(run_name)),
            raw: traces.join(raw_trace_file_name(run_name)),
        }
    }

    /// Where the trace directory sits, as the help states it: under the data directory the environment names, or its default under the home directory.
    #[must_use]
    pub fn described() -> String {
        format!(
            "{TRACES_SUBDIRECTORY}/ under the data directory (${DATA_HOME_VARIABLE}/{PRODUCT_NAME}, or ~/{DATA_HOME_UNDER_HOME}/{PRODUCT_NAME})"
        )
    }
}

/// The name of the events file of the run named `run_name`.
#[must_use]
pub fn events_trace_file_name(run_name: &str) -> String {
    format!("{run_name}{EVENTS_TRACE_SUFFIX}")
}

/// The name of the raw file of the run named `run_name`.
#[must_use]
pub fn raw_trace_file_name(run_name: &str) -> String {
    format!("{run_name}{RAW_TRACE_SUFFIX}")
}

/// Which of its run's two trace files a file is, counting the `.1` file it rolled over into as the same file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceFileKind {
    Events,
    Raw,
}

/// The file a trace file rolls over into, replacing any earlier one.
#[must_use]
pub fn rolled_over_trace_file(path: &Path) -> PathBuf {
    let mut rolled_over = path.as_os_str().to_owned();
    rolled_over.push(ROLLED_OVER_TRACE_SUFFIX);
    PathBuf::from(rolled_over)
}

/// The run a file name of the trace directory belongs to and which of its files it is; none for a name no trace file has.
#[must_use]
pub fn trace_file_of(file_name: &str) -> Option<(&str, TraceFileKind)> {
    let unrolled = file_name
        .strip_suffix(ROLLED_OVER_TRACE_SUFFIX)
        .unwrap_or(file_name);
    let (run_name, kind) = unrolled
        .strip_suffix(EVENTS_TRACE_SUFFIX)
        .map(|run_name| (run_name, TraceFileKind::Events))
        .or_else(|| {
            unrolled
                .strip_suffix(RAW_TRACE_SUFFIX)
                .map(|run_name| (run_name, TraceFileKind::Raw))
        })?;
    (!run_name.is_empty()).then_some((run_name, kind))
}
