/// The bridge's run of spec/observability.md decisions 4, 5 and 13, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::ffi::OsString;
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, UNIX_EPOCH};

    use estafette_trace::{
        BridgeRun, RUN_ID_BYTES, RunId, TraceBounds, TraceClocks, TraceDirectory, TraceEffects,
        TraceStop,
    };

    const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
    const RUN_BYTES: [u8; RUN_ID_BYTES] = [0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x07, 0x18];
    const RUN_HEX: &str = "a1b2c3d4e5f60718";
    const FROZEN_MONOTONIC_MS: u64 = 7;
    const START_WALL_MS: u64 = 951_868_799_123;
    const START_STAMP: &str = "20000229T235959Z";
    const NOW_MS: u64 = 2_000_000_000_000;
    const SMALL_FRESHNESS_MS: u64 = 10_000;
    const SMALL_DIRECTORY_BYTES: u64 = 150;
    const OLD_FILE_BYTES: u64 = 100;
    const ONE_EVENT: &str = "process.clock_before_unix_epoch";

    /// A fresh scratch root for the test and the trace directory under its data home.
    fn scratch_trace_directory(test_name: &str) -> (PathBuf, TraceDirectory) {
        let root = std::env::temp_dir().join(format!(
            "estafette-trace-package-{test_name}-{}",
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

    /// Clocks that always read `FROZEN_MONOTONIC_MS` and `wall_ms`.
    fn frozen_clocks(wall_ms: u64) -> TraceClocks {
        TraceClocks {
            monotonic_ms: Arc::new(|| FROZEN_MONOTONIC_MS),
            wall_ms: Arc::new(move || wall_ms),
        }
    }

    /// A bridge run begun on the operating system's files with its wall clock frozen at `wall_ms`.
    fn begun_bridge_run(
        directory: &TraceDirectory,
        wall_ms: u64,
        bounds: TraceBounds,
    ) -> BridgeRun {
        BridgeRun::begin(
            directory,
            &|path: &Path| fs::create_dir_all(path),
            RunId::from_random_bytes(RUN_BYTES),
            &TraceEffects::of_the_operating_system(frozen_clocks(wall_ms)),
            bounds,
        )
        .unwrap()
    }

    /// The names of the files in the directory, sorted.
    fn names_in(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// Creates a file of `bytes` bytes in the trace directory, last written at `written_ms`.
    fn stored_run_file(traces: &Path, name: &str, bytes: u64, written_ms: u64) {
        let file = File::create(traces.join(name)).unwrap();
        file.set_len(bytes).unwrap();
        file.set_modified(
            UNIX_EPOCH
                .checked_add(Duration::from_millis(written_ms))
                .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn observability_decision_4_a_bridge_run_opening_the_events_file_alone_names_it_for_the_bridge_and_creates_no_raw_file()
     {
        let (root, directory) = scratch_trace_directory("bridge-events-alone");
        let run = begun_bridge_run(&directory, START_WALL_MS, TraceBounds::SPEC);

        let events = run.open_events().unwrap();
        assert!(matches!(events.finish(), TraceStop::Drained));

        assert_eq!(
            names_in(&directory.path()),
            [format!("{START_STAMP}-{RUN_HEX}-bridge.events.jsonl")]
        );
        assert_eq!(
            run.events_file(),
            directory
                .files_of(&format!("{START_STAMP}-{RUN_HEX}-bridge"))
                .events
        );
        drop(fs::remove_dir_all(&root));
    }

    /// The line a bridge run writes for `ONE_EVENT` as its line `n`: the header in its order and no node.
    fn one_event_line(n: u64) -> String {
        format!(
            "{{\"v\":1,\"run\":\"{RUN_HEX}\",\"n\":{n},\"mono_ms\":{FROZEN_MONOTONIC_MS},\"wall_ms\":{START_WALL_MS},\"event\":\"{ONE_EVENT}\"}}\n"
        )
    }

    #[test]
    fn observability_decision_5_the_lines_of_a_bridge_run_begin_with_the_header_in_its_order_and_carry_no_node()
     {
        let (root, directory) = scratch_trace_directory("bridge-no-node");
        let run = begun_bridge_run(&directory, START_WALL_MS, TraceBounds::SPEC);
        let events = run.open_events().unwrap();

        events.write(ONE_EVENT, &[]);
        events.write(ONE_EVENT, &[]);
        assert!(matches!(events.finish(), TraceStop::Drained));

        assert_eq!(
            fs::read_to_string(run.events_file()).unwrap(),
            format!("{}{}", one_event_line(0), one_event_line(1))
        );
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_13_one_start_removes_an_old_bridge_run_and_an_old_estafette_run_both_files_of_each_together_sparing_a_fresh_one_of_each()
     {
        let (root, directory) = scratch_trace_directory("one-rule");
        let traces = directory.path();
        fs::create_dir_all(&traces).unwrap();
        let stale = NOW_MS - SMALL_FRESHNESS_MS - 1;
        let old_bridge = "20000101T000000Z-0000000000000001-bridge";
        let old_estafette = "20000102T000000Z-0000000000000002-estafette";
        let fresh_bridge = "20010101T000000Z-0000000000000003-bridge";
        let fresh_estafette = "20010102T000000Z-0000000000000004-estafette";
        stored_run_file(
            &traces,
            &format!("{old_bridge}.events.jsonl"),
            OLD_FILE_BYTES,
            stale,
        );
        stored_run_file(
            &traces,
            &format!("{old_bridge}.events.jsonl.1"),
            OLD_FILE_BYTES,
            stale,
        );
        stored_run_file(
            &traces,
            &format!("{old_estafette}.events.jsonl"),
            OLD_FILE_BYTES,
            stale,
        );
        stored_run_file(
            &traces,
            &format!("{old_estafette}.raw.jsonl"),
            OLD_FILE_BYTES,
            stale,
        );
        stored_run_file(
            &traces,
            &format!("{fresh_bridge}.events.jsonl"),
            OLD_FILE_BYTES,
            NOW_MS,
        );
        stored_run_file(
            &traces,
            &format!("{fresh_estafette}.events.jsonl"),
            OLD_FILE_BYTES,
            NOW_MS,
        );
        stored_run_file(
            &traces,
            &format!("{fresh_estafette}.raw.jsonl"),
            OLD_FILE_BYTES,
            NOW_MS,
        );

        let run = begun_bridge_run(
            &directory,
            NOW_MS,
            TraceBounds {
                directory_bytes: SMALL_DIRECTORY_BYTES,
                freshness: Duration::from_millis(SMALL_FRESHNESS_MS),
                ..TraceBounds::SPEC
            },
        );
        let events = run.open_events().unwrap();
        assert!(matches!(events.finish(), TraceStop::Drained));

        let kept: Vec<String> = names_in(&traces)
            .into_iter()
            .filter(|name| !name.contains(RUN_HEX))
            .collect();
        assert_eq!(
            kept,
            [
                format!("{fresh_bridge}.events.jsonl"),
                format!("{fresh_estafette}.events.jsonl"),
                format!("{fresh_estafette}.raw.jsonl"),
            ]
        );
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_13_a_start_whose_old_run_a_concurrent_start_removed_after_the_listing_goes_on()
     {
        let (root, directory) = scratch_trace_directory("concurrent-removal");
        let traces = directory.path();
        fs::create_dir_all(&traces).unwrap();
        let stale = NOW_MS - SMALL_FRESHNESS_MS - 1;
        let old_run = "20000101T000000Z-0000000000000001-estafette";
        let old_files = [
            traces.join(format!("{old_run}.events.jsonl")),
            traces.join(format!("{old_run}.raw.jsonl")),
        ];
        for file in &old_files {
            let name = file.file_name().unwrap().to_str().unwrap();
            stored_run_file(&traces, name, OLD_FILE_BYTES, stale);
        }
        let effects = TraceEffects::of_the_operating_system(frozen_clocks(NOW_MS));
        let listed_by_the_system = Arc::clone(&effects.list_files);
        let effects_with_a_concurrent_start_removing_after_the_listing = TraceEffects {
            list_files: Arc::new(move |listed_directory: &Path| {
                let listed = listed_by_the_system(listed_directory);
                for file in &old_files {
                    fs::remove_file(file).unwrap();
                }
                listed
            }),
            ..effects
        };

        let run = BridgeRun::begin(
            &directory,
            &|path: &Path| fs::create_dir_all(path),
            RunId::from_random_bytes(RUN_BYTES),
            &effects_with_a_concurrent_start_removing_after_the_listing,
            TraceBounds {
                directory_bytes: SMALL_DIRECTORY_BYTES,
                freshness: Duration::from_millis(SMALL_FRESHNESS_MS),
                ..TraceBounds::SPEC
            },
        )
        .expect("a start whose old run is already removed goes on");

        let events = run.open_events().unwrap();
        assert!(matches!(events.finish(), TraceStop::Drained));
        drop(fs::remove_dir_all(&root));
    }
}
