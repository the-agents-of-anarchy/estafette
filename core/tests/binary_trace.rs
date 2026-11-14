#[cfg(test)]
#[path = "common/fresh_directory.rs"]
mod fresh_directory;
#[cfg(test)]
#[path = "common/remove_scratch.rs"]
mod remove_scratch;
#[cfg(test)]
#[path = "support/trace_files.rs"]
mod trace_files;
#[cfg(test)]
#[path = "support/trace_reader.rs"]
mod trace_reader;

#[cfg(test)]
#[path = "support/whole_trace_lines.rs"]
mod whole_trace_lines;

/// The trace seam of spec/observability.md, decisions 3, 4, 5 and 12, through the shipped binary.
#[cfg(test)]
mod observability {
    use std::fmt::Write as _;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};

    use super::fresh_directory::fresh_directory_in;
    use super::remove_scratch::remove_scratch;
    use super::trace_files::{
        EVENTS_SUFFIX, RAW_SUFFIX, announcement_of_one_run, trace_files_of_one_run,
        traces_directory,
    };
    use super::trace_reader::{TraceLine, stopped_trace_lines_of};
    use super::whole_trace_lines::{
        WHOLE_LINE_WAIT, holds_within_the_wait, whole_lines_of_a_run_file,
    };
    use serde_json::json;

    const SHORT_SCRATCH_PARENT: &str = "/tmp";
    const SCRATCH_PREFIX: &str = "et";
    const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
    const NETWORK: &str = "lab";
    const UNKNOWN_HANDLE: &str = "bob";
    const BODY: &str = "hello";
    const NO_DEBUG: &str = "--no-debug";
    const LOOKALIKE_SWITCHES_OFF: [(&str, &str); 6] = [
        ("ESTAFETTE_DEBUG", "0"),
        ("ESTAFETTE_NO_DEBUG", "1"),
        ("ESTAFETTE_TRACE", "off"),
        ("RUST_LOG", "off"),
        ("DEBUG", "0"),
        ("NO_DEBUG", "1"),
    ];
    const LOOKALIKE_SWITCHES_ON: [(&str, &str); 4] = [
        ("ESTAFETTE_DEBUG", "1"),
        ("ESTAFETTE_TRACE", "on"),
        ("RUST_LOG", "trace"),
        ("DEBUG", "1"),
    ];
    const DEBUG_CONFIGURATION_FIELD: &str = "debug = false\n";
    const NETWORKS_UNDER_CONFIG_HOME: &str = "estafette/networks";
    const TOML_EXTENSION: &str = "toml";
    const CARD: &str = "alice";
    const KILLED_AFTER_ANSWERS: usize = 50;
    const FIRST_TOOL_CALL_ID: usize = 2;
    const UNREADABLE_CARDS_DIRECTORY: &str = "cards-is-a-file";
    const COMMAND_FAILED_EVENT: &str = "process.command_failed";
    const HANDSHAKE_FAILED_EVENT: &str = "tool.handshake_failed";
    const EXITED_EVENT: &str = "process.exited";
    /// The last events of a run whose command failed: the failure, then the exit it ends with.
    const LAST_EVENTS_OF_A_FAILED_COMMAND: [&str; 2] = [COMMAND_FAILED_EVENT, EXITED_EVENT];
    /// The events an `mcp` run whose first input is no JSON writes, in order: its start, the handshake that failed, the end of input and its exit.
    const EVENTS_OF_AN_MCP_RUN_WITH_NO_HANDSHAKE: [&str; 4] = [
        "process.started",
        HANDSHAKE_FAILED_EVENT,
        "process.input_ended",
        EXITED_EVENT,
    ];
    const RAW_FILE_OPENED_EVENT: &str = "raw_file_opened";
    const INITIALIZED_NOTIFICATION: &str =
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
    const CONFIG_HOME: &str = "config";
    const DATA_HOME: &str = "data";
    const RUNTIME_DIRECTORY: &str = "runtime";
    const PERMISSION_BITS: u32 = 0o777;
    const SPEC_DIRECTORY_MODE: u32 = 0o700;
    const SPEC_FILE_MODE: u32 = 0o600;
    const PERMISSIVE_UMASK: &str = "0000";
    const SPEC_HEADER_FIELDS: [&str; 5] = ["v", "run", "n", "mono_ms", "wall_ms"];
    const SPEC_FORMAT_VERSION: u64 = 1;
    const SPEC_RUN_HEX_CHARS: usize = 16;
    const SPEC_NODE_HEX_CHARS: usize = 10;
    const SPEC_START_STAMP_CHARS: usize = 16;
    const STAMP_TIME_SEPARATOR_INDEX: usize = 8;
    const STAMP_ZONE_INDEX: usize = 15;
    const NOT_JSON: &[u8] = b"not json\n";
    const BASE_HANDSHAKE_FAILURE: &str =
        "the MCP handshake failed: connection closed: initialize request\n";
    const BASE_TEST_SEND_FAILURE: &str =
        "the message was not delivered: No present member has that handle.\n";
    const INITIALIZE_REQUEST: &str = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"probe\",\"version\":\"0\"}}}\n";
    const BASE_INITIALIZE_RESPONSE: &str = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"estafette\",\"version\":\"0.1.0\"}}}\n";

    /// A scratch root under `/tmp`, short enough for an inbox socket path beneath it, with a network created in it.
    fn machine_with_network(test_tag: &str) -> PathBuf {
        let root = fresh_directory_in(
            Path::new(SHORT_SCRATCH_PARENT),
            &format!("{SCRATCH_PREFIX}-{}-{test_tag}", std::process::id()),
        )
        .unwrap();
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

    /// Runs the binary with `input` on its standard input until it exits.
    fn run_with_input(root: &Path, arguments: &[&str], input: &[u8]) -> Output {
        let mut child = binary(root)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }

    /// The mode bits of a path.
    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & PERMISSION_BITS
    }

    #[test]
    fn observability_decision_3_by_default_standard_error_gains_one_first_line_naming_both_files() {
        let root = machine_with_network("first-line");

        let output = run_with_input(&root, &["mcp", NETWORK], NOT_JSON);

        assert_eq!(output.status.code(), Some(0));
        assert_eq!(output.stdout, Vec::<u8>::new());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!(
                "{}{BASE_HANDSHAKE_FAILURE}",
                announcement_of_one_run(&root.join(DATA_HOME))
            )
        );
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_4_the_trace_directory_is_0700_and_its_files_0600_under_a_permissive_umask()
     {
        let root = machine_with_network("modes");

        let output = Command::new("sh")
            .args([
                "-c",
                &format!("umask {PERMISSIVE_UMASK}; exec \"$0\" \"$@\""),
                BINARY,
            ])
            .args(["mcp", NETWORK])
            .env_clear()
            .env("XDG_CONFIG_HOME", root.join(CONFIG_HOME))
            .env("XDG_DATA_HOME", root.join(DATA_HOME))
            .env("XDG_RUNTIME_DIR", root.join(RUNTIME_DIRECTORY))
            .stdin(Stdio::null())
            .output()
            .unwrap();

        assert!(output.status.success(), "{output:?}");
        let (events, raw) = trace_files_of_one_run(&root.join(DATA_HOME));
        assert_eq!(
            mode_of(&traces_directory(&root.join(DATA_HOME))),
            SPEC_DIRECTORY_MODE
        );
        assert_eq!(mode_of(&events), SPEC_FILE_MODE);
        assert_eq!(mode_of(&raw), SPEC_FILE_MODE);
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_4_a_run_is_named_by_its_start_and_run_id_never_a_pid_and_never_in_the_runtime_directory()
     {
        let root = machine_with_network("names");

        let output = run_with_input(&root, &["mcp", NETWORK], NOT_JSON);

        assert!(output.status.success(), "{output:?}");
        let (events, raw) = trace_files_of_one_run(&root.join(DATA_HOME));
        let run_name = events
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_suffix(EVENTS_SUFFIX)
            .unwrap()
            .to_owned();
        let [start, run, program] = run_name.split('-').collect::<Vec<_>>()[..] else {
            panic!("<start>-<run>-<program>: {run_name}");
        };
        assert_eq!(start.len(), SPEC_START_STAMP_CHARS, "{run_name}");
        assert_eq!(
            start.chars().nth(STAMP_TIME_SEPARATOR_INDEX),
            Some('T'),
            "{run_name}"
        );
        assert_eq!(start.chars().nth(STAMP_ZONE_INDEX), Some('Z'), "{run_name}");
        assert!(
            start.chars().enumerate().all(|(index, character)| {
                index == STAMP_TIME_SEPARATOR_INDEX
                    || index == STAMP_ZONE_INDEX
                    || character.is_ascii_digit()
            }),
            "{run_name}"
        );
        assert_eq!(run.len(), SPEC_RUN_HEX_CHARS, "{run_name}");
        assert!(
            run.chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        );
        assert_eq!(program, "estafette");
        assert_eq!(
            raw.file_name().unwrap().to_str().unwrap(),
            format!("{run_name}{RAW_SUFFIX}")
        );
        let runtime_entries: Vec<_> = fs::read_dir(root.join(RUNTIME_DIRECTORY))
            .unwrap()
            .flat_map(|entry| fs::read_dir(entry.unwrap().path()).unwrap())
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(runtime_entries, Vec::<std::ffi::OsString>::new());
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_5_the_raw_file_holds_one_header_line_saying_it_is_not_safe_to_post() {
        let root = machine_with_network("raw-line");

        let output = run_with_input(&root, &["mcp", NETWORK], NOT_JSON);

        assert!(output.status.success(), "{output:?}");
        let (events, raw) = trace_files_of_one_run(&root.join(DATA_HOME));
        let lines = stopped_trace_lines_of(&raw);
        let [line] = lines.as_slice() else {
            panic!("the raw file holds one line: {lines:?}");
        };
        assert_eq!(
            line.keys().collect::<Vec<_>>(),
            [
                "v",
                "run",
                "n",
                "mono_ms",
                "wall_ms",
                "event",
                "safe_to_post"
            ]
        );
        assert_eq!(line.value("v"), Some(&json!(SPEC_FORMAT_VERSION)));
        assert_eq!(line.n, 0);
        assert_eq!(line.event, RAW_FILE_OPENED_EVENT);
        assert_eq!(line.value("safe_to_post"), Some(&json!(false)));
        let run = line.run.as_str();
        assert!(
            events.to_str().unwrap().contains(&format!("-{run}-")),
            "{events:?} {run}"
        );
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_9_test_send_records_the_claim_of_its_operator_card_spec_3_13() {
        const CLAIMED_EVENT: &str = "process.claimed";
        const SPEC_OPERATOR_CARD: &str = "operator";
        let root = machine_with_network("test-send-claim");

        let sent = binary(&root)
            .args(["test-send", NETWORK, UNKNOWN_HANDLE, BODY])
            .output()
            .unwrap();

        assert_eq!(sent.status.code(), Some(1));
        let runs: Vec<PathBuf> = fs::read_dir(traces_directory(&root.join(DATA_HOME)))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_str().unwrap().ends_with(EVENTS_SUFFIX))
            .collect();
        let [run] = runs.as_slice() else {
            panic!("one test-send run: {runs:?}");
        };
        let lines = stopped_trace_lines_of(run);
        let claimed: Vec<&TraceLine> = lines
            .iter()
            .filter(|line| line.event == CLAIMED_EVENT)
            .collect();
        let [claimed] = claimed.as_slice() else {
            panic!("one claim: {lines:?}");
        };
        assert_eq!(claimed.value("card"), Some(&json!(SPEC_OPERATOR_CARD)));
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_5_every_events_line_begins_with_the_header_and_names_the_node_once_it_has_an_endpoint()
     {
        let root = machine_with_network("events-header");

        let sent = binary(&root)
            .args(["test-send", NETWORK, UNKNOWN_HANDLE, BODY])
            .output()
            .unwrap();
        let handshake = run_with_input(&root, &["mcp", NETWORK], NOT_JSON);

        assert_eq!(sent.status.code(), Some(1));
        assert!(handshake.status.success(), "{handshake:?}");
        let listed = binary(&root)
            .args(["list-cards", NETWORK])
            .output()
            .unwrap();
        let operator_id = String::from_utf8(listed.stdout)
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("operator\t"))
            .and_then(|rest| rest.split('\t').nth(1))
            .unwrap()
            .to_owned();
        let mut runs: Vec<PathBuf> = fs::read_dir(traces_directory(&root.join(DATA_HOME)))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_str().unwrap().ends_with(EVENTS_SUFFIX))
            .collect();
        runs.sort();
        let (test_send_lines, mcp_lines): (Vec<_>, Vec<_>) = runs
            .iter()
            .map(|path| stopped_trace_lines_of(path))
            .partition(|lines| lines.iter().any(|line| line.event == COMMAND_FAILED_EVENT));
        let [test_send_lines] = test_send_lines.as_slice() else {
            panic!("one test-send run: {runs:?}");
        };
        let [mcp_lines] = mcp_lines.as_slice() else {
            panic!("one mcp run: {runs:?}");
        };
        let test_send_events: Vec<&str> = test_send_lines
            .iter()
            .map(|line| line.event.as_str())
            .collect();
        let mcp_events: Vec<&str> = mcp_lines.iter().map(|line| line.event.as_str()).collect();
        assert!(
            test_send_events.ends_with(&LAST_EVENTS_OF_A_FAILED_COMMAND),
            "{test_send_events:?}"
        );
        assert_eq!(mcp_events, EVENTS_OF_AN_MCP_RUN_WITH_NO_HANDSHAKE);
        let failed =
            &test_send_lines[test_send_lines.len() - LAST_EVENTS_OF_A_FAILED_COMMAND.len()];
        assert_eq!(
            failed.keys().collect::<Vec<_>>(),
            ["v", "run", "n", "mono_ms", "wall_ms", "node", "event"]
        );
        assert_eq!(
            failed.node.as_deref(),
            operator_id.get(..SPEC_NODE_HEX_CHARS)
        );
        let handshake_failed = &mcp_lines[EVENTS_OF_AN_MCP_RUN_WITH_NO_HANDSHAKE
            .iter()
            .position(|event| *event == HANDSHAKE_FAILED_EVENT)
            .unwrap()];
        assert_eq!(
            handshake_failed.keys().collect::<Vec<_>>(),
            ["v", "run", "n", "mono_ms", "wall_ms", "event"]
        );
        assert_eq!(handshake_failed.event, HANDSHAKE_FAILED_EVENT);
        for line in test_send_lines.iter().chain(mcp_lines) {
            assert_eq!(
                line.keys()
                    .take(SPEC_HEADER_FIELDS.len())
                    .collect::<Vec<_>>(),
                SPEC_HEADER_FIELDS
            );
            assert_eq!(line.value("v"), Some(&json!(SPEC_FORMAT_VERSION)));
        }
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_3_mcp_under_no_debug_prints_what_the_base_printed_and_writes_no_trace()
     {
        let root = machine_with_network("no-debug-mcp");

        let refused = run_with_input(&root, &["mcp", NETWORK, NO_DEBUG], NOT_JSON);
        let mut child = binary(&root)
            .args(["mcp", NETWORK, NO_DEBUG])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(INITIALIZE_REQUEST.as_bytes()).unwrap();
        let mut response = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut response)
            .unwrap();
        drop(input);
        let initialized = child.wait_with_output().unwrap();

        assert_eq!(refused.status.code(), Some(0));
        assert_eq!(refused.stdout, Vec::<u8>::new());
        assert_eq!(
            String::from_utf8(refused.stderr).unwrap(),
            BASE_HANDSHAKE_FAILURE
        );
        assert_eq!(initialized.status.code(), Some(0));
        assert_eq!(response, BASE_INITIALIZE_RESPONSE);
        assert_eq!(initialized.stdout, Vec::<u8>::new());
        assert_eq!(initialized.stderr, Vec::<u8>::new());
        assert!(!traces_directory(&root.join(DATA_HOME)).exists());
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_3_test_send_under_no_debug_prints_what_the_base_printed_and_writes_no_trace()
     {
        let root = machine_with_network("no-debug-send");

        let output = binary(&root)
            .args(["test-send", NETWORK, UNKNOWN_HANDLE, BODY, NO_DEBUG])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, Vec::<u8>::new());
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            BASE_TEST_SEND_FAILURE
        );
        assert!(!traces_directory(&root.join(DATA_HOME)).exists());
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_3_a_trace_directory_that_cannot_be_created_refuses_the_command_before_any_other_side_effect()
     {
        let root = machine_with_network("refused");
        let traces = traces_directory(&root.join(DATA_HOME));
        fs::write(&traces, "a file where the trace directory belongs").unwrap();
        fs::remove_dir_all(root.join(RUNTIME_DIRECTORY)).unwrap();

        let output = run_with_input(&root, &["mcp", NETWORK], NOT_JSON);

        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, Vec::<u8>::new());
        let message = String::from_utf8(output.stderr).unwrap();
        assert_eq!(message.lines().count(), 1, "{message}");
        assert!(message.contains(traces.to_str().unwrap()), "{message}");
        assert!(message.contains(NO_DEBUG), "{message}");
        assert!(!message.contains("handshake"), "{message}");
        assert!(
            !root.join(RUNTIME_DIRECTORY).exists(),
            "the runtime base directory exists after the refusal"
        );
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_3_a_trace_that_cannot_start_refuses_before_any_base_directory_the_trace_does_not_need()
     {
        let root = fresh_directory_in(
            Path::new(SHORT_SCRATCH_PARENT),
            &format!("{SCRATCH_PREFIX}-{}-refused-first", std::process::id()),
        )
        .unwrap();
        let traces = traces_directory(&root.join(DATA_HOME));
        fs::create_dir_all(traces.parent().unwrap()).unwrap();
        fs::write(&traces, "a file where the trace directory belongs").unwrap();

        let output = run_with_input(&root, &["mcp", NETWORK], NOT_JSON);

        assert_eq!(output.status.code(), Some(1));
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(message.contains(traces.to_str().unwrap()), "{message}");
        assert!(
            !root.join(CONFIG_HOME).exists(),
            "the configuration base directory exists after the refusal"
        );
        assert!(
            !root.join(RUNTIME_DIRECTORY).exists(),
            "the runtime base directory exists after the refusal"
        );
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_1_no_environment_variable_switches_the_trace_off_or_on() {
        let traced_root = machine_with_network("environment-off");
        let untraced_root = machine_with_network("environment-on");

        let traced = binary(&traced_root)
            .envs(LOOKALIKE_SWITCHES_OFF)
            .args(["mcp", NETWORK])
            .output()
            .unwrap();
        let untraced = binary(&untraced_root)
            .envs(LOOKALIKE_SWITCHES_ON)
            .args(["mcp", NETWORK, NO_DEBUG])
            .output()
            .unwrap();

        assert!(traced.status.success(), "{traced:?}");
        assert!(
            traces_directory(&traced_root.join(DATA_HOME)).exists(),
            "an environment variable switched the trace off: no trace directory"
        );
        let (events, raw) = trace_files_of_one_run(&traced_root.join(DATA_HOME));
        assert!(events.exists() && raw.exists());
        assert!(untraced.status.success(), "{untraced:?}");
        assert!(!traces_directory(&untraced_root.join(DATA_HOME)).exists());
        remove_scratch(&traced_root).unwrap();
        remove_scratch(&untraced_root).unwrap();
    }

    #[test]
    fn observability_decision_1_no_configuration_field_switches_the_trace() {
        let root = machine_with_network("configuration-field");
        let configuration = root
            .join(CONFIG_HOME)
            .join(NETWORKS_UNDER_CONFIG_HOME)
            .join(format!("{NETWORK}.{TOML_EXTENSION}"));
        let written = fs::read_to_string(&configuration).unwrap();
        fs::write(
            &configuration,
            format!("{DEBUG_CONFIGURATION_FIELD}{written}"),
        )
        .unwrap();

        let output = binary(&root)
            .args(["mcp", NETWORK, NO_DEBUG])
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let message = String::from_utf8(output.stderr).unwrap();
        assert!(
            message.contains("at line 1, column 1: a key it does not know"),
            "{message}"
        );
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_3_under_debug_standard_output_is_what_the_base_printed() {
        let root = machine_with_network("debug-stdout");

        let mut child = binary(&root)
            .args(["mcp", NETWORK])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(INITIALIZE_REQUEST.as_bytes()).unwrap();
        let mut response = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut response)
            .unwrap();
        drop(input);
        let initialized = child.wait_with_output().unwrap();

        assert_eq!(initialized.status.code(), Some(0));
        assert_eq!(response, BASE_INITIALIZE_RESPONSE);
        assert_eq!(initialized.stdout, Vec::<u8>::new());
        assert_eq!(
            String::from_utf8(initialized.stderr).unwrap(),
            announcement_of_one_run(&root.join(DATA_HOME))
        );
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_4_commands_that_start_no_endpoint_write_no_trace() {
        let root = machine_with_network("no-endpoint");

        for (arguments, exit_code) in [
            (&["create-card", NETWORK, CARD, CARD][..], 0),
            (&["list-cards", NETWORK], 0),
            (&["network", "create", "other"], 0),
            (&["network", "show-invitation", NETWORK], 1),
            (&["network", "add-host", NETWORK, "not a host line"], 2),
        ] {
            let ran = binary(&root).args(arguments).output().unwrap();
            assert_eq!(ran.status.code(), Some(exit_code), "{arguments:?}: {ran:?}");
        }

        assert!(!traces_directory(&root.join(DATA_HOME)).exists());
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_5_a_restarted_process_keeps_its_node_and_changes_its_run() {
        let root = machine_with_network("restart");

        for _start in 0..2 {
            let sent = binary(&root)
                .args(["test-send", NETWORK, UNKNOWN_HANDLE, BODY])
                .output()
                .unwrap();
            assert_eq!(sent.status.code(), Some(1), "{sent:?}");
        }

        let mut runs: Vec<PathBuf> = fs::read_dir(traces_directory(&root.join(DATA_HOME)))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_str().unwrap().ends_with(EVENTS_SUFFIX))
            .collect();
        runs.sort();
        let last_lines: Vec<TraceLine> = runs
            .iter()
            .map(|path| stopped_trace_lines_of(path).pop().unwrap())
            .collect();
        let [first, second] = last_lines.as_slice() else {
            panic!("two starts: {runs:?}");
        };
        assert_eq!(first.node, second.node);
        assert!(first.node.is_some(), "{first:?}");
        assert_ne!(first.run, second.run);
        remove_scratch(&root).unwrap();
    }

    /// Whether the trace file of a running process comes to hold a whole line within `WHOLE_LINE_WAIT`.
    fn holds_a_whole_line_within_the_wait(path: &Path) -> bool {
        holds_within_the_wait(path, |lines| !lines.is_empty())
    }

    #[test]
    fn observability_decision_6_a_killed_process_leaves_in_its_trace_files_only_its_whole_numbered_lines_its_own_unended_line_and_zero_bytes()
     {
        let root = machine_with_network("killed");
        let cards = root.join(UNREADABLE_CARDS_DIRECTORY);
        fs::write(&cards, "a file where the cards directory belongs").unwrap();
        let mut child = binary(&root)
            .args(["mcp", NETWORK, "--cards-dir", cards.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut requests = format!("{INITIALIZE_REQUEST}{INITIALIZED_NOTIFICATION}");
        for id in FIRST_TOOL_CALL_ID..FIRST_TOOL_CALL_ID + KILLED_AFTER_ANSWERS {
            writeln!(
                requests,
                "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"list_cards\",\"arguments\":{{}}}}}}"
            )
            .unwrap();
        }
        input.write_all(requests.as_bytes()).unwrap();
        let answers = BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok)
            .take(KILLED_AFTER_ANSWERS + 1)
            .count();
        let (events, raw) = trace_files_of_one_run(&root.join(DATA_HOME));
        for path in [&events, &raw] {
            assert!(
                holds_a_whole_line_within_the_wait(path),
                "{}: the process wrote none of its whole numbered lines within {WHOLE_LINE_WAIT:?}",
                path.display()
            );
        }

        child.kill().unwrap();
        child.wait().unwrap();
        drop(input);

        assert_eq!(answers, KILLED_AFTER_ANSWERS + 1);
        for path in [&events, &raw] {
            let numbers: Vec<u64> = whole_lines_of_a_run_file(path)
                .iter()
                .map(|line| line.n)
                .collect();
            assert!(
                !numbers.is_empty(),
                "{}: the killed process left none of its whole numbered lines",
                path.display()
            );
            let consecutive: Vec<u64> = (0..).take(numbers.len()).collect();
            assert_eq!(numbers, consecutive, "{}", path.display());
        }
        let raw_events: Vec<String> = whole_lines_of_a_run_file(&raw)
            .into_iter()
            .map(|line| line.event)
            .collect();
        assert_eq!(raw_events, [RAW_FILE_OPENED_EVENT]);
        remove_scratch(&root).unwrap();
    }

    #[test]
    fn observability_decision_12_the_last_event_of_a_stopped_process_is_on_disk() {
        let root = machine_with_network("last-event");

        let sent = binary(&root)
            .args(["test-send", NETWORK, UNKNOWN_HANDLE, BODY])
            .output()
            .unwrap();

        assert_eq!(sent.status.code(), Some(1));
        let (events, _raw) = trace_files_of_one_run(&root.join(DATA_HOME));
        assert!(
            !fs::read(&events).unwrap().contains(&0),
            "the stopped process left its events file sized ahead of the line of its last event"
        );
        let lines = stopped_trace_lines_of(&events);
        let events_written: Vec<&str> = lines.iter().map(|line| line.event.as_str()).collect();
        assert!(
            events_written.ends_with(&LAST_EVENTS_OF_A_FAILED_COMMAND),
            "the last event of the stopped process is not the last line of its events file: {lines:?}"
        );
        remove_scratch(&root).unwrap();
    }
}
