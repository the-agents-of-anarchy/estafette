use std::fs;
use std::path::{Path, PathBuf};

/// Where the trace files of every run lie under a data home (spec/observability.md decision 4).
const TRACES_UNDER_DATA_HOME: &str = "estafette/traces";
/// The end of an events file's name.
pub const EVENTS_SUFFIX: &str = ".events.jsonl";
/// The end of a raw file's name.
pub const RAW_SUFFIX: &str = ".raw.jsonl";

/// The trace directory under the data home.
pub fn traces_directory(data_home: &Path) -> PathBuf {
    data_home.join(TRACES_UNDER_DATA_HOME)
}

/// The events file and the raw file of the one run in the data home's trace directory.
pub fn trace_files_of_one_run(data_home: &Path) -> (PathBuf, PathBuf) {
    let traces = traces_directory(data_home);
    let mut names: Vec<String> = fs::read_dir(&traces)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    let [events, raw] = names.as_slice() else {
        panic!("one run's two files under {}: {names:?}", traces.display());
    };
    assert!(events.ends_with(EVENTS_SUFFIX), "{names:?}");
    assert!(raw.ends_with(RAW_SUFFIX), "{names:?}");
    assert_eq!(
        events.strip_suffix(EVENTS_SUFFIX),
        raw.strip_suffix(RAW_SUFFIX),
        "{names:?}"
    );
    (traces.join(events), traces.join(raw))
}

/// The first line the binary writes to standard error under `--debug` (spec/observability.md decision 3).
pub fn announcement_of(events: &Path, raw: &Path) -> String {
    format!(
        "writing the debug trace: {} (safe to post) and {} (not safe to post)\n",
        events.display(),
        raw.display()
    )
}

/// The first line of the one run in the data home.
pub fn announcement_of_one_run(data_home: &Path) -> String {
    let (events, raw) = trace_files_of_one_run(data_home);
    announcement_of(&events, &raw)
}
