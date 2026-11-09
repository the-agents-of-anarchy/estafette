use std::io;
use std::process::{Command, Output};

mod common;

const SCRATCH_PREFIX: &str = "estafette-binary-cards";
const BINARY: &str = env!("CARGO_BIN_EXE_estafette");
const USAGE_ERROR_CODE: i32 = 2;
const PUBLIC_KEY_HEX_LINE_LENGTH: usize = 65;

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
                "Produces: one line per card",
                "creates no directory and no file",
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
    use std::os::unix::fs::PermissionsExt;
    let root = common::scratch_root(SCRATCH_PREFIX, "created").unwrap();
    let created = run_binary(&root, &["create-card", "acme", "ana-card", "ana"]).unwrap();
    assert_eq!(created.status.code(), Some(0));
    let mode_of =
        |path: std::path::PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode_of(root.join("config/estafette")), 0o700);
    assert_eq!(mode_of(root.join("data/estafette")), 0o700);
    let runtime_directories: Vec<_> = std::fs::read_dir(root.join("runtime"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(runtime_directories.len(), 1);
    assert_eq!(mode_of(runtime_directories[0].clone()), 0o700);
    let cards = root.join("data/estafette/cards/acme");
    assert_eq!(mode_of(cards.join("ana-card.yaml")), 0o644);
    assert_eq!(mode_of(cards.join("ana-card.key")), 0o600);
}

#[test]
fn binary_list_cards_creates_no_directory_and_no_file_as_its_help_states() {
    let root = common::scratch_root(SCRATCH_PREFIX, "list_creates_nothing").unwrap();
    let listed = run_binary(&root, &["list-cards", "acme"]).unwrap();
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(listed.stdout, Vec::<u8>::new());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
}
