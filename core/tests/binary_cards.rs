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
