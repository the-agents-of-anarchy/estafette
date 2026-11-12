use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::{Args, Subcommand};

use crate::card::{Card, CardInvalid, CardUnusable};
use crate::cli::lines::tab_separated_line;
use crate::dirs::{
    CARD_EXTENSION, CARDS_DIRECTORY_VARIABLE, CARDS_SUBDIRECTORY, Directories, DirectoriesError,
    DirectoryFilesystem, OperatingSystemFilesystem,
};
use crate::errors::ExitStatus;
use crate::handle::{Handle, HandleError};
use crate::keys::render_public_key;
use crate::sidecar::{CARD_FILE_MODE, SEED_BYTES, SidecarError, create_card};

const LINE_END: &str = "\n";
const CREATE_CARD_ABOUT: &str = "Create a card and its key file";
const LIST_CARDS_ABOUT: &str = "List the cards of a network";

fn cards_directory_description() -> String {
    format!(
        "--cards-dir or {CARDS_DIRECTORY_VARIABLE} followed by the network name, else the data directory's {CARDS_SUBDIRECTORY}/<network>"
    )
}

fn create_card_long_about() -> String {
    let cards_directory = cards_directory_description();
    format!(
        "Create a card with a fresh key for a network.\n\nReads: the cards directory of the network, which is {cards_directory}; nothing of the network configuration.\nProduces: the configuration, data and runtime directories of this program, readable by you alone, when missing; the card file <card>.{CARD_EXTENSION} with mode {CARD_FILE_MODE:04o} and the key file <card>.key, readable by you alone, in that directory, created when missing; the card's public key, as lowercase hexadecimal, on stdout.\nRefuses: a network, a card or a handle that breaks this rule: {}; a card or key file of that name that already exists, overwriting neither.",
        HandleError::NotAHandle
    )
}

fn list_cards_long_about() -> String {
    let cards_directory = cards_directory_description();
    format!(
        "List the cards of a network, one line each.\n\nReads: every .{CARD_EXTENSION} file in the cards directory of the network, which is {cards_directory}; nothing of the network configuration.\nProduces: the configuration, data and runtime directories of this program, readable by you alone, when missing; one line per card on stdout, in card name order: the card name, the handle and the public key, separated by tabs; nothing more when the cards directory does not exist.\nRefuses: a network that breaks this rule: {}; a card file whose name is not a handle or whose content is not a valid card; a directory that cannot be read.",
        HandleError::NotAHandle
    )
}

/// The network a command works on and the optional cards directory that replaces the default one.
#[derive(Debug, Clone, Args)]
pub struct RegistryArgs {
    pub network: Handle,
    /// The cards directory, replacing the default one
    #[arg(long)]
    pub cards_dir: Option<PathBuf>,
}

/// The card commands of spec 3.13.
#[derive(Debug, Subcommand)]
pub enum CardCommand {
    #[command(about = CREATE_CARD_ABOUT, long_about = create_card_long_about())]
    CreateCard {
        #[command(flatten)]
        registry: RegistryArgs,
        card: Handle,
        handle: Handle,
    },
    #[command(about = LIST_CARDS_ABOUT, long_about = list_cards_long_about())]
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
    CardUnusable { path: PathBuf, reason: CardUnusable },
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
            Self::CardUnusable { path, reason } => {
                write!(formatter, "{} is not usable here: {reason}", path.display())
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
                &handle,
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
    handle: &Handle,
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
        let (name, line) = describe_card_file(filesystem, &card_file, network)?;
        lines_by_card_name.insert(name, line);
    }
    Ok(lines_by_card_name.into_values().collect())
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
    network: &str,
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
    card.check_usable_on(network)
        .map_err(|reason| CardCommandError::CardUnusable {
            path: card_file.to_owned(),
            reason,
        })?;
    let line = tab_separated_line(&[name.as_str(), &card.handle, &card.public_key]);
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
