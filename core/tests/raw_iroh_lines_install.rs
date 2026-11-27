#[cfg(test)]
#[path = "common/fresh_directory.rs"]
mod fresh_directory;
#[cfg(test)]
#[path = "common/remove_scratch.rs"]
mod remove_scratch;
#[path = "common/this_process_directory.rs"]
mod this_process_directory;

/// `spec/observability.md` decision 7 through the install the composition root calls, in a process of its own.
#[cfg(test)]
mod raw_iroh_lines_install {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use estafette::dirs::{DirectoryFilesystem, OperatingSystemFilesystem, TraceDirectory};
    use estafette::event::{EventName, TraceLineName};
    use estafette::trace::iroh_lines::install;
    use estafette::trace::{
        RUN_ID_BYTES, RunId, Trace, TraceBounds, TraceClocks, TraceEffects, TraceStop,
    };

    use super::remove_scratch::remove_scratch;
    use super::this_process_directory::fresh_directory_of_this_process_in;
    use serde_json::Value;

    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78];
    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const SPEC_REFUSAL_START: &str = "cannot write iroh's own lines into the raw trace file: ";
    const SPEC_REFUSAL_END: &str = "; run with --no-debug to write no trace";
    const MARKER_TARGET: &str = "iroh::_events::direct_addrs";
    const MARKER_ADDRS: &str = "a marker";

    /// A fresh scratch root for the test and the trace directory under its data home.
    fn scratch_trace_directory() -> (PathBuf, TraceDirectory) {
        let root = fresh_directory_of_this_process_in(
            &std::env::temp_dir(),
            "estafette-raw-iroh-lines-install",
        )
        .unwrap();
        let data_home = root.join("data");
        let directory = TraceDirectory::resolve(&move |name: &str| {
            (name == DATA_HOME_VARIABLE).then(|| OsString::from(&data_home))
        })
        .unwrap();
        (root, directory)
    }

    #[test]
    fn observability_decision_7_install_sends_iroh_lines_to_the_raw_file_once_per_process_and_refuses_a_second_install_naming_no_debug()
     {
        let (root, directory) = scratch_trace_directory();
        let filesystem = OperatingSystemFilesystem::of_this_process().unwrap();
        let trace = Arc::new(
            Trace::start(
                &directory,
                &|path| filesystem.create_private_directory(path),
                RunId::from_random_bytes(RUN_BYTES),
                &TraceEffects::of_the_operating_system(TraceClocks {
                    monotonic_ms: Arc::new(|| 0),
                    wall_ms: Arc::new(|| 0),
                }),
                TraceBounds::SPEC,
            )
            .unwrap(),
        );

        install(Arc::clone(&trace)).unwrap();
        tracing::debug!(target: MARKER_TARGET, addrs = MARKER_ADDRS);
        let refused = install(Arc::clone(&trace)).unwrap_err();

        let refusal = refused.to_string();
        assert!(refusal.starts_with(SPEC_REFUSAL_START), "{refusal}");
        assert!(refusal.ends_with(SPEC_REFUSAL_END), "{refusal}");
        assert!(matches!(
            trace.finish(),
            [TraceStop::Drained, TraceStop::Drained]
        ));
        let raw = fs::read_to_string(&trace.files().raw).unwrap();
        let lines: Vec<Value> = raw
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let [_first_line, iroh_line] = lines.as_slice() else {
            panic!("the raw file holds its first line and one iroh line: {raw}");
        };
        assert_eq!(iroh_line["event"], TraceLineName::IrohLine.as_str());
        assert_eq!(iroh_line["target"], MARKER_TARGET);
        assert_eq!(iroh_line["fields"]["addrs"], MARKER_ADDRS);
        remove_scratch(&root).unwrap();
    }
}
