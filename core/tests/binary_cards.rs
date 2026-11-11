use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

mod common;

const SCRATCH_PREFIX: &str = "estafette-binary-cards";
const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
const USAGE_ERROR_CODE: i32 = 2;
const PUBLIC_KEY_HEX_LINE_LENGTH: usize = 65;
const PERMISSION_BITS: u32 = 0o777;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const CARD_FILE_MODE: u32 = 0o644;
const KEY_FILE_MODE: u32 = 0o600;
const UMASKS: [&str; 2] = ["0400", "0000"];

fn run_binary_under_umask(root: &Path, umask: &str, arguments: &[&str]) -> io::Result<Output> {
    Command::new("sh")
        .args(["-c", &format!("umask {umask}; exec \"$0\" \"$@\""), BINARY])
        .args(arguments)
        .env_clear()
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .output()
}

fn mode_of(path: &Path) -> io::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & PERMISSION_BITS)
}

fn run_binary(root: &std::path::Path, arguments: &[&str]) -> io::Result<Output> {
    Command::new(BINARY)
        .args(arguments)
        .env_clear()
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .output()
}

#[test]
fn binary_create_card_then_list_cards_round_trips_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "round_trip").unwrap();
    let created = run_binary(&root, &["create-card", "acme", "ana-card", "ana"]).unwrap();
    assert_eq!(created.status.code(), Some(0));
    let printed_key = String::from_utf8(created.stdout).unwrap();
    assert_eq!(printed_key.len(), PUBLIC_KEY_HEX_LINE_LENGTH);
    assert_eq!(created.stderr, Vec::<u8>::new());

    let listed = run_binary(&root, &["list-cards", "acme"]).unwrap();
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(listed.stdout).unwrap(),
        format!("ana-card\tana\t{printed_key}")
    );
}

#[test]
fn binary_create_card_twice_exits_one_with_the_message_alone_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "twice").unwrap();
    run_binary(&root, &["create-card", "acme", "ana-card", "ana"]).unwrap();
    let second = run_binary(&root, &["create-card", "acme", "ana-card", "bob"]).unwrap();
    assert_eq!(second.status.code(), Some(1));
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
            "acme",
            "ana-card",
            "ana",
            "--cards-dir",
            flagged.to_str().unwrap(),
        ],
    )
    .unwrap();
    assert_eq!(created.status.code(), Some(0));
    assert!(flagged.join("acme/ana-card.yaml").is_file());
}

#[test]
fn binary_usage_error_exits_two_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "usage").unwrap();
    let missing_arguments = run_binary(&root, &["create-card", "acme"]).unwrap();
    assert_eq!(missing_arguments.status.code(), Some(USAGE_ERROR_CODE));
    assert_eq!(missing_arguments.stdout, Vec::<u8>::new());
    let unknown_command = run_binary(&root, &["no-such-command"]).unwrap();
    assert_eq!(unknown_command.status.code(), Some(USAGE_ERROR_CODE));
}

#[test]
fn binary_names_outside_the_handle_grammar_are_usage_errors_before_any_write_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "bad_names").unwrap();
    let refused_arguments: [&[&str]; 5] = [
        &["create-card", "acme", "Ana", "ana"],
        &["create-card", "acme", "../up", "ana"],
        &["create-card", "acme", "ana", "Ana"],
        &["create-card", "Net", "ana", "ana"],
        &["list-cards", "Net"],
    ];
    for arguments in refused_arguments {
        let refused = run_binary(&root, arguments).unwrap();
        assert_eq!(
            refused.status.code(),
            Some(USAGE_ERROR_CODE),
            "{arguments:?}"
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
    let created = run_binary(&root, &["create-card", "acme", "ana-card", "ana"]).unwrap();
    assert_eq!(created.status.code(), Some(0));
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
    let listed = run_binary(&root, &["list-cards", "acme"]).unwrap();
    assert_eq!(listed.status.code(), Some(0));
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
    let created =
        run_binary_under_umask(&root, "077", &["create-card", "acme", "ana-card", "ana"]).unwrap();
    assert_eq!(created.status.code(), Some(0));
    let cards = root.join("data/estafette/cards/acme");
    assert_eq!(
        mode_of(&cards.join("ana-card.yaml")).unwrap(),
        CARD_FILE_MODE
    );
    assert_eq!(mode_of(&cards.join("ana-card.key")).unwrap(), KEY_FILE_MODE);
}

#[test]
fn binary_owner_key_is_owner_read_write_under_any_umask_of_spec_3_3() {
    for umask in UMASKS {
        let root = common::scratch_root(SCRATCH_PREFIX, &format!("key_modes_{umask}")).unwrap();
        let network = run_binary_under_umask(&root, umask, &["network", "create", "acme"]).unwrap();
        assert_eq!(network.status.code(), Some(0), "umask {umask}");
        assert_eq!(
            mode_of(&root.join("config/estafette/networks/acme.owner.key")).unwrap(),
            KEY_FILE_MODE,
            "owner key, umask {umask}"
        );
    }
}

const SEARCH_BLOCKING_UMASK: &str = "0100";

#[test]
fn binary_program_directories_are_owner_only_under_any_umask_of_spec_3_3() {
    for umask in [SEARCH_BLOCKING_UMASK, "0000"] {
        let root = common::scratch_root(SCRATCH_PREFIX, &format!("dir_modes_{umask}")).unwrap();
        for base in ["config", "data", "runtime"] {
            std::fs::create_dir(root.join(base)).unwrap();
        }
        let listed = run_binary_under_umask(&root, umask, &["list-cards", "acme"]).unwrap();
        assert_eq!(listed.status.code(), Some(0), "umask {umask}");
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

const CARDS_DIRECTORY_VARIABLE: &str = "ESTAFETTE_CARDS_DIR";
const CARDS_CHECK_LINE: &str = "cards\tpass\tcards valid: 1";

fn create_network_and_card_in(root: &Path, cards_directory: &str) -> io::Result<bool> {
    let network = run_binary(root, &["network", "create", "acme"])?;
    let card = run_binary(
        root,
        &[
            "create-card",
            "acme",
            "ana",
            "ana",
            "--cards-dir",
            cards_directory,
        ],
    )?;
    Ok(network.status.success() && card.status.success())
}

#[test]
fn binary_doctor_reads_the_cards_of_the_cards_dir_flag_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "doctor_flag").unwrap();
    let flagged = root.join("flagged");
    assert!(create_network_and_card_in(&root, flagged.to_str().unwrap()).unwrap());

    let diagnosed = run_binary(
        &root,
        &["doctor", "acme", "--cards-dir", flagged.to_str().unwrap()],
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
    assert!(create_network_and_card_in(&root, flagged.to_str().unwrap()).unwrap());

    let diagnosed = Command::new(BINARY)
        .args(["doctor", "acme", "--cards-dir", flagged.to_str().unwrap()])
        .env_clear()
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_RUNTIME_DIR", root.join("runtime"))
        .env(CARDS_DIRECTORY_VARIABLE, &cardless)
        .output()
        .unwrap();

    let report = String::from_utf8(diagnosed.stdout).unwrap();
    assert!(report.contains(CARDS_CHECK_LINE), "{report}");
}

#[test]
fn binary_test_send_creates_its_operator_card_in_the_cards_dir_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "test_send_flag").unwrap();
    let flagged = root.join("flagged");
    assert!(create_network_and_card_in(&root, flagged.to_str().unwrap()).unwrap());

    run_binary(
        &root,
        &[
            "test-send",
            "acme",
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

#[test]
fn binary_help_of_every_observer_command_names_the_cards_dir_flag_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "observer_help").unwrap();
    for subcommand in ["show-peers", "tail", "test-send", "doctor"] {
        let output = run_binary(&root, &[subcommand, "--help"]).unwrap();
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--cards-dir"), "{subcommand}: {help}");
    }
}

const CARD_FILE_NAME: &str = "ana-card.yaml";
const REFUSED_CARD_EXIT_CODE: i32 = 1;
const NESTING_DEPTHS: [usize; 3] = [3000, 10_000, 100_000];
const REFUSAL_EXCERPT_CHARACTERS: usize = 300;

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
    ("block mapping", block_mapping_public, &[3000, 10_000]),
    ("block sequence", block_sequence_public, &NESTING_DEPTHS),
    ("flow mapping", flow_mapping_public, &NESTING_DEPTHS),
    ("flow sequence", flow_sequence_public, &NESTING_DEPTHS),
];

#[test]
fn binary_list_cards_refuses_a_public_nested_too_deep_for_the_parser_as_an_invalid_card_of_spec_3_5()
 {
    for (shape, public_at, depths) in DEEP_PUBLIC_SHAPES {
        for &depth in depths {
            let root = common::scratch_root(
                SCRATCH_PREFIX,
                &format!("deep_{}_{depth}", shape.replace(' ', "_")),
            )
            .unwrap();
            let created = run_binary(&root, &["create-card", "acme", "ana-card", "ana"]).unwrap();
            assert_eq!(created.status.code(), Some(0));
            let card_file = root.join("data/estafette/cards/acme").join(CARD_FILE_NAME);
            let mut text = std::fs::read_to_string(&card_file).unwrap();
            text.push_str(&public_at(depth));
            std::fs::write(&card_file, text).unwrap();

            let listed = run_binary(&root, &["list-cards", "acme"]).unwrap();

            let refusal: String = String::from_utf8_lossy(&listed.stderr)
                .chars()
                .take(REFUSAL_EXCERPT_CHARACTERS)
                .collect();
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
