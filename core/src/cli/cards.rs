use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::{Args, Subcommand};
use iroh::SecretKey;

use crate::card::{Card, CardInvalid};
use crate::dirs::{Directories, DirectoriesError, DirectoryFilesystem};
use crate::errors::ExitStatus;
use crate::handle::{Handle, HandleError};
use crate::keys::render_public_key;
use crate::sidecar::{SidecarError, create_card};

const SEED_BYTES: usize = 32;
const CARD_FILE_EXTENSION: &str = "yaml";
const LIST_FIELD_SEPARATOR: char = '\t';
const LINE_END: &str = "\n";

/// The network and the optional cards directory override every card command takes.
#[derive(Debug, Args)]
pub struct RegistryArgs {
    pub network: String,
    #[arg(long)]
    pub cards_dir: Option<PathBuf>,
}

/// The card commands of spec 3.13.
#[derive(Debug, Subcommand)]
pub enum CardCommand {
    CreateCard {
        #[command(flatten)]
        registry: RegistryArgs,
        card: String,
        handle: String,
    },
    ListCards {
        #[command(flatten)]
        registry: RegistryArgs,
    },
}

/// Why a card command failed, printed as its message alone.
#[derive(Debug)]
enum CardCommandError {
    NameRefused {
        role: &'static str,
        source: HandleError,
    },
    Directories(DirectoriesError),
    Sidecar(SidecarError),
    CardsDirectoryUncreatable {
        path: PathBuf,
        source: io::Error,
    },
    Unreadable {
        path: PathBuf,
        source: io::Error,
    },
    CardInvalid {
        path: PathBuf,
        reason: CardInvalid,
    },
    OutputUnwritable(io::Error),
}

impl fmt::Display for CardCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameRefused { role, source } => {
                write!(formatter, "the {role} is refused: {source}")
            }
            Self::Directories(error) => error.fmt(formatter),
            Self::Sidecar(error) => error.fmt(formatter),
            Self::CardsDirectoryUncreatable { path, source } => {
                write!(formatter, "cannot create {}: {source}", path.display())
            }
            Self::Unreadable { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::CardInvalid { path, reason } => {
                write!(
                    formatter,
                    "{} is not a valid card: {reason}",
                    path.display()
                )
            }
            Self::OutputUnwritable(source) => {
                write!(formatter, "cannot write the result: {source}")
            }
        }
    }
}

impl std::error::Error for CardCommandError {}

/// Runs a card command: the result goes to stdout, the failure message alone to stderr.
pub fn run(
    command: CardCommand,
    environment: impl Fn(&str) -> Option<OsString>,
    filesystem: &impl DirectoryFilesystem,
    fill_seed: impl FnOnce(&mut [u8; SEED_BYTES]),
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> ExitStatus {
    match execute(command, environment, filesystem, fill_seed, stdout) {
        Ok(()) => ExitStatus::Success,
        Err(failure) => {
            drop(writeln!(stderr, "{failure}"));
            ExitStatus::Failure
        }
    }
}

fn execute(
    command: CardCommand,
    environment: impl Fn(&str) -> Option<OsString>,
    filesystem: &impl DirectoryFilesystem,
    fill_seed: impl FnOnce(&mut [u8; SEED_BYTES]),
    stdout: &mut impl Write,
) -> Result<(), CardCommandError> {
    let result = match command {
        CardCommand::CreateCard {
            registry,
            card,
            handle,
        } => {
            require_handle_grammar("network name", &registry.network)?;
            require_handle_grammar("card name", &card)?;
            require_handle_grammar("handle", &handle)?;
            let directories = establish_directories(environment, filesystem, registry.cards_dir)?;
            create_card_with_fresh_key(
                &directories,
                filesystem,
                &registry.network,
                &card,
                &handle,
                fill_seed,
            )?
        }
        CardCommand::ListCards { registry } => {
            require_handle_grammar("network name", &registry.network)?;
            let directories = establish_directories(environment, filesystem, registry.cards_dir)?;
            list_registry_cards(&directories, &registry.network)?
        }
    };
    stdout
        .write_all(result.as_bytes())
        .map_err(CardCommandError::OutputUnwritable)
}

fn require_handle_grammar(role: &'static str, name: &str) -> Result<(), CardCommandError> {
    Handle::from_str(name)
        .map(drop)
        .map_err(|source| CardCommandError::NameRefused { role, source })
}

fn establish_directories(
    environment: impl Fn(&str) -> Option<OsString>,
    filesystem: &impl DirectoryFilesystem,
    cards_dir: Option<PathBuf>,
) -> Result<Directories, CardCommandError> {
    Directories::establish(environment, filesystem, cards_dir)
        .map_err(CardCommandError::Directories)
}

fn create_card_with_fresh_key(
    directories: &Directories,
    filesystem: &impl DirectoryFilesystem,
    network: &str,
    card: &str,
    handle: &str,
    fill_seed: impl FnOnce(&mut [u8; SEED_BYTES]),
) -> Result<String, CardCommandError> {
    let cards_directory = directories
        .cards_directory(network)
        .map_err(CardCommandError::Directories)?;
    filesystem
        .create_private_directory(&cards_directory)
        .map_err(|source| CardCommandError::CardsDirectoryUncreatable {
            path: cards_directory,
            source,
        })?;
    let mut seed = [0; SEED_BYTES];
    fill_seed(&mut seed);
    create_card(
        &directories
            .card_file(network, card)
            .map_err(CardCommandError::Directories)?,
        &directories
            .card_sidecar_file(network, card)
            .map_err(CardCommandError::Directories)?,
        handle,
        seed,
    )
    .map_err(CardCommandError::Sidecar)?;
    Ok(format!(
        "{}\n",
        render_public_key(&SecretKey::from_bytes(&seed).public())
    ))
}

fn list_registry_cards(
    directories: &Directories,
    network: &str,
) -> Result<String, CardCommandError> {
    let cards_directory = directories
        .cards_directory(network)
        .map_err(CardCommandError::Directories)?;
    let mut lines_by_card_name = BTreeMap::new();
    for card_file in card_files_in(&cards_directory)? {
        if let Some((name, line)) = describe_card_file(&card_file)? {
            lines_by_card_name.insert(name, line);
        }
    }
    Ok(lines_by_card_name
        .into_values()
        .map(|line| line + LINE_END)
        .collect())
}

fn card_files_in(cards_directory: &Path) -> Result<Vec<PathBuf>, CardCommandError> {
    let unreadable = |source| CardCommandError::Unreadable {
        path: cards_directory.to_owned(),
        source,
    };
    let entries = match fs::read_dir(cards_directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(unreadable(source)),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry.map_err(unreadable)?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == CARD_FILE_EXTENSION)
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn describe_card_file(card_file: &Path) -> Result<Option<(String, String)>, CardCommandError> {
    let Some(name) = card_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| Handle::from_str(stem).is_ok())
    else {
        return Ok(None);
    };
    let text = fs::read_to_string(card_file).map_err(|source| CardCommandError::Unreadable {
        path: card_file.to_owned(),
        source,
    })?;
    let card = Card::from_str(&text).map_err(|reason| CardCommandError::CardInvalid {
        path: card_file.to_owned(),
        reason,
    })?;
    let line = [name, &card.handle, &card.public_key].join(&LIST_FIELD_SEPARATOR.to_string());
    Ok(Some((name.to_owned(), line)))
}
