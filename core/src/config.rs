use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use iroh::RelayUrl;
use serde::{Deserialize, Serialize};

use crate::wire::{self, MAX_RELAY_URL_BYTES};

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
const LEARNED_HOST_EXPIRY_DAYS: u64 = 30;
const LEARNED_HOST_EXPIRY_SECONDS: u64 = LEARNED_HOST_EXPIRY_DAYS * SECONDS_PER_DAY;
pub const OWNER_ONLY_FILE_MODE: u32 = 0o600;
const TEMPORARY_FILE_SUFFIX: &str = "tmp";
const HEX_KEY_LENGTH: usize = 64;
const RELAY_URL_SCHEME: &str = "https";
const HOST_RELAY_FIELD: &str = "host relay";

/// Where a host came from: the owner configured it and nothing removes it, or this member learned it and it expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOrigin {
    Configured,
    Learned { heard: u64 },
}

/// One host of a network: its node id, relay, newest accepted record `ts`, and its origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WireHost", into = "WireHost")]
pub struct Host {
    pub id: String,
    pub relay: String,
    pub ts: Option<u64>,
    pub origin: HostOrigin,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireHost {
    id: String,
    relay: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ts: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    heard: Option<u64>,
}

impl From<WireHost> for Host {
    fn from(wire: WireHost) -> Self {
        Self {
            id: wire.id,
            relay: wire.relay,
            ts: wire.ts,
            origin: wire
                .heard
                .map_or(HostOrigin::Configured, |heard| HostOrigin::Learned {
                    heard,
                }),
        }
    }
}

impl From<Host> for WireHost {
    fn from(host: Host) -> Self {
        Self {
            id: host.id,
            relay: host.relay,
            ts: host.ts,
            heard: match host.origin {
                HostOrigin::Configured => None,
                HostOrigin::Learned { heard } => Some(heard),
            },
        }
    }
}

/// The durable state of one network: its secret, its owner key, and its hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub secret: String,
    pub owner: String,
    pub hosts: Vec<Host>,
}

/// The set of values a configuration field accepts, rendered from the limits that enforce it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptedValue {
    HexKey,
    RelayUrl,
}

impl fmt::Display for AcceptedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HexKey => write!(
                formatter,
                "{HEX_KEY_LENGTH} lowercase hexadecimal characters"
            ),
            Self::RelayUrl => write!(
                formatter,
                "an {RELAY_URL_SCHEME} URL of at most {MAX_RELAY_URL_BYTES} bytes"
            ),
        }
    }
}

/// A refusal to load or rewrite a network configuration file.
#[derive(Debug)]
pub enum ConfigError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Malformed {
        path: PathBuf,
        source: toml::de::Error,
    },
    Unserializable(toml::ser::Error),
    InvalidField {
        path: PathBuf,
        field: &'static str,
        accepted: AcceptedValue,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField {
                path,
                field,
                accepted,
            } => write!(formatter, "{}: {field} must be {accepted}", path.display()),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Malformed { path, source } => write!(
                formatter,
                "{}: not a valid network configuration: {source}",
                path.display()
            ),
            Self::Unserializable(source) => {
                write!(
                    formatter,
                    "network configuration cannot be written: {source}"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl Host {
    /// Reads the relay as the wire's relay URL, naming the configuration file when it is refused.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidField` when the relay is over the wire's limit.
    pub fn wire_relay(&self, path: &Path) -> Result<wire::RelayUrl, ConfigError> {
        wire::RelayUrl::try_from(self.relay.clone()).map_err(|_over_limit| {
            ConfigError::InvalidField {
                path: path.to_owned(),
                field: HOST_RELAY_FIELD,
                accepted: AcceptedValue::RelayUrl,
            }
        })
    }

    /// Combines two records of one host: the newer's relay, the larger `ts`, the later `heard`, configured if either is.
    fn merged_with(&self, newer: &Self) -> Self {
        let origin = match (self.origin, newer.origin) {
            (HostOrigin::Learned { heard: earlier }, HostOrigin::Learned { heard: later }) => {
                HostOrigin::Learned {
                    heard: earlier.max(later),
                }
            }
            (HostOrigin::Configured | HostOrigin::Learned { .. }, HostOrigin::Configured)
            | (HostOrigin::Configured, HostOrigin::Learned { .. }) => HostOrigin::Configured,
        };
        Self {
            id: newer.id.clone(),
            relay: newer.relay.clone(),
            ts: self.ts.max(newer.ts),
            origin,
        }
    }

    const fn is_learned_and_expired(&self, now_unix_seconds: u64) -> bool {
        match self.origin {
            HostOrigin::Configured => false,
            HostOrigin::Learned { heard } => {
                now_unix_seconds.saturating_sub(heard) > LEARNED_HOST_EXPIRY_SECONDS
            }
        }
    }
}

impl NetworkConfig {
    /// Reads and validates the configuration file at `path`.
    ///
    /// # Errors
    /// Returns a `ConfigError` when the file cannot be read or is not a valid network configuration.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = fs::read_to_string(path).map_err(io_error(path))?;
        parse(path, &text)
    }

    /// Merges this configuration with the file's current content, drops expired learned hosts, writes the result whole, and returns it.
    ///
    /// # Errors
    /// Returns a `ConfigError` when this configuration or the file is malformed, or the file cannot be read or written.
    pub fn rewrite(&self, path: &Path, now_unix_seconds: u64) -> Result<Self, ConfigError> {
        self.validate(path)?;
        let merged = self
            .merged_with(read_current(path)?)
            .without_expired_hosts(now_unix_seconds);
        let text = toml::to_string(&merged).map_err(ConfigError::Unserializable)?;
        write_atomically(path, &text).map_err(io_error(path))?;
        Ok(merged)
    }

    fn validate(&self, path: &Path) -> Result<(), ConfigError> {
        require_hex_key(path, "secret", &self.secret)?;
        require_hex_key(path, "owner", &self.owner)?;
        for host in &self.hosts {
            require_hex_key(path, "host id", &host.id)?;
            require_relay_url(path, &host.relay)?;
        }
        Ok(())
    }

    fn merged_with(&self, on_disk: Option<Self>) -> Self {
        let mut hosts: BTreeMap<String, Host> = BTreeMap::new();
        let on_disk_hosts = on_disk.into_iter().flat_map(|config| config.hosts);
        for host in on_disk_hosts.chain(self.hosts.iter().cloned()) {
            let merged = match hosts.get(&host.id) {
                Some(existing) => existing.merged_with(&host),
                None => host,
            };
            hosts.insert(merged.id.clone(), merged);
        }
        Self {
            secret: self.secret.clone(),
            owner: self.owner.clone(),
            hosts: hosts.into_values().collect(),
        }
    }

    fn without_expired_hosts(mut self, now_unix_seconds: u64) -> Self {
        self.hosts
            .retain(|host| !host.is_learned_and_expired(now_unix_seconds));
        self
    }
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> ConfigError {
    |source| ConfigError::Io {
        path: path.to_owned(),
        source,
    }
}

fn parse(path: &Path, text: &str) -> Result<NetworkConfig, ConfigError> {
    let config: NetworkConfig = toml::from_str(text).map_err(|source| ConfigError::Malformed {
        path: path.to_owned(),
        source,
    })?;
    config.validate(path)?;
    Ok(config)
}

fn require_hex_key(path: &Path, field: &'static str, value: &str) -> Result<(), ConfigError> {
    let is_lowercase_hex_key = value.len() == HEX_KEY_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if is_lowercase_hex_key {
        Ok(())
    } else {
        Err(ConfigError::InvalidField {
            path: path.to_owned(),
            field,
            accepted: AcceptedValue::HexKey,
        })
    }
}

fn require_relay_url(path: &Path, value: &str) -> Result<(), ConfigError> {
    let is_https_relay_url = value.len() <= MAX_RELAY_URL_BYTES
        && value
            .parse::<RelayUrl>()
            .is_ok_and(|relay| relay.scheme() == RELAY_URL_SCHEME);
    if is_https_relay_url {
        Ok(())
    } else {
        Err(ConfigError::InvalidField {
            path: path.to_owned(),
            field: HOST_RELAY_FIELD,
            accepted: AcceptedValue::RelayUrl,
        })
    }
}

fn read_current(path: &Path) -> Result<Option<NetworkConfig>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => parse(path, &text).map(Some),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path)(source)),
    }
}

/// Writes the contents to a new uniquely named file readable by its owner alone, syncs it, and renames it over the path, removing it when that fails.
pub(crate) fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let (file, temporary_path) = create_unique_temporary_file(path)?;
    fill_and_rename(file, &temporary_path, path, contents).map_err(|failure| {
        match remove_temporary_file(&temporary_path) {
            Ok(()) => failure,
            Err(cleanup) => io::Error::new(
                failure.kind(),
                format!(
                    "{failure}; {} was left behind: {cleanup}",
                    temporary_path.display()
                ),
            ),
        }
    })
}

fn create_unique_temporary_file(path: &Path) -> io::Result<(File, PathBuf)> {
    for attempt in 0..=u32::MAX {
        let temporary_path =
            path.with_added_extension(format!("{attempt}.{TEMPORARY_FILE_SUFFIX}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(OWNER_ONLY_FILE_MODE)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((file, temporary_path)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(source),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

fn fill_and_rename(
    mut file: File,
    temporary_path: &Path,
    path: &Path,
    contents: &str,
) -> io::Result<()> {
    file.set_permissions(Permissions::from_mode(OWNER_ONLY_FILE_MODE))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    fs::rename(temporary_path, path)
}

fn remove_temporary_file(temporary_path: &Path) -> io::Result<()> {
    match fs::remove_file(temporary_path) {
        Err(source) if source.kind() != io::ErrorKind::NotFound => Err(source),
        Ok(()) | Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const NOW: u64 = 10_000_000;
    const GROUP_READABLE_FILE_MODE: u32 = 0o644;
    const PERMISSION_BITS_MASK: u32 = 0o777;
    const CONCURRENT_WRITERS: usize = 8;
    const CONCURRENT_ROUNDS: usize = 50;

    fn scratch_directory(test_name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "estafette-config-{test_name}-{}",
            std::process::id()
        ));
        if directory.exists() {
            fs::remove_dir_all(&directory).unwrap();
        }
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn hex_key(digit: char) -> String {
        digit.to_string().repeat(HEX_KEY_LENGTH)
    }

    fn learned(id: &str, relay: &str, ts: Option<u64>, heard: u64) -> Host {
        Host {
            id: id.to_owned(),
            relay: relay.to_owned(),
            ts,
            origin: HostOrigin::Learned { heard },
        }
    }

    /// Spec 3.4 and 3.6: a relay is a wire relay URL only up to the limit, and over it names the host relay field.
    #[test]
    fn wire_relay_accepts_the_limit_and_refuses_one_byte_over_naming_the_field() {
        let path = Path::new("net.toml");
        let relay_of_bytes = |bytes: usize| {
            let scheme_and_slash = "https:///".len();
            format!("https://{}/", "r".repeat(bytes - scheme_and_slash))
        };
        for bytes in (MAX_RELAY_URL_BYTES - 2)..=(MAX_RELAY_URL_BYTES + 2) {
            let host = Host {
                id: hex_key('a'),
                relay: relay_of_bytes(bytes),
                ts: None,
                origin: HostOrigin::Configured,
            };

            let outcome = host.wire_relay(path);

            if bytes <= MAX_RELAY_URL_BYTES {
                assert_eq!(outcome.unwrap().as_str(), host.relay, "{bytes} bytes");
            } else {
                match outcome.unwrap_err() {
                    ConfigError::InvalidField {
                        path: refused_path,
                        field,
                        accepted,
                    } => {
                        assert_eq!(refused_path, path);
                        assert_eq!(field, "host relay");
                        assert_eq!(accepted, AcceptedValue::RelayUrl);
                    }
                    other => panic!("{bytes} bytes: {other}"),
                }
            }
        }
    }

    fn network(hosts: Vec<Host>) -> NetworkConfig {
        NetworkConfig {
            secret: hex_key('5'),
            owner: hex_key('6'),
            hosts,
        }
    }

    /// Spec 3.3: a rewrite merges a concurrent writer's extra host, the larger ts and the later heard.
    #[test]
    fn rewrite_unions_hosts_and_takes_larger_ts_and_later_heard() {
        let directory = scratch_directory("merge");
        let path = directory.join("net.toml");
        let on_disk = network(vec![
            learned(&hex_key('a'), "https://a/", Some(5), NOW - 10),
            learned(&hex_key('b'), "https://b/", Some(8), NOW - 40),
            learned(&hex_key('c'), "https://c/", Some(7), NOW - 20),
        ]);
        on_disk.rewrite(&path, NOW).unwrap();
        let held = network(vec![
            learned(&hex_key('a'), "https://a/", Some(9), NOW - 50),
            learned(&hex_key('b'), "https://b/", Some(4), NOW - 5),
            learned(&hex_key('d'), "https://m/", Some(3), NOW - 30),
        ]);

        let merged = held.rewrite(&path, NOW).unwrap();

        let expected = network(vec![
            learned(&hex_key('a'), "https://a/", Some(9), NOW - 10),
            learned(&hex_key('b'), "https://b/", Some(8), NOW - 5),
            learned(&hex_key('c'), "https://c/", Some(7), NOW - 20),
            learned(&hex_key('d'), "https://m/", Some(3), NOW - 30),
        ]);
        assert_eq!(merged, expected);
        assert_eq!(NetworkConfig::load(&path).unwrap(), expected);
    }

    /// Spec 3.3 and 3.14: a learned host heard more than 30 days ago is dropped, one heard exactly 30 days ago and a configured host stay.
    #[test]
    fn rewrite_drops_expired_learned_hosts_and_keeps_configured_ones() {
        let directory = scratch_directory("expiry");
        let path = directory.join("net.toml");
        let held = network(vec![
            learned(
                &hex_key('1'),
                "https://s/",
                Some(1),
                NOW - LEARNED_HOST_EXPIRY_SECONDS - 1,
            ),
            learned(
                &hex_key('2'),
                "https://b/",
                Some(2),
                NOW - LEARNED_HOST_EXPIRY_SECONDS,
            ),
            Host {
                id: hex_key('3'),
                relay: "https://k/".to_owned(),
                ts: Some(3),
                origin: HostOrigin::Configured,
            },
        ]);

        let written = held.rewrite(&path, NOW).unwrap();

        let ids: Vec<&str> = written.hosts.iter().map(|host| host.id.as_str()).collect();
        assert_eq!(ids, [hex_key('2'), hex_key('3')]);
        assert_eq!(NetworkConfig::load(&path).unwrap(), written);
    }

    /// Spec 3.3: a host configured on one side stays configured when the other side learned it.
    #[test]
    fn rewrite_keeps_a_host_configured_when_either_side_holds_it_configured() {
        let directory = scratch_directory("configured-wins");
        let path = directory.join("net.toml");
        let owner_configured = Host {
            id: hex_key('a'),
            relay: "https://a/".to_owned(),
            ts: Some(1),
            origin: HostOrigin::Configured,
        };
        network(vec![owner_configured.clone()])
            .rewrite(&path, NOW)
            .unwrap();
        let learned_long_ago = network(vec![learned(
            &hex_key('a'),
            "https://a/",
            Some(1),
            NOW - LEARNED_HOST_EXPIRY_SECONDS - 5,
        )]);

        let merged = learned_long_ago.rewrite(&path, NOW).unwrap();

        assert_eq!(merged.hosts, [owner_configured]);
    }

    /// Spec 3.3: the file is 0600 after a rewrite, also when it existed with a looser mode.
    #[test]
    fn rewrite_leaves_the_file_owner_only() {
        let directory = scratch_directory("mode");
        let path = directory.join("net.toml");
        let held = network(vec![learned(&hex_key('a'), "https://a/", Some(1), NOW)]);
        held.rewrite(&path, NOW).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(GROUP_READABLE_FILE_MODE)).unwrap();

        held.rewrite(&path, NOW).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & PERMISSION_BITS_MASK, OWNER_ONLY_FILE_MODE);
    }

    /// Spec 3.3: a rewrite renames a temporary file over the old one and leaves nothing beside it.
    #[test]
    fn rewrite_leaves_only_the_configuration_file() {
        let directory = scratch_directory("atomic");
        let path = directory.join("net.toml");

        network(vec![learned(&hex_key('a'), "https://a/", Some(1), NOW)])
            .rewrite(&path, NOW)
            .unwrap();

        let names: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["net.toml"]);
    }

    /// Conventions interfaces-and-refusals: an unknown key at either level is refused at load.
    #[test]
    fn load_refuses_an_unknown_key() {
        let directory = scratch_directory("unknown-key");
        let top_level = directory.join("top.toml");
        let in_host = directory.join("host.toml");
        fs::write(
            &top_level,
            "secret = \"s\"\nowner = \"o\"\nhosts = []\nextra = 1\n",
        )
        .unwrap();
        fs::write(
            &in_host,
            "secret = \"s\"\nowner = \"o\"\nhosts = [{ id = \"a\", relay = \"r\", extra = 1 }]\n",
        )
        .unwrap();

        assert!(matches!(
            NetworkConfig::load(&top_level),
            Err(ConfigError::Malformed { .. })
        ));
        assert!(matches!(
            NetworkConfig::load(&in_host),
            Err(ConfigError::Malformed { .. })
        ));
    }

    /// Spec 3.3: a rewrite over a malformed file refuses and leaves the file as it was.
    #[test]
    fn rewrite_refuses_a_malformed_file_and_leaves_it_untouched() {
        let directory = scratch_directory("malformed");
        let path = directory.join("net.toml");
        fs::write(&path, "secret = ").unwrap();

        let outcome = network(vec![]).rewrite(&path, NOW);

        assert!(matches!(outcome, Err(ConfigError::Malformed { .. })));
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret = ");
    }

    /// Spec 3.3: two processes rewriting the same content at once both succeed.
    #[test]
    fn rewrite_by_concurrent_writers_of_the_same_content_all_succeed() {
        let directory = scratch_directory("concurrent");
        let path = directory.join("net.toml");
        let held = network(vec![learned(&hex_key('a'), "https://a/", Some(1), NOW)]);

        for _round in 0..CONCURRENT_ROUNDS {
            std::thread::scope(|scope| {
                let mut writers = Vec::new();
                for _writer in 0..CONCURRENT_WRITERS {
                    writers.push(scope.spawn(|| held.rewrite(&path, NOW)));
                }
                for writer in writers {
                    assert_eq!(writer.join().unwrap().unwrap(), held);
                }
            });
        }
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }

    /// Spec 3.3: a write that fails after the temporary file exists leaves no temporary file behind.
    #[test]
    fn failed_write_removes_its_temporary_file() {
        let directory = scratch_directory("debris");
        let blocked_final_path = directory.join("net.toml");
        fs::create_dir(&blocked_final_path).unwrap();

        let outcome = write_atomically(&blocked_final_path, "secret = \"x\"\n");

        assert!(outcome.is_err());
        let names: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["net.toml"]);
    }

    /// Spec 3.1 and 3.3: load refuses a secret, owner, node id or relay of the wrong shape, naming the accepted set.
    #[test]
    fn load_refuses_a_field_of_the_wrong_shape() {
        let directory = scratch_directory("shapes");
        let valid_relay = "https://relay.example/";
        let long_relay = format!("https://{}/", "r".repeat(MAX_RELAY_URL_BYTES));
        let refused_cases = [
            (
                hex_key('a').to_uppercase(),
                hex_key('b'),
                hex_key('c'),
                valid_relay.to_owned(),
                "secret",
                AcceptedValue::HexKey,
            ),
            (
                "abc".to_owned(),
                hex_key('b'),
                hex_key('c'),
                valid_relay.to_owned(),
                "secret",
                AcceptedValue::HexKey,
            ),
            (
                hex_key('a'),
                String::new(),
                hex_key('c'),
                valid_relay.to_owned(),
                "owner",
                AcceptedValue::HexKey,
            ),
            (
                hex_key('a'),
                hex_key('b'),
                "g".repeat(HEX_KEY_LENGTH),
                valid_relay.to_owned(),
                "host id",
                AcceptedValue::HexKey,
            ),
            (
                hex_key('a'),
                hex_key('b'),
                hex_key('c'),
                "http://relay.example/".to_owned(),
                "host relay",
                AcceptedValue::RelayUrl,
            ),
            (
                hex_key('a'),
                hex_key('b'),
                hex_key('c'),
                "not a url".to_owned(),
                "host relay",
                AcceptedValue::RelayUrl,
            ),
            (
                hex_key('a'),
                hex_key('b'),
                hex_key('c'),
                long_relay,
                "host relay",
                AcceptedValue::RelayUrl,
            ),
        ];

        for (index, (secret, owner, id, relay, field, accepted)) in
            refused_cases.into_iter().enumerate()
        {
            let path = directory.join(format!("case-{index}.toml"));
            fs::write(
                &path,
                format!(
                    "secret = \"{secret}\"\nowner = \"{owner}\"\nhosts = [{{ id = \"{id}\", relay = \"{relay}\" }}]\n"
                ),
            )
            .unwrap();

            let refusal = NetworkConfig::load(&path).unwrap_err();

            assert!(
                matches!(
                    refusal,
                    ConfigError::InvalidField { field: refused, accepted: set, .. }
                        if refused == field && set == accepted
                ),
                "case {index}: {refusal}"
            );
        }
    }

    /// Spec 3.1 and 3.3: the refusal message renders the accepted set from the limits that enforce it.
    #[test]
    fn refusal_message_names_the_enforced_limits() {
        let directory = scratch_directory("message");
        let path = directory.join("net.toml");
        let mut held = network(vec![learned(&hex_key('a'), "http://insecure/", None, NOW)]);
        held.secret = "short".to_owned();
        let secret_refusal = held.rewrite(&path, NOW).unwrap_err().to_string();
        held.secret = hex_key('5');
        let relay_refusal = held.rewrite(&path, NOW).unwrap_err().to_string();

        assert!(secret_refusal.contains(&format!("{HEX_KEY_LENGTH} lowercase hexadecimal")));
        assert!(secret_refusal.contains("secret must be"));
        assert!(relay_refusal.contains(&format!("{MAX_RELAY_URL_BYTES} bytes")));
        assert!(relay_refusal.contains(RELAY_URL_SCHEME));
    }

    /// Spec 3.3: a rewrite refuses a held configuration of the wrong shape before writing anything.
    #[test]
    fn rewrite_refuses_a_held_configuration_of_the_wrong_shape() {
        let directory = scratch_directory("held-shape");
        let path = directory.join("net.toml");
        let mut held = network(vec![]);
        held.secret = "short".to_owned();

        let outcome = held.rewrite(&path, NOW);

        assert!(matches!(outcome, Err(ConfigError::InvalidField { .. })));
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    }

    /// Spec 3.3: a rewrite creates the file when none exists.
    #[test]
    fn rewrite_creates_a_missing_file() {
        let directory = scratch_directory("missing");
        let path = directory.join("net.toml");

        let written = network(vec![learned(&hex_key('a'), "https://a/", None, NOW)])
            .rewrite(&path, NOW)
            .unwrap();

        assert_eq!(NetworkConfig::load(&path).unwrap(), written);
    }
}
