#[cfg(test)]
#[path = "support/trace_files.rs"]
mod trace_files;

/// iroh's own lines in the raw trace file of spec/observability.md decision 7, through the shipped binary.
#[cfg(test)]
mod raw_iroh_lines {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use serde_json::Value;

    use super::trace_files::{announcement_of_one_run, trace_files_of_one_run};

    const SHORT_SCRATCH_PARENT: &str = "/tmp";
    const SCRATCH_PREFIX: &str = "eri";
    const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
    const NETWORK: &str = "lab";
    const UNKNOWN_HANDLE: &str = "bob";
    const BODY: &str = "hello";
    const CONFIG_HOME: &str = "config";
    const DATA_HOME: &str = "data";
    const RUNTIME_DIRECTORY: &str = "runtime";
    const BASE_TEST_SEND_FAILURE: &str =
        "the message was not delivered: No present member has that handle.\n";
    const SPEC_RAW_FILE_OPENED_EVENT: &str = "raw_file_opened";
    const SPEC_IROH_TARGETS: [&str; 4] = [
        "iroh::_events",
        "iroh::socket::remote_map",
        "iroh_gossip::proto::hyparview",
        "iroh_gossip::net",
    ];
    const IROH_EVENTS_TARGET: &str = "iroh::_events";

    /// A scratch root under `/tmp` with a network created in it.
    fn machine_with_network(test_tag: &str) -> PathBuf {
        let root = PathBuf::from(SHORT_SCRATCH_PARENT).join(format!(
            "{SCRATCH_PREFIX}-{}-{test_tag}",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        let created = binary(&root)
            .args(["network", "create", NETWORK])
            .output()
            .unwrap();
        assert!(created.status.success(), "{created:?}");
        root
    }

    /// The binary with the machine's environment alone.
    fn binary(root: &Path) -> Command {
        let mut command = Command::new(BINARY);
        command
            .env_clear()
            .env("XDG_CONFIG_HOME", root.join(CONFIG_HOME))
            .env("XDG_DATA_HOME", root.join(DATA_HOME))
            .env("XDG_RUNTIME_DIR", root.join(RUNTIME_DIRECTORY));
        command
    }

    /// Each line of a trace file as JSON.
    fn lines_of(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn observability_decision_7_a_process_that_starts_an_endpoint_writes_iroh_lines_of_the_four_targets_after_the_raw_file_first_line_and_nothing_more_to_standard_error()
     {
        let root = machine_with_network("installed");

        let sent = binary(&root)
            .args(["test-send", NETWORK, UNKNOWN_HANDLE, BODY])
            .output()
            .unwrap();

        assert_eq!(sent.status.code(), Some(1), "{sent:?}");
        assert_eq!(sent.stdout, Vec::<u8>::new());
        assert_eq!(
            String::from_utf8(sent.stderr).unwrap(),
            format!(
                "{}{BASE_TEST_SEND_FAILURE}",
                announcement_of_one_run(&root.join(DATA_HOME))
            )
        );
        let (_events, raw) = trace_files_of_one_run(&root.join(DATA_HOME));
        let lines = lines_of(&raw);
        let (first, iroh_lines) = lines.split_first().unwrap();
        assert_eq!(first["event"], SPEC_RAW_FILE_OPENED_EVENT);
        let targets: Vec<&str> = iroh_lines
            .iter()
            .map(|line| line["target"].as_str().unwrap_or_default())
            .collect();
        assert!(
            targets
                .iter()
                .any(|target| target.starts_with(IROH_EVENTS_TARGET)),
            "no line of {IROH_EVENTS_TARGET} among {targets:?}"
        );
        for target in &targets {
            assert!(
                SPEC_IROH_TARGETS
                    .iter()
                    .any(|spec_target| target.starts_with(spec_target)),
                "{target} is none of the four targets"
            );
        }
        drop(fs::remove_dir_all(&root));
    }
}
