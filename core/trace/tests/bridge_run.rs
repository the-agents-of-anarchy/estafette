#[cfg(test)]
#[path = "../../tests/common/fresh_directory.rs"]
mod fresh_directory;
#[cfg(test)]
#[path = "../../tests/common/remove_scratch.rs"]
mod remove_scratch;

#[cfg(test)]
#[path = "support/frozen_run.rs"]
mod frozen_run;

/// The bridge's run of spec/observability.md decisions 4, 5 and 13, through the package's public surface.
#[cfg(test)]
mod observability {
    use std::ffi::OsString;
    use std::fs::{self, File, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, UNIX_EPOCH};

    use super::fresh_directory::fresh_directory_in;
    use super::frozen_run::{
        DATA_HOME_VARIABLE, FROZEN_MONOTONIC_MS, NOW_MS, RUN_BYTES, frozen_clocks_at,
    };
    use super::remove_scratch::remove_scratch;
    use estafette_trace::{
        BridgeRun, RunId, TraceAction, TraceBounds, TraceDirectory, TraceEffects, TraceRefused,
        TraceStop,
    };

    const READ_WITHOUT_SEARCH_MODE: u32 = 0o400;
    const OWNER_ONLY_DIRECTORY_MODE: u32 = 0o700;
    const LISTING_RACE_STARTS: u32 = 20_000;
    const CHURNING_ENTRY_NAMES: u64 = 64;
    const CHURNING_ENTRY_CONTENT: &[u8] = b"x";
    const RUN_HEX: &str = "a1b2c3d4e5f60718";
    const START_WALL_MS: u64 = 951_868_799_123;
    const START_STAMP: &str = "20000229T235959Z";
    const SMALL_FRESHNESS_MS: u64 = 10_000;
    const SMALL_DIRECTORY_BYTES: u64 = 150;
    const OLD_FILE_BYTES: u64 = 100;
    const ONE_EVENT: &str = "process.clock_before_unix_epoch";

    /// A fresh scratch root for the test and the trace directory under its data home.
    fn scratch_trace_directory(test_name: &str) -> (PathBuf, TraceDirectory) {
        let root = fresh_directory_in(
            &std::env::temp_dir(),
            &format!("estafette-trace-package-{test_name}-{}", std::process::id()),
        )
        .unwrap();
        let data_home = root.join("data");
        let directory = TraceDirectory::resolve(&move |name: &str| {
            (name == DATA_HOME_VARIABLE).then(|| OsString::from(&data_home))
        })
        .unwrap();
        (root, directory)
    }

    /// A bridge run begun on the effects within the bounds, its trace directory created on the operating system's files, or the refusal to begin it.
    fn try_begin_bridge_run(
        directory: &TraceDirectory,
        effects: &TraceEffects,
        bounds: TraceBounds,
    ) -> Result<BridgeRun, TraceRefused> {
        BridgeRun::begin(
            directory,
            &|path: &Path| fs::create_dir_all(path),
            RunId::from_random_bytes(RUN_BYTES),
            effects,
            bounds,
        )
    }

    /// A bridge run begun on the operating system's files with its wall clock frozen at `wall_ms`.
    fn begun_bridge_run(
        directory: &TraceDirectory,
        wall_ms: u64,
        bounds: TraceBounds,
    ) -> BridgeRun {
        try_begin_bridge_run(
            directory,
            &TraceEffects::of_the_operating_system(frozen_clocks_at(wall_ms)),
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
        remove_scratch(&root).unwrap();
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
        remove_scratch(&root).unwrap();
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
        remove_scratch(&root).unwrap();
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
        let effects = TraceEffects::of_the_operating_system(frozen_clocks_at(NOW_MS));
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

        let run = try_begin_bridge_run(
            &directory,
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
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_13_a_start_listing_a_directory_whose_entries_come_and_go_while_it_lists_goes_on()
     {
        let (root, directory) = scratch_trace_directory("churning-listing");
        let traces = directory.path();
        fs::create_dir_all(&traces).unwrap();
        let churn_stop = Arc::new(AtomicBool::new(false));
        let entry_churn = thread::spawn({
            let churn_stop = Arc::clone(&churn_stop);
            move || {
                let mut round: u64 = 0;
                while !churn_stop.load(Ordering::Relaxed) {
                    let entry = traces.join(format!("churning-{}", round % CHURNING_ENTRY_NAMES));
                    fs::write(&entry, CHURNING_ENTRY_CONTENT).unwrap();
                    remove_scratch(&entry).unwrap();
                    round += 1;
                }
            }
        });
        let effects = TraceEffects::of_the_operating_system(frozen_clocks_at(NOW_MS));

        let refusal = (0..LISTING_RACE_STARTS)
            .find_map(|_| try_begin_bridge_run(&directory, &effects, TraceBounds::SPEC).err());
        churn_stop.store(true, Ordering::Relaxed);
        entry_churn.join().unwrap();

        assert!(
            refusal.is_none(),
            "a listing that met a removed entry refused the start: {refusal:?}"
        );
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_13_a_start_whose_listing_cannot_read_an_entry_for_another_reason_refuses_naming_the_directory()
     {
        let (root, directory) = scratch_trace_directory("unreadable-entry");
        let traces = directory.path();
        fs::create_dir_all(&traces).unwrap();
        stored_run_file(&traces, "unreadable-entry", OLD_FILE_BYTES, NOW_MS);
        fs::set_permissions(&traces, Permissions::from_mode(READ_WITHOUT_SEARCH_MODE)).unwrap();

        let refusal = try_begin_bridge_run(
            &directory,
            &TraceEffects::of_the_operating_system(frozen_clocks_at(NOW_MS)),
            TraceBounds::SPEC,
        )
        .err();
        fs::set_permissions(&traces, Permissions::from_mode(OWNER_ONLY_DIRECTORY_MODE)).unwrap();

        let refusal = refusal.expect("an entry that cannot be read refuses the start");
        assert_eq!(refusal.action(), TraceAction::ListDirectory);
        assert_eq!(refusal.path(), traces);
        remove_scratch(&root).unwrap();
    }
}
