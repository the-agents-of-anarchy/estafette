use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use iroh::PublicKey;

use crate::config::{
    AcceptedValue, ConfigError, Host, HostOrigin, NetworkConfig, OWNER_ONLY_FILE_MODE,
};
use crate::crypto::{
    HOST_CERTIFICATE_HEX_CHARS, KEY_LEN, render_host_certificate, sign_host_certificate,
};
use crate::dirs::{Directories, DirectoriesError, DirectoryFilesystem, OperatingSystemFilesystem};
use crate::handle::{Handle, HandleError};
use crate::invite::{
    HOST_LINE_PREFIX, INVITATION_PREFIX, InviteError, parse_host_line, parse_invitation,
    render_invitation,
};
use crate::keys::{
    KeyError, NetworkSecret, generate_network_secret, generate_secret_key, parse_network_secret,
    parse_public_key, parse_secret_key, render_network_secret, render_public_key,
    render_secret_key,
};
use crate::wire::{HostAddr, Invitation, Version};

const CREATE_ABOUT: &str = "Create a network on this machine";
const JOIN_ABOUT: &str = "Join a network from an invitation";
const ADD_HOST_ABOUT: &str = "Add a host to a configured network";
const SHOW_INVITATION_ABOUT: &str = "Print the invitation of a configured network";
const CERTIFY_ABOUT: &str = "Sign a host's node id with the owner key";

fn create_long_about() -> String {
    format!(
        "Create a network on this machine.\n\nReads: whether a configuration file of that name exists.\nProduces: the network configuration file and the owner key file, both readable by you alone; the run record on stderr. The configuration names no host yet, so no invitation exists until a host is added with add-host.\nRefuses: a network name that breaks this rule: {}; a network that already has a configuration file or an owner key file here.",
        HandleError::NotAHandle
    )
}

fn join_long_about() -> String {
    format!(
        "Join a network from an invitation string.\n\nReads: the invitation argument.\nProduces: the network configuration file, readable by you alone, with the hosts the invitation names.\nRefuses: text that does not start with `{INVITATION_PREFIX}` followed by lowercase hexadecimal; an invitation naming no host; an invitation whose owner or a host node id is not an Ed25519 public key; a network that already has a configuration file here."
    )
}

fn add_host_long_about() -> String {
    format!(
        "Add a host to the hosts of a network already configured here.\n\nReads: the network configuration file and the host address line argument.\nProduces: the configuration file rewritten with the host added as configured by you.\nRefuses: a line that does not start with `{HOST_LINE_PREFIX}` followed by lowercase hexadecimal; a node id that is not an Ed25519 public key; a network with no configuration file here; a relay that is not {}.",
        AcceptedValue::RelayUrl
    )
}

const SHOW_INVITATION_LONG_ABOUT: &str = "Print the invitation string of a network configured here.\n\nReads: the network configuration file.\nProduces: one invitation string on stdout.\nRefuses: a network with no configuration file here; a network whose configuration names no host.";

fn certify_long_about() -> String {
    format!(
        "Sign a host's node id with the owner key of a network.\n\nReads: the owner key file of the network and the host address line argument.\nProduces: the host certificate, {HOST_CERTIFICATE_HEX_CHARS} lowercase hexadecimal characters, on stdout.\nRefuses: a network with no owner key file on this machine; a line that does not start with `{HOST_LINE_PREFIX}` followed by lowercase hexadecimal; a node id that is not an Ed25519 public key."
    )
}

/// The network commands of the operator command line.
#[derive(Debug, Clone, Subcommand)]
pub enum NetworkCommand {
    #[command(about = CREATE_ABOUT, long_about = create_long_about())]
    Create { network: Handle },
    #[command(about = JOIN_ABOUT, long_about = join_long_about())]
    Join {
        #[arg(value_parser = parse_invitation)]
        invitation: Invitation,
    },
    #[command(about = ADD_HOST_ABOUT, long_about = add_host_long_about())]
    AddHost {
        network: Handle,
        #[arg(value_parser = parse_host_line)]
        host_line: HostAddr,
    },
    #[command(about = SHOW_INVITATION_ABOUT, long_about = SHOW_INVITATION_LONG_ABOUT)]
    ShowInvitation { network: Handle },
    #[command(about = CERTIFY_ABOUT, long_about = certify_long_about())]
    Certify {
        network: Handle,
        #[arg(value_parser = parse_host_line)]
        host_line: HostAddr,
    },
}

/// The file operations of the network commands, supplied by the composition root.
pub trait NetworkFilesystem: DirectoryFilesystem {
    /// Tells whether a file or directory exists at the path.
    ///
    /// # Errors
    ///
    /// Fails when existence cannot be determined.
    fn path_exists(&self, path: &Path) -> io::Result<bool>;

    /// Reads a whole file as text.
    ///
    /// # Errors
    ///
    /// Fails when the file is missing, unreadable or not UTF-8.
    fn read_to_string(&self, path: &Path) -> io::Result<String>;

    /// Creates a file readable by its owner alone and writes the contents, refusing a path that exists.
    ///
    /// # Errors
    ///
    /// Fails with `AlreadyExists` when the path exists, and with the operating system's error otherwise.
    fn create_new_owner_only_file(&self, path: &Path, contents: &str) -> io::Result<()>;
}

impl NetworkFilesystem for OperatingSystemFilesystem {
    fn path_exists(&self, path: &Path) -> io::Result<bool> {
        path.try_exists()
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn create_new_owner_only_file(&self, path: &Path, contents: &str) -> io::Result<()> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(OWNER_ONLY_FILE_MODE)
            .open(path)?
            .write_all(contents.as_bytes())
    }
}

#[derive(Debug)]
enum Failure {
    Directories(DirectoriesError),
    Config(ConfigError),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Output(io::Error),
    NetworkAlreadyExists {
        network: String,
    },
    NetworkNotConfigured {
        network: String,
    },
    NetworkHasNoHost {
        network: String,
    },
    InvitationNetworkName(HandleError),
    HostNodeIdNotAKey,
    InvitationOwnerNotAKey,
    NoOwnerKey {
        network: String,
    },
    OwnerKeyUnreadable {
        path: PathBuf,
        source: KeyError,
    },
    StoredKey {
        field: &'static str,
        source: KeyError,
    },
    Invitation(InviteError),
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directories(source) => write!(formatter, "{source}"),
            Self::Config(source) => write!(formatter, "{source}"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Output(source) => write!(formatter, "cannot write the result: {source}"),
            Self::NetworkAlreadyExists { network } => write!(
                formatter,
                "network {network} already has a configuration file or an owner key file here: choose another name, or remove both files if the network is abandoned"
            ),
            Self::NetworkNotConfigured { network } => write!(
                formatter,
                "network {network} has no configuration file here: create it, or join it with an invitation"
            ),
            Self::NetworkHasNoHost { network } => write!(
                formatter,
                "network {network} names no host yet, so it has no invitation: add a host first"
            ),
            Self::InvitationNetworkName(source) => {
                write!(
                    formatter,
                    "the invitation's network name is not usable: {source}"
                )
            }
            Self::HostNodeIdNotAKey => {
                write!(formatter, "the host's node id is not an Ed25519 public key")
            }
            Self::InvitationOwnerNotAKey => write!(
                formatter,
                "the invitation's owner is not an Ed25519 public key"
            ),
            Self::NoOwnerKey { network } => write!(
                formatter,
                "network {network} has no owner key file here: only the machine that created the network can certify a host"
            ),
            Self::OwnerKeyUnreadable { path, source } => {
                write!(
                    formatter,
                    "{}: the owner key is not usable: {source}",
                    path.display()
                )
            }
            Self::StoredKey { field, source } => {
                write!(
                    formatter,
                    "the configuration's {field} is not usable: {source}"
                )
            }
            Self::Invitation(source) => write!(formatter, "{source}"),
        }
    }
}

/// A network command that could not run, stated for whoever ran it.
#[derive(Debug)]
pub struct NetworkCommandError(Failure);

impl fmt::Display for NetworkCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for NetworkCommandError {}

impl From<DirectoriesError> for Failure {
    fn from(source: DirectoriesError) -> Self {
        Self::Directories(source)
    }
}

impl From<ConfigError> for Failure {
    fn from(source: ConfigError) -> Self {
        Self::Config(source)
    }
}

/// Runs one network command against this machine's directories, writing the result to `result` and the run record to `record`.
///
/// # Errors
///
/// Returns [`NetworkCommandError`] naming what the operator must change.
pub fn run(
    command: &NetworkCommand,
    directories: &Directories,
    filesystem: &impl NetworkFilesystem,
    fill_from_secure_source: impl FnMut(&mut [u8; KEY_LEN]),
    now_unix_seconds: u64,
    result: &mut impl Write,
    record: &mut impl Write,
) -> Result<(), NetworkCommandError> {
    run_command(
        command,
        directories,
        filesystem,
        fill_from_secure_source,
        now_unix_seconds,
        result,
        record,
    )
    .map_err(NetworkCommandError)
}

fn run_command(
    command: &NetworkCommand,
    directories: &Directories,
    filesystem: &impl NetworkFilesystem,
    fill_from_secure_source: impl FnMut(&mut [u8; KEY_LEN]),
    now_unix_seconds: u64,
    result: &mut impl Write,
    record: &mut impl Write,
) -> Result<(), Failure> {
    match command {
        NetworkCommand::Create { network } => create_network(
            network,
            directories,
            filesystem,
            fill_from_secure_source,
            now_unix_seconds,
            record,
        ),
        NetworkCommand::AddHost { network, host_line } => {
            add_host(network, host_line, directories, now_unix_seconds)
        }
        NetworkCommand::ShowInvitation { network } => show_invitation(network, directories, result),
        NetworkCommand::Certify { network, host_line } => {
            certify_host(network, host_line, directories, filesystem, result)
        }
        NetworkCommand::Join { invitation } => {
            join_network(invitation, directories, filesystem, now_unix_seconds)
        }
    }
}

fn create_network(
    network: &Handle,
    directories: &Directories,
    filesystem: &impl NetworkFilesystem,
    mut fill_from_secure_source: impl FnMut(&mut [u8; KEY_LEN]),
    now_unix_seconds: u64,
    record: &mut impl Write,
) -> Result<(), Failure> {
    let config_file = directories.network_config_file(network.as_str())?;
    let owner_key_file = directories.owner_key_file(network.as_str())?;
    refuse_existing_file(filesystem, network, &config_file)?;
    create_parent_directory(filesystem, &config_file)?;
    let owner = generate_secret_key(&mut fill_from_secure_source);
    write_new_owner_only_file(
        filesystem,
        network,
        &owner_key_file,
        &render_secret_key(&owner),
    )?;
    let secret = generate_network_secret(&mut fill_from_secure_source);
    NetworkConfig {
        secret: render_network_secret(&secret),
        owner: render_public_key(&owner.public()),
        hosts: Vec::new(),
    }
    .rewrite(&config_file, now_unix_seconds)?;
    writeln!(
        record,
        "created network {network}: it names no host yet, so it has no invitation until a host is added",
        network = network.as_str()
    )
    .map_err(Failure::Output)
}

fn refuse_existing_file(
    filesystem: &impl NetworkFilesystem,
    network: &Handle,
    path: &Path,
) -> Result<(), Failure> {
    let exists = filesystem.path_exists(path).map_err(io_failure(path))?;
    if exists {
        Err(Failure::NetworkAlreadyExists {
            network: network.as_str().to_owned(),
        })
    } else {
        Ok(())
    }
}

fn create_parent_directory(
    filesystem: &impl NetworkFilesystem,
    file: &Path,
) -> Result<(), Failure> {
    file.parent().map_or(Ok(()), |parent| {
        filesystem
            .create_private_directory(parent)
            .map_err(io_failure(parent))
    })
}

fn write_new_owner_only_file(
    filesystem: &impl NetworkFilesystem,
    network: &Handle,
    path: &Path,
    contents: &str,
) -> Result<(), Failure> {
    filesystem
        .create_new_owner_only_file(path, contents)
        .map_err(|source| match source.kind() {
            io::ErrorKind::AlreadyExists => Failure::NetworkAlreadyExists {
                network: network.as_str().to_owned(),
            },
            _ => io_failure(path)(source),
        })
}

fn io_failure(path: &Path) -> impl FnOnce(io::Error) -> Failure {
    |source| Failure::Io {
        path: path.to_owned(),
        source,
    }
}

fn join_network(
    invitation: &Invitation,
    directories: &Directories,
    filesystem: &impl NetworkFilesystem,
    now_unix_seconds: u64,
) -> Result<(), Failure> {
    let network = invitation
        .network
        .parse::<Handle>()
        .map_err(Failure::InvitationNetworkName)?;
    let config_file = directories.network_config_file(network.as_str())?;
    refuse_existing_file(filesystem, &network, &config_file)?;
    let owner = public_key_or(&invitation.owner, || Failure::InvitationOwnerNotAKey)?;
    let hosts = invitation
        .hosts
        .iter()
        .map(configured_host)
        .collect::<Result<_, _>>()?;
    create_parent_directory(filesystem, &config_file)?;
    NetworkConfig {
        secret: render_network_secret(&NetworkSecret::from_bytes(invitation.secret)),
        owner: render_public_key(&owner),
        hosts,
    }
    .rewrite(&config_file, now_unix_seconds)?;
    Ok(())
}

fn add_host(
    network: &Handle,
    host_line: &HostAddr,
    directories: &Directories,
    now_unix_seconds: u64,
) -> Result<(), Failure> {
    let (config_file, mut config) = load_configured_network(network, directories)?;
    config.hosts.push(configured_host(host_line)?);
    config.rewrite(&config_file, now_unix_seconds)?;
    Ok(())
}

fn show_invitation(
    network: &Handle,
    directories: &Directories,
    result: &mut impl Write,
) -> Result<(), Failure> {
    let (config_file, config) = load_configured_network(network, directories)?;
    let invitation = invitation_of(network, &config, &config_file)?;
    let text = render_invitation(&invitation).map_err(Failure::Invitation)?;
    writeln!(result, "{text}").map_err(Failure::Output)
}

fn load_configured_network(
    network: &Handle,
    directories: &Directories,
) -> Result<(PathBuf, NetworkConfig), Failure> {
    let config_file = directories.network_config_file(network.as_str())?;
    match NetworkConfig::load(&config_file) {
        Ok(config) => Ok((config_file, config)),
        Err(ConfigError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Err(Failure::NetworkNotConfigured {
                network: network.as_str().to_owned(),
            })
        }
        Err(refusal) => Err(refusal.into()),
    }
}

fn configured_host(address: &HostAddr) -> Result<Host, Failure> {
    Ok(Host {
        id: render_public_key(&host_public_key(address)?),
        relay: address.relay.as_str().to_owned(),
        ts: None,
        origin: HostOrigin::Configured,
    })
}

fn host_public_key(address: &HostAddr) -> Result<PublicKey, Failure> {
    public_key_or(&address.id, || Failure::HostNodeIdNotAKey)
}

fn public_key_or(bytes: &[u8; KEY_LEN], refusal: fn() -> Failure) -> Result<PublicKey, Failure> {
    PublicKey::from_bytes(bytes).map_err(|_not_a_key| refusal())
}

fn invitation_of(
    network: &Handle,
    config: &NetworkConfig,
    config_file: &Path,
) -> Result<Invitation, Failure> {
    if config.hosts.is_empty() {
        return Err(Failure::NetworkHasNoHost {
            network: network.as_str().to_owned(),
        });
    }
    let secret = parse_network_secret(&config.secret).map_err(stored_key("secret"))?;
    let owner = parse_public_key(&config.owner).map_err(stored_key("owner"))?;
    let hosts = config
        .hosts
        .iter()
        .map(|host| host_address_of(host, config_file))
        .collect::<Result<_, _>>()?;
    Ok(Invitation {
        v: Version::CURRENT,
        network: network.as_str().to_owned(),
        secret: *secret.as_bytes(),
        owner: *owner.as_bytes(),
        hosts,
    })
}

fn host_address_of(host: &Host, config_file: &Path) -> Result<HostAddr, Failure> {
    let id = parse_public_key(&host.id).map_err(stored_key("host id"))?;
    let relay = host.wire_relay(config_file)?;
    Ok(HostAddr {
        v: Version::CURRENT,
        id: *id.as_bytes(),
        relay,
    })
}

fn stored_key(field: &'static str) -> impl FnOnce(KeyError) -> Failure {
    move |source| Failure::StoredKey { field, source }
}

fn certify_host(
    network: &Handle,
    host_line: &HostAddr,
    directories: &Directories,
    filesystem: &impl NetworkFilesystem,
    result: &mut impl Write,
) -> Result<(), Failure> {
    let owner_key_file = directories.owner_key_file(network.as_str())?;
    let owner_key_text =
        filesystem
            .read_to_string(&owner_key_file)
            .map_err(|source| match source.kind() {
                io::ErrorKind::NotFound => Failure::NoOwnerKey {
                    network: network.as_str().to_owned(),
                },
                _ => io_failure(&owner_key_file)(source),
            })?;
    let owner =
        parse_secret_key(&owner_key_text).map_err(|source| Failure::OwnerKeyUnreadable {
            path: owner_key_file.clone(),
            source,
        })?;
    let host = host_public_key(host_line)?;
    let certificate = sign_host_certificate(&owner, &host);
    writeln!(result, "{}", render_host_certificate(&certificate)).map_err(Failure::Output)
}
