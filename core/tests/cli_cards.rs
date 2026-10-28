use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use estafette::cli::cards::{CardCommand, CardRegistryReader, RegistryArgs, run};
use estafette::dirs::{DirectoryFilesystem, OperatingSystemFilesystem};
use estafette::errors::ExitStatus;
use iroh::SecretKey;

mod common;

const SCRATCH_PREFIX: &str = "estafette-cli-cards";
const NETWORK: &str = "acme";

struct Outcome {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn environment_under(root: &Path, cards_dir: Option<&Path>) -> impl Fn(&str) -> Option<OsString> {
    let root = root.to_owned();
    let cards_dir = cards_dir.map(|path| path.as_os_str().to_owned());
    move |name| match name {
        "XDG_CONFIG_HOME" => Some(root.join("config").into_os_string()),
        "XDG_DATA_HOME" => Some(root.join("data").into_os_string()),
        "XDG_RUNTIME_DIR" => Some(root.join("runtime").into_os_string()),
        "ESTAFETTE_CARDS_DIR" => cards_dir.clone(),
        _ => None,
    }
}

fn execute(root: &Path, command: CardCommand, seed_byte: u8) -> Outcome {
    execute_with_environment(environment_under(root, None), command, seed_byte)
}

fn execute_with_environment(
    environment: impl Fn(&str) -> Option<OsString>,
    command: CardCommand,
    seed_byte: u8,
) -> Outcome {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        command,
        environment,
        &OperatingSystemFilesystem,
        |seed| seed.fill(seed_byte),
        &mut stdout,
        &mut stderr,
    );
    Outcome {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

fn create_card(card: &str, handle: &str, cards_dir: Option<PathBuf>) -> CardCommand {
    CardCommand::CreateCard {
        registry: RegistryArgs {
            network: NETWORK.to_owned(),
            cards_dir,
        },
        card: card.to_owned(),
        handle: handle.to_owned(),
    }
}

fn list_cards(cards_dir: Option<PathBuf>) -> CardCommand {
    CardCommand::ListCards {
        registry: RegistryArgs {
            network: NETWORK.to_owned(),
            cards_dir,
        },
    }
}

fn public_key_of(seed_byte: u8) -> String {
    data_encoding::HEXLOWER.encode(SecretKey::from_bytes(&[seed_byte; 32]).public().as_bytes())
}

struct CannedRegistry {
    card_file: PathBuf,
    card_text: String,
}

impl DirectoryFilesystem for CannedRegistry {
    fn create_private_directory(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }

    fn owner_user_id(&self, _path: &Path) -> io::Result<u32> {
        Ok(0)
    }
}

impl CardRegistryReader for CannedRegistry {
    fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        if Some(path) == self.card_file.parent() {
            Ok(vec![self.card_file.clone()])
        } else {
            Err(io::ErrorKind::NotFound.into())
        }
    }

    fn read_text(&self, path: &Path) -> io::Result<String> {
        if path == self.card_file {
            Ok(self.card_text.clone())
        } else {
            Err(io::ErrorKind::NotFound.into())
        }
    }
}

#[test]
fn list_cards_reads_only_through_the_handed_in_file_system() {
    let cards_dir = PathBuf::from("/nowhere-on-disk/cards");
    let registry = CannedRegistry {
        card_file: cards_dir.join(NETWORK).join("ana.yaml"),
        card_text: format!("handle: ana\npublic_key: {}\n", public_key_of(21)),
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        list_cards(Some(cards_dir)),
        environment_under(Path::new("/nowhere-on-disk/base"), None),
        &registry,
        |_seed| {},
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(status, ExitStatus::Success);
    assert_eq!(
        String::from_utf8_lossy(&stdout),
        format!("ana\tana\t{}\n", public_key_of(21))
    );
    assert_eq!(stderr, Vec::<u8>::new());
}

#[test]
fn create_card_then_list_cards_shows_name_handle_and_public_key_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "create_then_list").unwrap();
    let created = execute(&root, create_card("ana-card", "ana", None), 7);
    assert_eq!(created.status, ExitStatus::Success);
    assert_eq!(created.stdout, format!("{}\n", public_key_of(7)));
    assert_eq!(created.stderr, "");

    let listed = execute(&root, list_cards(None), 0);
    assert_eq!(listed.status, ExitStatus::Success);
    assert_eq!(
        listed.stdout,
        format!("ana-card\tana\t{}\n", public_key_of(7))
    );
    assert_eq!(listed.stderr, "");
}

#[test]
fn create_card_twice_refuses_and_keeps_the_first_card_of_spec_3_5() {
    let root = common::scratch_root(SCRATCH_PREFIX, "create_twice").unwrap();
    execute(&root, create_card("ana-card", "ana", None), 7);

    let second = execute(&root, create_card("ana-card", "bob", None), 9);
    assert_eq!(second.status, ExitStatus::Failure);
    assert_eq!(second.stdout, "");
    assert_eq!(
        second.stderr,
        "the card or its sidecar already exists; create never overwrites\n"
    );

    let listed = execute(&root, list_cards(None), 0);
    assert_eq!(
        listed.stdout,
        format!("ana-card\tana\t{}\n", public_key_of(7))
    );
}

#[test]
fn create_card_honors_the_cards_dir_flag_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "flag_override").unwrap();
    let flagged = root.join("flagged");
    let created = execute(
        &root,
        create_card("ana-card", "ana", Some(flagged.clone())),
        7,
    );
    assert_eq!(created.status, ExitStatus::Success);
    assert!(flagged.join(NETWORK).join("ana-card.yaml").is_file());
    assert!(flagged.join(NETWORK).join("ana-card.key").is_file());
    assert!(!root.join("data/estafette/cards").exists());

    let listed = execute(&root, list_cards(Some(flagged)), 0);
    assert_eq!(
        listed.stdout,
        format!("ana-card\tana\t{}\n", public_key_of(7))
    );
    assert_eq!(execute(&root, list_cards(None), 0).stdout, "");
}

#[test]
fn list_cards_honors_the_cards_dir_variable_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "variable_override").unwrap();
    let from_variable = root.join("from-variable");
    let created = execute_with_environment(
        environment_under(&root, Some(&from_variable)),
        create_card("ana-card", "ana", None),
        7,
    );
    assert_eq!(created.status, ExitStatus::Success);
    assert!(from_variable.join(NETWORK).join("ana-card.yaml").is_file());

    let listed = execute_with_environment(
        environment_under(&root, Some(&from_variable)),
        list_cards(None),
        0,
    );
    assert_eq!(
        listed.stdout,
        format!("ana-card\tana\t{}\n", public_key_of(7))
    );
}

#[test]
fn create_card_allows_the_reserved_operator_name_of_spec_3_13() {
    let root = common::scratch_root(SCRATCH_PREFIX, "operator_card").unwrap();
    let created = execute(&root, create_card("operator", "operator", None), 3);
    assert_eq!(created.status, ExitStatus::Success);
    assert_eq!(
        execute(&root, list_cards(None), 0).stdout,
        format!("operator\toperator\t{}\n", public_key_of(3))
    );
}

#[test]
fn list_cards_prints_one_line_per_card_in_card_name_order() {
    let root = common::scratch_root(SCRATCH_PREFIX, "list_order").unwrap();
    for (index, (card, handle)) in [("zed", "zed"), ("ana", "ana-one"), ("mid", "mid")]
        .into_iter()
        .enumerate()
    {
        let seed_byte = u8::try_from(index + 10).unwrap();
        assert_eq!(
            execute(&root, create_card(card, handle, None), seed_byte).status,
            ExitStatus::Success
        );
    }
    assert_eq!(
        execute(&root, list_cards(None), 0).stdout,
        format!(
            "ana\tana-one\t{}\nmid\tmid\t{}\nzed\tzed\t{}\n",
            public_key_of(11),
            public_key_of(12),
            public_key_of(10)
        )
    );
}

#[test]
fn create_card_refuses_names_outside_the_handle_grammar_before_writing_anything_of_spec_3_3() {
    let root = common::scratch_root(SCRATCH_PREFIX, "bad_names").unwrap();
    let refused_commands = [
        (
            create_card("Ana", "ana", None),
            "the card name is refused: ",
        ),
        (
            create_card("../up", "ana", None),
            "the card name is refused: ",
        ),
        (create_card("ana", "Ana", None), "the handle is refused: "),
        (
            CardCommand::CreateCard {
                registry: RegistryArgs {
                    network: "Net".to_owned(),
                    cards_dir: None,
                },
                card: "ana".to_owned(),
                handle: "ana".to_owned(),
            },
            "the network name is refused: ",
        ),
    ];
    for (command, expected_prefix) in refused_commands {
        let outcome = execute(&root, command, 5);
        assert_eq!(outcome.status, ExitStatus::Failure);
        assert_eq!(outcome.stdout, "");
        assert!(
            outcome.stderr.starts_with(expected_prefix),
            "{}",
            outcome.stderr
        );
    }
    assert!(!root.join("data").exists());
}

#[test]
fn list_cards_names_the_invalid_card_file_and_fails() {
    let root = common::scratch_root(SCRATCH_PREFIX, "invalid_card").unwrap();
    execute(&root, create_card("ana", "ana", None), 7);
    let broken = root
        .join("data/estafette/cards")
        .join(NETWORK)
        .join("bad.yaml");
    fs::write(&broken, "handle: bad\n").unwrap();

    let listed = execute(&root, list_cards(None), 0);
    assert_eq!(listed.status, ExitStatus::Failure);
    assert_eq!(listed.stdout, "");
    assert_eq!(
        listed.stderr,
        format!(
            "{} is not a valid card: card has no `public_key`\n",
            broken.display()
        )
    );
}
