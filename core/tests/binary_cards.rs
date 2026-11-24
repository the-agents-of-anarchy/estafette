use std::fs::Permissions;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

mod common;
#[path = "common/remove_scratch.rs"]
mod remove_scratch;

const SCRATCH_PREFIX: &str = "estafette-binary-cards";
const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
const NETWORK: &str = "acme";
const USAGE_ERROR_CODE: i32 = 2;
const PUBLIC_KEY_HEX_LINE_LENGTH: usize = 65;
const PERMISSION_BITS: u32 = 0o777;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const CARD_FILE_MODE: u32 = 0o644;
const KEY_FILE_MODE: u32 = 0o600;
/// A umask that clears every permission bit of group and others.
const RESTRICTIVE_UMASK: &str = "077";
/// A umask that clears the owner's read bit, which a file written with the default mode would lose.
const READ_BLOCKING_UMASK: &str = "0400";
/// A umask that clears the owner's search bit, which a directory made with the default mode would lose.
const SEARCH_BLOCKING_UMASK: &str = "0100";
/// A umask that clears nothing.
const PERMISSIVE_UMASK: &str = "0000";
/// The umasks a key file must come out of owner read-write alone (spec 3.3).
const KEY_FILE_UMASKS: [&str; 2] = [READ_BLOCKING_UMASK, PERMISSIVE_UMASK];
/// The umasks a program directory must come out of owner-only (spec 3.3).
const DIRECTORY_UMASKS: [&str; 2] = [SEARCH_BLOCKING_UMASK, PERMISSIVE_UMASK];

/// The program run with the environment of a scratch root alone: its configuration, data and runtime homes.
fn command_in_scratch_environment(program: &str, root: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"));
    command
}

fn run_binary_under_umask(root: &Path, umask: &str, arguments: &[&str]) -> io::Result<Output> {
    command_in_scratch_environment("sh", root)
        .args(["-c", &format!("umask {umask}; exec \"$0\" \"$@\""), BINARY])
        .args(arguments)
        .output()
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn require_success(output: &Output) -> io::Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(stderr_text(output)))
    }
}

fn mode_of(path: &Path) -> io::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & PERMISSION_BITS)
}

fn run_binary(root: &Path, arguments: &[&str]) -> io::Result<Output> {
    command_in_scratch_environment(BINARY, root)
        .args(arguments)
        .output()
}

#[test]
fn binary_create_card_then_list_cards_round_trips_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "round_trip").unwrap();
    let created = run_binary(&root, &["create-card", NETWORK, "ana-card", "ana"]).unwrap();
    assert_eq!(created.status.code(), Some(0), "{}", stderr_text(&created));
    let printed_key = String::from_utf8(created.stdout).unwrap();
    assert_eq!(printed_key.len(), PUBLIC_KEY_HEX_LINE_LENGTH);
    assert_eq!(created.stderr, Vec::<u8>::new());

    let listed = run_binary(&root, &["list-cards", NETWORK]).unwrap();
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_text(&listed));
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        format!("ana-card\tana\t{printed_key}")
    );
}

#[test]
fn binary_create_card_twice_exits_one_with_the_message_alone_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "twice").unwrap();
    run_binary(&root, &["create-card", NETWORK, "ana-card", "ana"]).unwrap();
    let second = run_binary(&root, &["create-card", NETWORK, "ana-card", "bob"]).unwrap();
    assert_eq!(second.status.code(), Some(1), "{}", stderr_text(&second));
    assert_eq!(second.stdout, Vec::<u8>::new());
    assert_eq!(
        String::from_utf8(second.stderr).unwrap(),
        "the card or its sidecar already exists; create never overwrites\n"
    );
}

#[test]
fn binary_cards_dir_flag_places_the_card_there_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "flag").unwrap();
    let flagged = root.join("flagged");
    let created = run_binary(
        &root,
        &[
            "create-card",
            NETWORK,
            "ana-card",
            "ana",
            "--cards-dir",
            flagged.to_str().unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(created.status.code(), Some(0), "{}", stderr_text(&created));
    assert!(flagged.join("acme/ana-card.yaml").is_file());
}

#[test]
fn binary_usage_error_exits_two_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "usage").unwrap();
    let missing_arguments = run_binary(&root, &["create-card", NETWORK]).unwrap();
    assert_eq!(
        missing_arguments.status.code(),
        Some(USAGE_ERROR_CODE),
        "{}",
        stderr_text(&missing_arguments)
    );
    assert_eq!(missing_arguments.stdout, Vec::<u8>::new());
    let unknown_command = run_binary(&root, &["no-such-command"]).unwrap();
    assert_eq!(
        unknown_command.status.code(),
        Some(USAGE_ERROR_CODE),
        "{}",
        stderr_text(&unknown_command)
    );
}

#[test]
fn binary_names_outside_the_handle_grammar_are_usage_errors_before_any_write_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "bad_names").unwrap();
    let refused_arguments: [&[&str]; 5] = [
        &["create-card", NETWORK, "Ana", "ana"],
        &["create-card", NETWORK, "../up", "ana"],
        &["create-card", NETWORK, "ana", "Ana"],
        &["create-card", "Net", "ana", "ana"],
        &["list-cards", "Net"],
    ];
    for arguments in refused_arguments {
        let refused = run_binary(&root, arguments).unwrap();
        assert_eq!(
            refused.status.code(),
            Some(USAGE_ERROR_CODE),
            "{arguments:?}: {}",
            stderr_text(&refused)
        );
        assert_eq!(refused.stdout, Vec::<u8>::new());
    }
    assert!(!root.join("data").exists());
}

#[test]
fn binary_help_of_each_card_command_states_what_it_does_reads_produces_and_refuses_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "help").unwrap();
    let help_sentences: [(&str, &[&str]); 2] = [
        (
            "create-card",
            &[
                "Create a card",
                "Reads: ",
                "Produces: the configuration, data and runtime directories of this program, readable by you alone, when missing; the card file <card>.yaml with mode 0644 and the key file <card>.key, readable by you alone",
                "Refuses: a network, a card or a handle",
            ],
        ),
        (
            "list-cards",
            &[
                "List the cards",
                "Reads: ",
                "Produces: the configuration, data and runtime directories of this program, readable by you alone, when missing; one line per card",
                "Refuses: a network that breaks this rule",
            ],
        ),
    ];
    for (subcommand, stated) in help_sentences {
        let output = run_binary(&root, &[subcommand, "--help"]).unwrap();
        let help = String::from_utf8(output.stdout).unwrap();
        for sentence in stated {
            assert!(help.contains(sentence), "{subcommand}: {help}");
        }
        assert!(
            !help.contains("override every card command takes"),
            "{help}"
        );
    }
}

#[test]
fn binary_create_card_creates_the_program_directories_and_two_files_with_the_modes_its_help_states()
{
    let root = common::scratch_root(SCRATCH_PREFIX, "created").unwrap();
    let created = run_binary(&root, &["create-card", NETWORK, "ana-card", "ana"]).unwrap();
    assert_eq!(created.status.code(), Some(0), "{}", stderr_text(&created));
    assert_eq!(
        mode_of(&root.join("config/estafette")).unwrap(),
        PRIVATE_DIRECTORY_MODE
    );
    assert_eq!(
        mode_of(&root.join("data/estafette")).unwrap(),
        PRIVATE_DIRECTORY_MODE
    );
    let runtime_directories: Vec<_> = std::fs::read_dir(root.join("runtime"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(runtime_directories.len(), 1);
    assert_eq!(
        mode_of(&runtime_directories[0]).unwrap(),
        PRIVATE_DIRECTORY_MODE
    );
    let cards = root.join("data/estafette/cards/acme");
    assert_eq!(
        mode_of(&cards.join("ana-card.yaml")).unwrap(),
        CARD_FILE_MODE
    );
    assert_eq!(mode_of(&cards.join("ana-card.key")).unwrap(), KEY_FILE_MODE);
}

#[test]
fn binary_list_cards_creates_the_program_directories_as_its_help_states() {
    let root = common::scratch_root(SCRATCH_PREFIX, "list_creates_directories").unwrap();
    let listed = run_binary(&root, &["list-cards", NETWORK]).unwrap();
    assert_eq!(listed.status.code(), Some(0), "{}", stderr_text(&listed));
    assert_eq!(listed.stdout, Vec::<u8>::new());
    for created in ["config/estafette", "data/estafette"] {
        assert_eq!(
            mode_of(&root.join(created)).unwrap(),
            PRIVATE_DIRECTORY_MODE,
            "{created}"
        );
    }
    assert_eq!(std::fs::read_dir(root.join("runtime")).unwrap().count(), 1);
}

#[test]
fn binary_create_card_sets_exact_file_modes_under_a_restrictive_umask_of_spec_3_5() {
    let root = common::scratch_root(SCRATCH_PREFIX, "restrictive_umask").unwrap();
    let created = run_binary_under_umask(
        &root,
        RESTRICTIVE_UMASK,
        &["create-card", NETWORK, "ana-card", "ana"],
    )
    .unwrap();
    assert_eq!(created.status.code(), Some(0), "{}", stderr_text(&created));
    let cards = root.join("data/estafette/cards/acme");
    assert_eq!(
        mode_of(&cards.join("ana-card.yaml")).unwrap(),
        CARD_FILE_MODE
    );
    assert_eq!(mode_of(&cards.join("ana-card.key")).unwrap(), KEY_FILE_MODE);
}

#[test]
fn binary_owner_key_is_owner_read_write_under_any_umask_of_spec_3_3() {
    for umask in KEY_FILE_UMASKS {
        let root = common::scratch_root(SCRATCH_PREFIX, &format!("key_modes_{umask}")).unwrap();
        let network =
            run_binary_under_umask(&root, umask, &["network", "create", NETWORK]).unwrap();
        assert_eq!(
            network.status.code(),
            Some(0),
            "umask {umask}: {}",
            stderr_text(&network)
        );
        assert_eq!(
            mode_of(&root.join("config/estafette/networks/acme.owner.key")).unwrap(),
            KEY_FILE_MODE,
            "owner key, umask {umask}"
        );
    }
}

const LEFTOVER_PREFIX: &str = "estafette-scratch-root-leftover";
const LEFTOVER_TEST_NAME: &str = "planted";
const MADE_NOWHERE_TEST_NAME: &str = "made_nowhere";
const REMOVED_NOWHERE_TEST_NAME: &str = "removed_nowhere";
const EXHAUSTED_TEST_NAME: &str = "exhausted";
const TAKEN_CHILD_NAME: &str = "taken";
const UNLISTABLE_PARENT: &str = "config";
const PLANTED_FILE: &str = "config/estafette/networks/acme.toml";
const PLANTED_FILE_TEXT: &str = "an earlier run's network";
const ABSENT_NAME: &str = "absent";
const UNMADE_CHILD_NAME: &str = "child";
const UNLISTABLE_PARENT_MODE: u32 = 0o377;
const LISTABLE_PARENT_MODE: u32 = 0o700;

fn plant_leftover_that_cannot_be_listed_or_removed(leftover: &Path) -> io::Result<()> {
    let planted_file = leftover.join(PLANTED_FILE);
    std::fs::create_dir_all(planted_file.parent().unwrap_or(leftover))?;
    std::fs::write(planted_file, PLANTED_FILE_TEXT)?;
    std::fs::set_permissions(
        leftover.join(UNLISTABLE_PARENT),
        Permissions::from_mode(UNLISTABLE_PARENT_MODE),
    )
}

fn clear_planted_leftover(leftover: &Path) -> io::Result<()> {
    std::fs::set_permissions(
        leftover.join(UNLISTABLE_PARENT),
        Permissions::from_mode(LISTABLE_PARENT_MODE),
    )?;
    remove_scratch::remove_scratch(leftover)
}

/// A planted leftover cleared when this value drops, so a failing assertion leaves nothing in the temp directory, a clearing that fails failing the test unless it is already failing.
struct PlantedLeftover {
    path: PathBuf,
}

impl Drop for PlantedLeftover {
    fn drop(&mut self) {
        if let Err(failure) = clear_planted_leftover(&self.path) {
            let report = format!("planted leftover not cleared: {failure}");
            assert!(std::thread::panicking(), "{report}");
            drop(writeln!(io::stderr(), "{report}"));
        }
    }
}

#[test]
fn scratch_root_hands_out_an_empty_root_though_an_earlier_run_left_an_unremovable_one_under_its_name()
 {
    let leftover = PlantedLeftover {
        path: std::env::temp_dir().join(format!(
            "{LEFTOVER_PREFIX}-{LEFTOVER_TEST_NAME}-{}",
            std::process::id()
        )),
    };
    plant_leftover_that_cannot_be_listed_or_removed(&leftover.path).unwrap();

    let root = common::scratch_root(LEFTOVER_PREFIX, LEFTOVER_TEST_NAME).unwrap();

    assert_eq!(root.parent(), leftover.path.parent());
    let leftover_name = leftover.path.file_name().unwrap().to_string_lossy();
    let root_name = root.file_name().unwrap().to_string_lossy();
    assert!(
        root_name
            .strip_prefix(&format!("{leftover_name}-"))
            .is_some_and(|number| number.parse::<u32>().is_ok()),
        "{} was not handed out as the leftover's name {} and a number",
        root.display(),
        leftover.path.display()
    );
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    remove_scratch::remove_scratch(&root).unwrap();
}

#[test]
fn scratch_directory_that_cannot_be_made_fails_naming_its_path_and_the_cause() {
    let root = common::scratch_root(LEFTOVER_PREFIX, MADE_NOWHERE_TEST_NAME).unwrap();
    let absent_parent = root.join(ABSENT_NAME);

    let failure =
        common::fresh_directory::fresh_directory_in(&absent_parent, UNMADE_CHILD_NAME).unwrap_err();

    assert_eq!(failure.kind(), io::ErrorKind::NotFound);
    assert!(
        failure.to_string().starts_with(&format!(
            "{}: ",
            absent_parent.join(UNMADE_CHILD_NAME).display()
        )),
        "{failure}"
    );
    remove_scratch::remove_scratch(&root).unwrap();
}

#[test]
fn scratch_directory_whose_every_numbered_name_is_taken_fails_naming_its_parent_and_the_name() {
    let root = common::scratch_root(LEFTOVER_PREFIX, EXHAUSTED_TEST_NAME).unwrap();
    std::fs::create_dir(root.join(TAKEN_CHILD_NAME)).unwrap();
    for number in 1..=common::fresh_directory::NUMBERED_NAMES_TRIED {
        std::fs::create_dir(root.join(format!("{TAKEN_CHILD_NAME}-{number}"))).unwrap();
    }

    let failure = common::fresh_directory::fresh_directory_in(&root, TAKEN_CHILD_NAME).unwrap_err();

    assert_eq!(failure.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(
        failure.to_string(),
        format!(
            "{}: every numbered name of {TAKEN_CHILD_NAME} is taken",
            root.display()
        )
    );
    remove_scratch::remove_scratch(&root).unwrap();
}

#[test]
fn scratch_path_that_cannot_be_removed_fails_naming_its_path_and_the_cause() {
    let root = common::scratch_root(LEFTOVER_PREFIX, REMOVED_NOWHERE_TEST_NAME).unwrap();
    let absent = root.join(ABSENT_NAME);

    let failure = remove_scratch::remove_scratch(&absent).unwrap_err();

    assert_eq!(failure.kind(), io::ErrorKind::NotFound);
    assert!(
        failure
            .to_string()
            .starts_with(&format!("{}: ", absent.display())),
        "{failure}"
    );
    remove_scratch::remove_scratch(&root).unwrap();
}

#[test]
fn binary_program_directories_are_owner_only_under_any_umask_of_spec_3_3() {
    for umask in DIRECTORY_UMASKS {
        let root = common::scratch_root(SCRATCH_PREFIX, &format!("dir_modes_{umask}")).unwrap();
        for base in ["config", "data", "runtime"] {
            std::fs::create_dir(root.join(base)).unwrap();
        }
        let listed = run_binary_under_umask(&root, umask, &["list-cards", NETWORK]).unwrap();
        assert_eq!(
            listed.status.code(),
            Some(0),
            "umask {umask}: {}",
            stderr_text(&listed)
        );
        for created in ["config/estafette", "data/estafette"] {
            assert_eq!(
                mode_of(&root.join(created)).unwrap(),
                PRIVATE_DIRECTORY_MODE,
                "{created}, umask {umask}"
            );
        }
        let runtime_directories: Vec<_> = std::fs::read_dir(root.join("runtime"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(runtime_directories.len(), 1);
        assert_eq!(
            mode_of(&runtime_directories[0]).unwrap(),
            PRIVATE_DIRECTORY_MODE,
            "runtime, umask {umask}"
        );
    }
}

/// The relay domain a host is run with, whose certificate is never obtained since the run ends at the missing host certificate.
#[cfg(feature = "relay")]
const HOST_KEY_RELAY_DOMAIN: &str = "relay.example.test";
/// The address a host is run with, never bound since the run ends at the missing host certificate.
#[cfg(feature = "relay")]
const HOST_KEY_BIND_ADDRESS: &str = "127.0.0.1";
/// The exit code of a host run that ends at its missing certificate, after it wrote its key (spec 3.14).
#[cfg(feature = "relay")]
const MISSING_CERTIFICATE_EXIT_CODE: i32 = 1;

#[cfg(feature = "relay")]
#[test]
fn binary_host_key_is_owner_read_write_under_any_umask_of_spec_3_3() {
    for umask in KEY_FILE_UMASKS {
        let root =
            common::scratch_root(SCRATCH_PREFIX, &format!("host_key_modes_{umask}")).unwrap();
        let network =
            run_binary_under_umask(&root, umask, &["network", "create", NETWORK]).unwrap();
        require_success(&network).unwrap();

        let started = run_binary_under_umask(
            &root,
            umask,
            &[
                "run-infrastructure",
                NETWORK,
                HOST_KEY_BIND_ADDRESS,
                HOST_KEY_RELAY_DOMAIN,
            ],
        )
        .unwrap();

        assert_eq!(
            started.status.code(),
            Some(MISSING_CERTIFICATE_EXIT_CODE),
            "umask {umask}: {}",
            stderr_text(&started)
        );
        assert_eq!(
            mode_of(&root.join(format!("data/estafette/infra/{NETWORK}.key"))).unwrap(),
            KEY_FILE_MODE,
            "host key, umask {umask}"
        );
    }
}

const CARDS_DIRECTORY_VARIABLE: &str = "ESTAFETTE_CARDS_DIR";
const CARDS_CHECK_LINE: &str = "cards\tpass\tcards valid: 1";

fn create_network_and_card_in(root: &Path, cards_directory: &str) -> io::Result<()> {
    let network = run_binary(root, &["network", "create", NETWORK])?;
    require_success(&network)?;
    let card = run_binary(
        root,
        &[
            "create-card",
            NETWORK,
            "ana",
            "ana",
            "--cards-dir",
            cards_directory,
        ],
    )?;
    require_success(&card)
}

#[test]
fn binary_doctor_reads_the_cards_of_the_cards_dir_flag_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "doctor_flag").unwrap();
    let flagged = root.join("flagged");
    create_network_and_card_in(&root, flagged.to_str().unwrap()).unwrap();

    let diagnosed = run_binary(
        &root,
        &["doctor", NETWORK, "--cards-dir", flagged.to_str().unwrap()],
    )
    .unwrap();

    let report = String::from_utf8(diagnosed.stdout).unwrap();
    assert!(report.contains(CARDS_CHECK_LINE), "{report}");
}

#[test]
fn binary_doctor_reads_the_cards_of_the_cards_dir_flag_over_the_variable_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "doctor_flag_over_variable").unwrap();
    let flagged = root.join("flagged");
    let cardless = root.join("cardless");
    create_network_and_card_in(&root, flagged.to_str().unwrap()).unwrap();

    let diagnosed = command_in_scratch_environment(BINARY, &root)
        .args(["doctor", NETWORK, "--cards-dir", flagged.to_str().unwrap()])
        .env(CARDS_DIRECTORY_VARIABLE, &cardless)
        .output()
        .unwrap();

    let report = String::from_utf8(diagnosed.stdout).unwrap();
    assert!(report.contains(CARDS_CHECK_LINE), "{report}");
}

#[test]
fn binary_doctor_reads_the_cards_of_the_cards_dir_variable_without_the_flag_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "doctor_variable_alone").unwrap();
    let variable_directory = root.join("variable");
    create_network_and_card_in(&root, variable_directory.to_str().unwrap()).unwrap();

    let diagnosed = command_in_scratch_environment(BINARY, &root)
        .args(["doctor", NETWORK])
        .env(CARDS_DIRECTORY_VARIABLE, &variable_directory)
        .output()
        .unwrap();

    let report = String::from_utf8(diagnosed.stdout).unwrap();
    assert!(report.contains(CARDS_CHECK_LINE), "{report}");
}

#[test]
fn binary_test_send_creates_its_operator_card_in_the_cards_dir_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "test_send_flag").unwrap();
    let flagged = root.join("flagged");
    create_network_and_card_in(&root, flagged.to_str().unwrap()).unwrap();

    run_binary(
        &root,
        &[
            "test-send",
            NETWORK,
            "ana",
            "hello",
            "--cards-dir",
            flagged.to_str().unwrap(),
        ],
    )
    .unwrap();

    assert!(flagged.join("acme/operator.yaml").is_file());
    assert!(
        !root
            .join("data/estafette/cards/acme/operator.yaml")
            .exists()
    );
}

const COMMANDS_THAT_READ_CARDS: [&str; 2] = ["test-send", "doctor"];
const COMMANDS_THAT_READ_NO_CARD: [&str; 2] = ["show-peers", "tail"];

#[test]
fn binary_help_names_the_cards_dir_flag_of_the_commands_that_read_cards_alone_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "observer_help").unwrap();
    for subcommand in COMMANDS_THAT_READ_CARDS {
        let output = run_binary(&root, &[subcommand, "--help"]).unwrap();
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--cards-dir"), "{subcommand}: {help}");
    }
    for subcommand in COMMANDS_THAT_READ_NO_CARD {
        let output = run_binary(&root, &[subcommand, "--help"]).unwrap();
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(!help.contains("cards-dir"), "{subcommand}: {help}");
    }
}

#[test]
fn binary_show_peers_and_tail_refuse_the_cards_dir_flag_as_a_usage_error_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "observer_flag_refused").unwrap();
    for subcommand in COMMANDS_THAT_READ_NO_CARD {
        let output = run_binary(&root, &[subcommand, NETWORK, "--cards-dir", "/nowhere"]).unwrap();
        assert_eq!(output.status.code(), Some(USAGE_ERROR_CODE), "{subcommand}");
        assert!(
            stderr_text(&output).contains("unexpected argument '--cards-dir'"),
            "{subcommand}: {}",
            stderr_text(&output)
        );
    }
}

const CARD_FILE_NAME: &str = "ana-card.yaml";
const REFUSED_CARD_EXIT_CODE: i32 = 1;
const NESTING_DEPTHS: [usize; 3] = [3000, 10_000, 100_000];
/// The block mapping stops at 10000 levels: its indentation grows quadratically, about 5 GB of text at 100000.
const BLOCK_MAPPING_DEPTHS: [usize; 2] = [3000, 10_000];

/// A scratch root named for the test holding the card `ana-card` the binary created, its file extended by the YAML text.
fn root_with_card_extended_by(name: &str, extra_yaml: &str) -> io::Result<PathBuf> {
    let root = common::scratch_root(SCRATCH_PREFIX, name)?;
    let created = run_binary(&root, &["create-card", NETWORK, "ana-card", "ana"])?;
    require_success(&created)?;
    let card_file = root.join("data/estafette/cards/acme").join(CARD_FILE_NAME);
    let mut text = std::fs::read_to_string(&card_file)?;
    text.push_str(extra_yaml);
    std::fs::write(&card_file, text)?;
    Ok(root)
}

type DeepPublicShape = (&'static str, fn(usize) -> String, &'static [usize]);

fn block_mapping_public(depth: usize) -> String {
    let mut text = String::from("public:\n");
    for level in 1..=depth {
        text.push_str(&" ".repeat(level));
        text.push_str("a:\n");
    }
    text.push_str(&" ".repeat(depth.saturating_add(1)));
    text.push_str("1\n");
    text
}

fn block_sequence_public(depth: usize) -> String {
    format!("public:\n{}1\n", "- ".repeat(depth))
}

fn flow_mapping_public(depth: usize) -> String {
    format!("public: {}1{}\n", "{a: ".repeat(depth), "}".repeat(depth))
}

fn flow_sequence_public(depth: usize) -> String {
    format!("public: {}1{}\n", "[".repeat(depth), "]".repeat(depth))
}

const DEEP_PUBLIC_SHAPES: [DeepPublicShape; 4] = [
    ("block mapping", block_mapping_public, &BLOCK_MAPPING_DEPTHS),
    ("block sequence", block_sequence_public, &NESTING_DEPTHS),
    ("flow mapping", flow_mapping_public, &NESTING_DEPTHS),
    ("flow sequence", flow_sequence_public, &NESTING_DEPTHS),
];

#[test]
fn binary_list_cards_refuses_a_public_nested_too_deep_for_the_parser_as_an_invalid_card_of_spec_3_5()
 {
    for (shape, public_at, depths) in DEEP_PUBLIC_SHAPES {
        for &depth in depths {
            let root = root_with_card_extended_by(
                &format!("deep_{}_{depth}", shape.replace(' ', "_")),
                &public_at(depth),
            )
            .unwrap();

            let listed = run_binary(&root, &["list-cards", NETWORK]).unwrap();

            let refusal = stderr_text(&listed);
            assert_eq!(
                listed.status.code(),
                Some(REFUSED_CARD_EXIT_CODE),
                "{shape} {depth}: {refusal}"
            );
            assert!(
                refusal.contains("is not a valid card"),
                "{shape} {depth}: {refusal}"
            );
        }
    }
}

/// The start of the doctor's line for a cards check that fails (spec 3.13).
const CARDS_CHECK_FAILED: &str = "cards\tfail\t";

#[test]
fn binary_doctor_fails_the_cards_check_for_a_public_nested_too_deep_for_the_parser_of_spec_3_5() {
    let root = root_with_card_extended_by("doctor_deep", &block_sequence_public(NESTING_DEPTHS[0]))
        .unwrap();
    let network = run_binary(&root, &["network", "create", NETWORK]).unwrap();
    require_success(&network).unwrap();

    let diagnosed = run_binary(&root, &["doctor", NETWORK]).unwrap();

    let report = String::from_utf8(diagnosed.stdout).unwrap();
    assert!(report.contains(CARDS_CHECK_FAILED), "{report}");
    assert!(report.contains(CARD_FILE_NAME), "{report}");
}

const MCP_COMMAND: &str = "mcp";
/// The parent of the runtime directory the MCP server needs, short enough for its inbox socket's path to fit a socket address.
const SHORT_RUNTIME_PARENT: &str = "/tmp";
const SHORT_RUNTIME_NAME: &str = "ebc-mcp-runtime";
const MCP_ANSWERS_WITHIN: Duration = Duration::from_secs(20);
const MCP_INITIALIZE_ID: u64 = 1;
const MCP_INIT_TOOL_CALL_ID: u64 = 2;
const MCP_INIT_TOOL: &str = "init";
const CARD_INVALID_CODE: &str = "card_invalid";

/// A directory removed when this value drops, so a failing assertion leaves nothing under the temp directory, a removal that fails failing the test unless it is already failing.
struct RemovedOnDrop {
    path: PathBuf,
}

impl Drop for RemovedOnDrop {
    fn drop(&mut self) {
        if let Err(failure) = remove_scratch::remove_scratch(&self.path) {
            let report = format!("directory not removed: {failure}");
            assert!(std::thread::panicking(), "{report}");
            drop(writeln!(io::stderr(), "{report}"));
        }
    }
}

/// A child process killed and reaped when this value drops, whether the test passed or failed.
struct KilledOnDrop(Child);

impl Drop for KilledOnDrop {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

/// The answer of the MCP server of the network to the init tool call naming the card, its inbox socket in the short runtime directory, read within `MCP_ANSWERS_WITHIN`, failing with the condition's name when the server ends or stays silent first.
fn mcp_answer_to_init_of(root: &Path, short_runtime: &Path, card: &str) -> io::Result<Value> {
    let mut server = KilledOnDrop(
        command_in_scratch_environment(BINARY, root)
            .env("XDG_RUNTIME_DIR", short_runtime)
            .args([MCP_COMMAND, NETWORK])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let mut requests = server
        .0
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("the server's standard input is not piped"))?;
    let answers = BufReader::new(
        server
            .0
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("the server's standard output is not piped"))?,
    );
    let (answered, answer_lines) = mpsc::channel();
    std::thread::spawn(move || {
        for line in answers.lines() {
            if answered.send(line).is_err() {
                break;
            }
        }
    });
    for request in [
        json!({"jsonrpc": "2.0", "id": MCP_INITIALIZE_ID, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "probe", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": MCP_INIT_TOOL_CALL_ID, "method": "tools/call", "params": {"name": MCP_INIT_TOOL, "arguments": {"card": card}}}),
    ] {
        writeln!(requests, "{request}")?;
    }
    loop {
        let line = answer_lines
            .recv_timeout(MCP_ANSWERS_WITHIN)
            .map_err(|failure| {
                io::Error::other(format!(
                    "an answer to the init call within {MCP_ANSWERS_WITHIN:?}, the server ending or staying silent first: {failure}"
                ))
            })??;
        let answer: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
        if answer.get("id").and_then(Value::as_u64) == Some(MCP_INIT_TOOL_CALL_ID) {
            return Ok(answer);
        }
    }
}

#[test]
fn binary_mcp_init_refuses_a_public_nested_too_deep_for_the_parser_as_card_invalid_of_spec_3_5() {
    let root =
        root_with_card_extended_by("mcp_deep", &block_mapping_public(BLOCK_MAPPING_DEPTHS[1]))
            .unwrap();
    let network = run_binary(&root, &["network", "create", NETWORK]).unwrap();
    require_success(&network).unwrap();
    let short_runtime = RemovedOnDrop {
        path: common::fresh_directory::fresh_directory_in(
            Path::new(SHORT_RUNTIME_PARENT),
            SHORT_RUNTIME_NAME,
        )
        .unwrap(),
    };

    let answer = mcp_answer_to_init_of(&root, &short_runtime.path, "ana-card").unwrap();

    assert_eq!(
        answer["result"]["structuredContent"]["error"]["code"], CARD_INVALID_CODE,
        "{answer}"
    );
}

const OPERATOR_CARD_FILE_NAME: &str = "operator.yaml";

#[test]
fn binary_test_send_refuses_an_operator_card_whose_public_nests_too_deep_for_the_parser_as_an_invalid_card_of_spec_3_5()
 {
    let root =
        root_with_card_extended_by("test_send_deep", &block_sequence_public(NESTING_DEPTHS[2]))
            .unwrap();
    let network = run_binary(&root, &["network", "create", NETWORK]).unwrap();
    require_success(&network).unwrap();
    let cards_directory = root.join("data/estafette/cards/acme");
    std::fs::copy(
        cards_directory.join(CARD_FILE_NAME),
        cards_directory.join(OPERATOR_CARD_FILE_NAME),
    )
    .unwrap();

    let sent = run_binary(&root, &["test-send", NETWORK, "ana", "hello"]).unwrap();

    let refusal = stderr_text(&sent);
    assert_eq!(
        sent.status.code(),
        Some(REFUSED_CARD_EXIT_CODE),
        "{refusal}"
    );
    assert!(
        refusal.contains(&format!("{OPERATOR_CARD_FILE_NAME} is not a valid card")),
        "{refusal}"
    );
}

const DEEPEST_IGNORED_KEY_DEPTH: usize = 2000;
const IGNORED_KEY_DEPTHS: [usize; 2] = [300, DEEPEST_IGNORED_KEY_DEPTH];

#[test]
fn binary_list_cards_lists_a_card_whose_ignored_key_nests_beyond_the_parser_bound_of_spec_3_5() {
    for depth in IGNORED_KEY_DEPTHS {
        let root = root_with_card_extended_by(
            &format!("ignored_{depth}"),
            &format!("ignored:\n{}1\n", "- ".repeat(depth)),
        )
        .unwrap();

        let listed = run_binary(&root, &["list-cards", NETWORK]).unwrap();

        assert_eq!(
            listed.status.code(),
            Some(0),
            "depth {depth}: {}",
            stderr_text(&listed)
        );
        assert!(
            String::from_utf8_lossy(&listed.stdout).starts_with("ana-card\tana\t"),
            "depth {depth}"
        );
    }
}

#[test]
fn binary_list_cards_lists_a_card_whose_public_aliases_a_container_anchored_beyond_the_parser_bound_of_spec_3_5()
 {
    let root = root_with_card_extended_by(
        "alias_beyond_bound",
        &format!(
            "ignored:\n{}&anchor [7]\npublic:\n  seen: *anchor\n",
            "- ".repeat(DEEPEST_IGNORED_KEY_DEPTH)
        ),
    )
    .unwrap();

    let listed = run_binary(&root, &["list-cards", NETWORK]).unwrap();

    assert_eq!(listed.status.code(), Some(0), "{}", stderr_text(&listed));
    assert!(String::from_utf8_lossy(&listed.stdout).starts_with("ana-card\tana\t"));
}

const GROUP_OTHER_READABLE_DIRECTORY_MODE: u32 = 0o755;

fn runtime_directory_of(root: &Path) -> io::Result<PathBuf> {
    let user_id = std::fs::metadata(root)?.uid();
    Ok(root.join(format!("runtime/estafette-{user_id}")))
}

#[test]
fn binary_refuses_an_existing_runtime_directory_that_grants_access_to_group_and_other_runtime_directory_is_private_or_refused()
 {
    let root = common::scratch_root(SCRATCH_PREFIX, "runtime_directory_0755").unwrap();
    let runtime = runtime_directory_of(&root).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::set_permissions(
        &runtime,
        Permissions::from_mode(GROUP_OTHER_READABLE_DIRECTORY_MODE),
    )
    .unwrap();

    let listed = run_binary(&root, &["list-cards", NETWORK]).unwrap();

    assert_eq!(listed.status.code(), Some(1), "{}", stderr_text(&listed));
    assert_eq!(listed.stdout, Vec::<u8>::new());
    let path = runtime.display();
    assert_eq!(
        stderr_text(&listed),
        format!(
            "cannot create {path}: {path} already exists and is mode 0755, which grants access to group or other; it must be a directory of yours with no access for group or other\n"
        )
    );
    assert_eq!(
        mode_of(&runtime).unwrap(),
        GROUP_OTHER_READABLE_DIRECTORY_MODE
    );
}

#[test]
fn binary_refuses_a_symbolic_link_in_place_of_the_runtime_directory_runtime_directory_is_private_or_refused()
 {
    let root = common::scratch_root(SCRATCH_PREFIX, "runtime_directory_link").unwrap();
    let runtime = runtime_directory_of(&root).unwrap();
    let target = root.join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::set_permissions(&target, Permissions::from_mode(PRIVATE_DIRECTORY_MODE)).unwrap();
    std::fs::create_dir_all(runtime.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &runtime).unwrap();

    let listed = run_binary(&root, &["list-cards", NETWORK]).unwrap();

    assert_eq!(listed.status.code(), Some(1), "{}", stderr_text(&listed));
    assert_eq!(listed.stdout, Vec::<u8>::new());
    let path = runtime.display();
    assert_eq!(
        stderr_text(&listed),
        format!(
            "cannot create {path}: {path} already exists and is a symbolic link; it must be a directory of yours with no access for group or other\n"
        )
    );
}

const READ_SEARCH_ONLY_DIRECTORY_MODE: u32 = 0o500;
/// The descriptor limit that leaves the three standard streams open and one descriptor free, where a pipe needs two.
const NO_ROOM_FOR_A_PIPE: &str = "4";
/// Closes the descriptors a test runner may leave open, such as the heavy gate's lock, which a process under so low a limit aborts on.
const CLOSE_INHERITED_DESCRIPTORS: &str = "exec 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-";

#[test]
fn binary_refuses_to_start_when_it_cannot_make_a_pipe_to_learn_its_user_runtime_directory_is_private_or_refused()
 {
    let root = common::scratch_root(SCRATCH_PREFIX, "no_descriptor_for_a_pipe").unwrap();

    let listed = command_in_scratch_environment("sh", &root)
        .args([
            "-c",
            &format!(
                "{CLOSE_INHERITED_DESCRIPTORS}; ulimit -n {NO_ROOM_FOR_A_PIPE}; exec \"$0\" \"$@\""
            ),
            BINARY,
            "list-cards",
            NETWORK,
        ])
        .output()
        .unwrap();

    assert_eq!(listed.status.code(), Some(1), "{}", stderr_text(&listed));
    assert_eq!(listed.stdout, Vec::<u8>::new());
    assert_eq!(
        stderr_text(&listed),
        "cannot learn the user this process runs as: it could not make a pipe: Too many open files (os error 24)\n"
    );
}

fn run_binary_with_temporary_directory(
    root: &Path,
    temporary_directory: &Path,
    arguments: &[&str],
) -> io::Result<Output> {
    command_in_scratch_environment(BINARY, root)
        .args(arguments)
        .env("TMPDIR", temporary_directory)
        .output()
}

#[test]
fn binary_starts_over_existing_private_directories_when_the_temporary_directory_is_missing_runtime_directory_is_private_or_refused()
 {
    let root = common::scratch_root(SCRATCH_PREFIX, "temporary_directory_missing").unwrap();
    let first = run_binary(&root, &["list-cards", NETWORK]).unwrap();
    assert_eq!(first.status.code(), Some(0), "{}", stderr_text(&first));

    let listed = run_binary_with_temporary_directory(
        &root,
        &root.join("no-such-temporary-directory"),
        &["list-cards", NETWORK],
    )
    .unwrap();

    assert_eq!(listed.status.code(), Some(0), "{}", stderr_text(&listed));
    assert_eq!(stderr_text(&listed), "");
}

#[test]
fn binary_starts_over_existing_private_directories_when_the_temporary_directory_is_not_writable_runtime_directory_is_private_or_refused()
 {
    let root = common::scratch_root(SCRATCH_PREFIX, "temporary_directory_0500").unwrap();
    let first = run_binary(&root, &["list-cards", NETWORK]).unwrap();
    assert_eq!(first.status.code(), Some(0), "{}", stderr_text(&first));
    let temporary_directory = root.join("read-only-temporary-directory");
    std::fs::create_dir(&temporary_directory).unwrap();
    std::fs::set_permissions(
        &temporary_directory,
        Permissions::from_mode(READ_SEARCH_ONLY_DIRECTORY_MODE),
    )
    .unwrap();

    let listed =
        run_binary_with_temporary_directory(&root, &temporary_directory, &["list-cards", NETWORK])
            .unwrap();

    assert_eq!(listed.status.code(), Some(0), "{}", stderr_text(&listed));
    assert_eq!(stderr_text(&listed), "");
}
