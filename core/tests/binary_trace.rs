#[cfg(test)]
#[path = "support/trace_files.rs"]
mod trace_files;

/// The trace seam of spec/observability.md, decisions 3, 4, 5 and 12, through the shipped binary.
#[cfg(test)]
mod observability {
    use std::fmt;
    use std::fmt::Write as _;
    use std::fs;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output, Stdio};

    use super::trace_files::{
        EVENTS_SUFFIX, RAW_SUFFIX, announcement_of_one_run, trace_files_of_one_run,
        traces_directory,
    };
    use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};
    use serde_json::Value;

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

    /// The keys of a JSON object in the order the line writes them.
    struct KeysInOrder(Vec<String>);

    impl<'de> Deserialize<'de> for KeysInOrder {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct KeysVisitor;

            impl<'de> Visitor<'de> for KeysVisitor {
                type Value = KeysInOrder;

                fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    formatter.write_str("a JSON object")
                }

                fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<KeysInOrder, A::Error> {
                    let mut keys = Vec::new();
                    while let Some(key) = map.next_key::<String>()? {
                        map.next_value::<IgnoredAny>()?;
                        keys.push(key);
                    }
                    Ok(KeysInOrder(keys))
                }
            }

            deserializer.deserialize_map(KeysVisitor)
        }
    }

    /// One line of a trace file: its keys in written order and its values by key.
    #[derive(Debug)]
    struct TraceLine {
        keys: Vec<String>,
        values: serde_json::Map<String, Value>,
    }

    /// Each line of a trace file.
    fn trace_lines(path: &Path) -> Vec<TraceLine> {
        BufReader::new(fs::File::open(path).unwrap())
            .lines()
            .map(|line| {
                let line = line.unwrap();
                let KeysInOrder(keys) = serde_json::from_str(&line).unwrap();
                let Value::Object(values) = serde_json::from_str(&line).unwrap() else {
                    panic!("a trace line is a JSON object: {line}");
                };
                TraceLine { keys, values }
            })
            .collect()
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
        drop(fs::remove_dir_all(&root));
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
        drop(fs::remove_dir_all(&root));
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
        drop(fs::remove_dir_all(&root));
    }

    /// The keys of a trace line in their written order.
    fn keys_of(line: &TraceLine) -> Vec<&str> {
        line.keys.iter().map(String::as_str).collect()
    }

    #[test]
    fn observability_decision_5_the_raw_file_holds_one_header_line_saying_it_is_not_safe_to_post() {
        let root = machine_with_network("raw-line");

        let output = run_with_input(&root, &["mcp", NETWORK], NOT_JSON);

        assert!(output.status.success(), "{output:?}");
        let (events, raw) = trace_files_of_one_run(&root.join(DATA_HOME));
        let lines = trace_lines(&raw);
        let [line] = lines.as_slice() else {
            panic!("the raw file holds one line: {lines:?}");
        };
        assert_eq!(
            keys_of(line),
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
        assert_eq!(line.values["v"], SPEC_FORMAT_VERSION);
        assert_eq!(line.values["n"], 0);
        assert_eq!(line.values["event"], "raw_file_opened");
        assert_eq!(line.values["safe_to_post"], false);
        let run = line.values["run"].as_str().unwrap();
        assert!(
            events.to_str().unwrap().contains(&format!("-{run}-")),
            "{events:?} {run}"
        );
        drop(fs::remove_dir_all(&root));
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
            .map(|path| trace_lines(path))
            .partition(|lines| {
                lines
                    .iter()
                    .any(|line| line.values["event"] == "process.command_failed")
            });
        let [test_send_lines] = test_send_lines.as_slice() else {
            panic!("one test-send run: {runs:?}");
        };
        let [mcp_lines] = mcp_lines.as_slice() else {
            panic!("one mcp run: {runs:?}");
        };
        let failed = test_send_lines.last().unwrap();
        assert_eq!(
            keys_of(failed),
            ["v", "run", "n", "mono_ms", "wall_ms", "node", "event"]
        );
        assert_eq!(
            failed.values["node"],
            operator_id.get(..SPEC_NODE_HEX_CHARS).unwrap()
        );
        let handshake_failed = mcp_lines.last().unwrap();
        assert_eq!(
            keys_of(handshake_failed),
            ["v", "run", "n", "mono_ms", "wall_ms", "event"]
        );
        assert_eq!(handshake_failed.values["event"], "tool.handshake_failed");
        for line in test_send_lines.iter().chain(mcp_lines) {
            assert_eq!(
                keys_of(line)[..SPEC_HEADER_FIELDS.len()],
                SPEC_HEADER_FIELDS
            );
            assert_eq!(line.values["v"], SPEC_FORMAT_VERSION);
        }
        drop(fs::remove_dir_all(&root));
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
        drop(fs::remove_dir_all(&root));
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
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_3_a_trace_directory_that_cannot_be_created_refuses_the_command_before_any_other_side_effect()
     {
        let root = machine_with_network("refused");
        let traces = traces_directory(&root.join(DATA_HOME));
        fs::write(&traces, "a file where the trace directory belongs").unwrap();

        let output = run_with_input(&root, &["mcp", NETWORK], NOT_JSON);

        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, Vec::<u8>::new());
        let message = String::from_utf8(output.stderr).unwrap();
        assert_eq!(message.lines().count(), 1, "{message}");
        assert!(message.contains(traces.to_str().unwrap()), "{message}");
        assert!(message.contains(NO_DEBUG), "{message}");
        assert!(!message.contains("handshake"), "{message}");
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_3_a_trace_that_cannot_start_refuses_before_any_base_directory_the_trace_does_not_need()
     {
        let root = PathBuf::from(SHORT_SCRATCH_PARENT).join(format!(
            "{SCRATCH_PREFIX}-{}-refused-first",
            std::process::id()
        ));
        drop(fs::remove_dir_all(&root));
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
        drop(fs::remove_dir_all(&root));
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
        let (events, raw) = trace_files_of_one_run(&traced_root.join(DATA_HOME));
        assert!(events.exists() && raw.exists());
        assert!(untraced.status.success(), "{untraced:?}");
        assert!(!traces_directory(&untraced_root.join(DATA_HOME)).exists());
        drop(fs::remove_dir_all(&traced_root));
        drop(fs::remove_dir_all(&untraced_root));
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
        drop(fs::remove_dir_all(&root));
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
        drop(fs::remove_dir_all(&root));
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
        drop(fs::remove_dir_all(&root));
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
            .map(|path| trace_lines(path).pop().unwrap())
            .collect();
        let [first, second] = last_lines.as_slice() else {
            panic!("two starts: {runs:?}");
        };
        assert_eq!(first.values["node"], second.values["node"]);
        assert!(first.values["node"].is_string(), "{:?}", first.values);
        assert_ne!(first.values["run"], second.values["run"]);
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn observability_decision_12_a_killed_process_loses_at_most_its_line_in_flight_and_every_ended_line_is_whole_and_numbered_without_a_gap()
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

        child.kill().unwrap();
        child.wait().unwrap();
        drop(input);

        assert_eq!(answers, KILLED_AFTER_ANSWERS + 1);
        for path in <[PathBuf; 2]>::from(trace_files_of_one_run(&root.join(DATA_HOME))) {
            let bytes = fs::read(&path).unwrap();
            let numbers: Vec<Value> = bytes
                .split_inclusive(|byte| *byte == b'\n')
                .filter(|line| line.ends_with(b"\n"))
                .map(|line| serde_json::from_slice::<Value>(line).unwrap()["n"].clone())
                .collect();
            let consecutive: Vec<Value> = (0..numbers.len()).map(Value::from).collect();
            assert_eq!(numbers, consecutive, "{}", path.display());
            assert!(
                bytes
                    .split_inclusive(|byte| *byte == b'\n')
                    .filter(|line| !line.ends_with(b"\n"))
                    .count()
                    <= 1,
                "{}",
                path.display()
            );
        }
        let (_events, raw) = trace_files_of_one_run(&root.join(DATA_HOME));
        let raw_bytes = fs::read(&raw).unwrap();
        let first_raw_line = raw_bytes
            .split_inclusive(|byte| *byte == b'\n')
            .next()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(first_raw_line).unwrap()["event"],
            "raw_file_opened"
        );
        drop(fs::remove_dir_all(&root));
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
        let lines = trace_lines(&events);
        assert_eq!(
            lines.last().unwrap().values["event"],
            "process.command_failed",
            "{lines:?}"
        );
        drop(fs::remove_dir_all(&root));
    }
}
