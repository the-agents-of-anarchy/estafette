use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use data_encoding::HEXLOWER;
use iroh::PublicKey;

use crate::config::{ConfigError, Host, HostOrigin, NetworkConfig};
use crate::crypto::sign_host_certificate;
use crate::dirs::{Directories, DirectoriesError, DirectoryFilesystem};
use crate::handle::{Handle, HandleError};
use crate::invite::{InviteError, parse_host_line, parse_invitation, render_invitation};
use crate::keys::{
    KeyError, generate_secret_key, parse_network_secret, parse_public_key, parse_secret_key,
    render_public_key, render_secret_key,
};
use crate::wire::{CodecError, HostAddr, Invitation, RelayUrl, Version};

const OWNER_ONLY_FILE_MODE: u32 = 0o600;
const RANDOM_BYTES: usize = 32;

const CREATE_ABOUT: &str = "Create a network on this machine";
const CREATE_LONG_ABOUT: &str = "Create a network on this machine.\n\nReads: whether a configuration file of that name exists.\nProduces: the network configuration file and the owner key file, both readable by you alone; the run record on stderr. The configuration names no host yet, so no invitation exists until a host is added with add-host.\nRefuses: a network name that is not 1 to 32 characters of a-z, 0-9 and '-' starting with a letter; a network that already has a configuration file or an owner key file here.";
const JOIN_ABOUT: &str = "Join a network from an invitation";
const JOIN_LONG_ABOUT: &str = "Join a network from an invitation string.\n\nReads: the invitation argument.\nProduces: the network configuration file, readable by you alone, with the hosts the invitation names.\nRefuses: text that does not start with `estafette-invite-1:` followed by lowercase hexadecimal; an invitation naming no host; a network that already has a configuration file here.";
const ADD_HOST_ABOUT: &str = "Add a host to a configured network";
const ADD_HOST_LONG_ABOUT: &str = "Add a host to the hosts of a network already configured here.\n\nReads: the network configuration file and the host address line argument.\nProduces: the configuration file rewritten with the host added as configured by you.\nRefuses: a line that does not start with `estafette-host-1:` followed by lowercase hexadecimal; a network with no configuration file here; a relay that is not an https URL of at most 128 bytes.";
const SHOW_INVITATION_ABOUT: &str = "Print the invitation of a configured network";
const SHOW_INVITATION_LONG_ABOUT: &str = "Print the invitation string of a network configured here.\n\nReads: the network configuration file.\nProduces: one invitation string on stdout.\nRefuses: a network with no configuration file here; a network whose configuration names no host.";
const CERTIFY_ABOUT: &str = "Sign a host's node id with the owner key";
const CERTIFY_LONG_ABOUT: &str = "Sign a host's node id with the owner key of a network.\n\nReads: the owner key file of the network and the host address line argument.\nProduces: the host certificate, 128 lowercase hexadecimal characters, on stdout.\nRefuses: a network with no owner key file on this machine; a line that does not start with `estafette-host-1:` followed by lowercase hexadecimal.";

/// The network commands of the operator command line.
#[derive(Debug, Clone, Subcommand)]
pub enum NetworkCommand {
    #[command(about = CREATE_ABOUT, long_about = CREATE_LONG_ABOUT)]
    Create { network: Handle },
    #[command(about = JOIN_ABOUT, long_about = JOIN_LONG_ABOUT)]
    Join {
        #[arg(value_parser = parse_invitation)]
        invitation: Invitation,
    },
    #[command(about = ADD_HOST_ABOUT, long_about = ADD_HOST_LONG_ABOUT)]
    AddHost {
        network: Handle,
        #[arg(value_parser = parse_host_line)]
        host_line: HostAddr,
    },
    #[command(about = SHOW_INVITATION_ABOUT, long_about = SHOW_INVITATION_LONG_ABOUT)]
    ShowInvitation { network: Handle },
    #[command(about = CERTIFY_ABOUT, long_about = CERTIFY_LONG_ABOUT)]
    Certify {
        network: Handle,
        #[arg(value_parser = parse_host_line)]
        host_line: HostAddr,
    },
}

/// A network command that could not run, stated for whoever ran it.
#[derive(Debug)]
pub enum NetworkCommandError {
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
    HostLineNodeIdNotAKey,
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
    StoredHostRelay(CodecError),
    Invitation(InviteError),
}

impl fmt::Display for NetworkCommandError {
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
            Self::HostLineNodeIdNotAKey => write!(
                formatter,
                "the host address line's node id is not an Ed25519 public key"
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
            Self::StoredHostRelay(source) => {
                write!(
                    formatter,
                    "the configuration's host relay is not usable: {source}"
                )
            }
            Self::Invitation(source) => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for NetworkCommandError {}

impl From<DirectoriesError> for NetworkCommandError {
    fn from(source: DirectoriesError) -> Self {
        Self::Directories(source)
    }
}

impl From<ConfigError> for NetworkCommandError {
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
    filesystem: &impl DirectoryFilesystem,
    fill_from_secure_source: impl FnMut(&mut [u8; RANDOM_BYTES]),
    now_unix_seconds: u64,
    result: &mut impl Write,
    record: &mut impl Write,
) -> Result<(), NetworkCommandError> {
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
            certify_host(network, host_line, directories, result)
        }
        NetworkCommand::Join { invitation } => {
            join_network(invitation, directories, filesystem, now_unix_seconds)
        }
    }
}

fn create_network(
    network: &Handle,
    directories: &Directories,
    filesystem: &impl DirectoryFilesystem,
    mut fill_from_secure_source: impl FnMut(&mut [u8; RANDOM_BYTES]),
    now_unix_seconds: u64,
    record: &mut impl Write,
) -> Result<(), NetworkCommandError> {
    let config_file = directories.network_config_file(network.as_str())?;
    let owner_key_file = directories.owner_key_file(network.as_str())?;
    refuse_existing_file(network, &config_file)?;
    create_parent_directory(filesystem, &config_file)?;
    let owner = generate_secret_key(&mut fill_from_secure_source);
    write_new_owner_only_file(network, &owner_key_file, &render_secret_key(&owner))?;
    let mut secret = [0; RANDOM_BYTES];
    fill_from_secure_source(&mut secret);
    NetworkConfig {
        secret: HEXLOWER.encode(&secret),
        owner: render_public_key(&owner.public()),
        hosts: Vec::new(),
    }
    .rewrite(&config_file, now_unix_seconds)?;
    writeln!(
        record,
        "created network {network}: it names no host yet, so it has no invitation until a host is added",
        network = network.as_str()
    )
    .map_err(NetworkCommandError::Output)
}

fn refuse_existing_file(network: &Handle, path: &Path) -> Result<(), NetworkCommandError> {
    let exists = path.try_exists().map_err(io_failure(path))?;
    if exists {
        Err(NetworkCommandError::NetworkAlreadyExists {
            network: network.as_str().to_owned(),
        })
    } else {
        Ok(())
    }
}

fn create_parent_directory(
    filesystem: &impl DirectoryFilesystem,
    file: &Path,
) -> Result<(), NetworkCommandError> {
    file.parent().map_or(Ok(()), |parent| {
        filesystem
            .create_private_directory(parent)
            .map_err(io_failure(parent))
    })
}

fn write_new_owner_only_file(
    network: &Handle,
    path: &Path,
    contents: &str,
) -> Result<(), NetworkCommandError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(OWNER_ONLY_FILE_MODE)
        .open(path)
        .map_err(|source| match source.kind() {
            io::ErrorKind::AlreadyExists => NetworkCommandError::NetworkAlreadyExists {
                network: network.as_str().to_owned(),
            },
            _ => io_failure(path)(source),
        })?;
    file.write_all(contents.as_bytes())
        .map_err(io_failure(path))
}

fn io_failure(path: &Path) -> impl FnOnce(io::Error) -> NetworkCommandError {
    |source| NetworkCommandError::Io {
        path: path.to_owned(),
        source,
    }
}

fn join_network(
    invitation: &Invitation,
    directories: &Directories,
    filesystem: &impl DirectoryFilesystem,
    now_unix_seconds: u64,
) -> Result<(), NetworkCommandError> {
    let network = invitation
        .network
        .parse::<Handle>()
        .map_err(NetworkCommandError::InvitationNetworkName)?;
    let config_file = directories.network_config_file(network.as_str())?;
    refuse_existing_file(&network, &config_file)?;
    create_parent_directory(filesystem, &config_file)?;
    NetworkConfig {
        secret: HEXLOWER.encode(&invitation.secret),
        owner: HEXLOWER.encode(&invitation.owner),
        hosts: invitation.hosts.iter().map(configured_host).collect(),
    }
    .rewrite(&config_file, now_unix_seconds)?;
    Ok(())
}

fn add_host(
    network: &Handle,
    host_line: &HostAddr,
    directories: &Directories,
    now_unix_seconds: u64,
) -> Result<(), NetworkCommandError> {
    let (config_file, mut config) = load_configured_network(network, directories)?;
    config.hosts.push(configured_host(host_line));
    config.rewrite(&config_file, now_unix_seconds)?;
    Ok(())
}

fn show_invitation(
    network: &Handle,
    directories: &Directories,
    result: &mut impl Write,
) -> Result<(), NetworkCommandError> {
    let (_, config) = load_configured_network(network, directories)?;
    let invitation = invitation_of(network, &config)?;
    let text = render_invitation(&invitation).map_err(NetworkCommandError::Invitation)?;
    writeln!(result, "{text}").map_err(NetworkCommandError::Output)
}

fn load_configured_network(
    network: &Handle,
    directories: &Directories,
) -> Result<(PathBuf, NetworkConfig), NetworkCommandError> {
    let config_file = directories.network_config_file(network.as_str())?;
    match NetworkConfig::load(&config_file) {
        Ok(config) => Ok((config_file, config)),
        Err(ConfigError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Err(NetworkCommandError::NetworkNotConfigured {
                network: network.as_str().to_owned(),
            })
        }
        Err(refusal) => Err(refusal.into()),
    }
}

fn configured_host(address: &HostAddr) -> Host {
    Host {
        id: HEXLOWER.encode(&address.id),
        relay: address.relay.as_str().to_owned(),
        ts: None,
        origin: HostOrigin::Configured,
    }
}

fn invitation_of(
    network: &Handle,
    config: &NetworkConfig,
) -> Result<Invitation, NetworkCommandError> {
    if config.hosts.is_empty() {
        return Err(NetworkCommandError::NetworkHasNoHost {
            network: network.as_str().to_owned(),
        });
    }
    let secret = parse_network_secret(&config.secret).map_err(stored_key("secret"))?;
    let owner = parse_public_key(&config.owner).map_err(stored_key("owner"))?;
    let hosts = config
        .hosts
        .iter()
        .map(host_address_of)
        .collect::<Result<_, _>>()?;
    Ok(Invitation {
        v: Version::CURRENT,
        network: network.as_str().to_owned(),
        secret: *secret.as_bytes(),
        owner: *owner.as_bytes(),
        hosts,
    })
}

fn host_address_of(host: &Host) -> Result<HostAddr, NetworkCommandError> {
    let id = parse_public_key(&host.id).map_err(stored_key("host id"))?;
    let relay =
        RelayUrl::try_from(host.relay.clone()).map_err(NetworkCommandError::StoredHostRelay)?;
    Ok(HostAddr {
        v: Version::CURRENT,
        id: *id.as_bytes(),
        relay,
    })
}

fn stored_key(field: &'static str) -> impl FnOnce(KeyError) -> NetworkCommandError {
    move |source| NetworkCommandError::StoredKey { field, source }
}

fn certify_host(
    network: &Handle,
    host_line: &HostAddr,
    directories: &Directories,
    result: &mut impl Write,
) -> Result<(), NetworkCommandError> {
    let owner_key_file = directories.owner_key_file(network.as_str())?;
    let owner_key_text =
        fs::read_to_string(&owner_key_file).map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => NetworkCommandError::NoOwnerKey {
                network: network.as_str().to_owned(),
            },
            _ => io_failure(&owner_key_file)(source),
        })?;
    let owner = parse_secret_key(&owner_key_text).map_err(|source| {
        NetworkCommandError::OwnerKeyUnreadable {
            path: owner_key_file.clone(),
            source,
        }
    })?;
    let host = PublicKey::from_bytes(&host_line.id)
        .ok()
        .ok_or(NetworkCommandError::HostLineNodeIdNotAKey)?;
    let certificate = sign_host_certificate(&owner, &host);
    writeln!(result, "{}", HEXLOWER.encode(&certificate.to_bytes()))
        .map_err(NetworkCommandError::Output)
}
