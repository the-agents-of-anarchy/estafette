use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::{Args, Subcommand};

use crate::card::{Card, CardInvalid};
use crate::dirs::{
    CARD_EXTENSION, Directories, DirectoriesError, DirectoryFilesystem, OperatingSystemFilesystem,
};
use crate::errors::ExitStatus;
use crate::handle::{Handle, HandleError};
use crate::keys::render_public_key;
use crate::sidecar::{SEED_BYTES, SidecarError, create_card};

const LIST_FIELD_SEPARATOR: char = '\t';
const LINE_END: &str = "\n";

/// The network and the optional cards directory override every card command takes.
#[derive(Debug, Args)]
pub struct RegistryArgs {
    pub network: Handle,
    #[arg(long)]
    pub cards_dir: Option<PathBuf>,
}

/// The card commands of spec 3.13.
#[derive(Debug, Subcommand)]
pub enum CardCommand {
    CreateCard {
        #[command(flatten)]
        registry: RegistryArgs,
        card: Handle,
        handle: Handle,
    },
    ListCards {
        #[command(flatten)]
        registry: RegistryArgs,
    },
}

/// Why a card command failed, printed as its message alone.
#[derive(Debug)]
enum CardCommandError {
    Directories(DirectoriesError),
    Sidecar(SidecarError),
    CardsDirectoryUncreatable { path: PathBuf, source: io::Error },
    Unreadable { path: PathBuf, source: io::Error },
    CardNameRefused { path: PathBuf, source: HandleError },
    CardInvalid { path: PathBuf, reason: CardInvalid },
    OutputUnwritable(io::Error),
}

impl fmt::Display for CardCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directories(error) => error.fmt(formatter),
            Self::Sidecar(error) => error.fmt(formatter),
            Self::CardsDirectoryUncreatable { path, source } => {
                write!(formatter, "cannot create {}: {source}", path.display())
            }
            Self::Unreadable { path, source } => {
                write!(formatter, "cannot read {}: {source}", path.display())
            }
            Self::CardNameRefused { path, source } => {
                write!(
                    formatter,
                    "{} is not a valid card name: {source}",
                    path.display()
                )
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

/// The reads the card registry listing performs, supplied by the composition root.
pub trait CardRegistryReader {
    /// Returns the paths of the entries of a directory.
    ///
    /// # Errors
    ///
    /// Fails when the directory cannot be read; `NotFound` when it does not exist.
    fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>>;

    /// Returns the text of a file.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read as UTF-8 text.
    fn read_text(&self, path: &Path) -> io::Result<String>;
}

impl CardRegistryReader for OperatingSystemFilesystem {
    fn read_directory(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        fs::read_dir(path)?
            .map(|entry| entry.map(|found| found.path()))
            .collect()
    }

    fn read_text(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }
}

/// Runs a card command: the result goes to stdout, the failure message alone to stderr.
pub fn run(
    command: CardCommand,
    environment: impl Fn(&str) -> Option<OsString>,
    filesystem: &(impl DirectoryFilesystem + CardRegistryReader),
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
    filesystem: &(impl DirectoryFilesystem + CardRegistryReader),
    fill_seed: impl FnOnce(&mut [u8; SEED_BYTES]),
    stdout: &mut impl Write,
) -> Result<(), CardCommandError> {
    let result = match command {
        CardCommand::CreateCard {
            registry,
            card,
            handle,
        } => {
            let directories = establish_directories(environment, filesystem, registry.cards_dir)?;
            create_card_with_fresh_key(
                &directories,
                filesystem,
                registry.network.as_str(),
                card.as_str(),
                handle.as_str(),
                fill_seed,
            )?
        }
        CardCommand::ListCards { registry } => {
            let directories = establish_directories(environment, filesystem, registry.cards_dir)?;
            list_registry_cards(filesystem, &directories, registry.network.as_str())?
        }
    };
    stdout
        .write_all(result.as_bytes())
        .map_err(CardCommandError::OutputUnwritable)
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
    let public_key = create_card(
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
    Ok(render_public_key(&public_key) + LINE_END)
}

fn list_registry_cards(
    filesystem: &impl CardRegistryReader,
    directories: &Directories,
    network: &str,
) -> Result<String, CardCommandError> {
    let cards_directory = directories
        .cards_directory(network)
        .map_err(CardCommandError::Directories)?;
    let mut lines_by_card_name = BTreeMap::new();
    let card_files = card_files_in(filesystem, &cards_directory).map_err(|source| {
        CardCommandError::Unreadable {
            path: cards_directory.clone(),
            source,
        }
    })?;
    for card_file in card_files {
        let (name, line) = describe_card_file(filesystem, &card_file)?;
        lines_by_card_name.insert(name, line);
    }
    Ok(lines_by_card_name
        .into_values()
        .map(|line| line + LINE_END)
        .collect())
}

/// Returns the card files of a cards directory, none when it does not exist.
///
/// # Errors
///
/// Fails when the directory exists and cannot be read.
pub(crate) fn card_files_in(
    filesystem: &impl CardRegistryReader,
    cards_directory: &Path,
) -> io::Result<Vec<PathBuf>> {
    let entries = match filesystem.read_directory(cards_directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(source),
    };
    Ok(entries
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == CARD_EXTENSION)
        })
        .collect())
}

fn describe_card_file(
    filesystem: &impl CardRegistryReader,
    card_file: &Path,
) -> Result<(String, String), CardCommandError> {
    let name = card_name_of(card_file).map_err(|source| CardCommandError::CardNameRefused {
        path: card_file.to_owned(),
        source,
    })?;
    let text = filesystem
        .read_text(card_file)
        .map_err(|source| CardCommandError::Unreadable {
            path: card_file.to_owned(),
            source,
        })?;
    let card = Card::from_str(&text).map_err(|reason| CardCommandError::CardInvalid {
        path: card_file.to_owned(),
        reason,
    })?;
    let line =
        [name.as_str(), &card.handle, &card.public_key].join(&LIST_FIELD_SEPARATOR.to_string());
    Ok((name.as_str().to_owned(), line))
}

/// Reads the card name a card file is stored under.
///
/// # Errors
///
/// Fails when the file stem is not a handle.
pub(crate) fn card_name_of(card_file: &Path) -> Result<Handle, HandleError> {
    card_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or(HandleError::NotAHandle)
        .and_then(Handle::from_str)
}
