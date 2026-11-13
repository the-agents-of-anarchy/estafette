/// `spec/observability.md` decision 7 through the install the composition root calls, in a process of its own.
#[cfg(test)]
mod raw_iroh_lines_install {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use estafette::dirs::{OperatingSystemFilesystem, TraceDirectory};
    use estafette::trace::iroh_lines::install;
    use estafette::trace::{
        RUN_ID_BYTES, RunId, Trace, TraceBounds, TraceClocks, TraceEffects, TraceStop,
    };
    use serde_json::Value;

    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0x0f, 0x1e, 0x2d, 0x3c, 0x4b, 0x5a, 0x69, 0x78];
    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const SPEC_IROH_LINE: &str = "iroh_line";
    const SPEC_REFUSAL: &str = "cannot write iroh's own lines into the raw trace file: a global default trace dispatcher has already been set; run with --no-debug to write no trace";

    /// A fresh scratch root for the test and the trace directory under its data home.
    fn scratch_trace_directory() -> (PathBuf, TraceDirectory) {
        let root = std::env::temp_dir().join(format!(
            "estafette-raw-iroh-lines-install-{}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
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
        let trace = Arc::new(
            Trace::start(
                &directory,
                &OperatingSystemFilesystem,
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
        tracing::debug!(target: "iroh::_events::direct_addrs", addrs = "a marker");
        let refused = install(Arc::clone(&trace)).unwrap_err();

        assert_eq!(refused.to_string(), SPEC_REFUSAL);
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
        assert_eq!(iroh_line["event"], SPEC_IROH_LINE);
        assert_eq!(iroh_line["target"], "iroh::_events::direct_addrs");
        assert_eq!(iroh_line["fields"]["addrs"], "a marker");
        drop(fs::remove_dir_all(&root));
    }
}
