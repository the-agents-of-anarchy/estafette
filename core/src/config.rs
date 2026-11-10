use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use iroh::RelayUrl;
use serde::{Deserialize, Serialize};

use crate::wire::{self, MAX_RELAY_URL_BYTES};

const MILLIS_PER_SECOND: u64 = 1000;
const MILLIS_PER_DAY: u64 = 24 * 60 * 60 * MILLIS_PER_SECOND;
const LEARNED_HOST_EXPIRY_DAYS: u64 = 30;
const LEARNED_HOST_EXPIRY_MILLIS: u64 = LEARNED_HOST_EXPIRY_DAYS * MILLIS_PER_DAY;
pub const OWNER_ONLY_FILE_MODE: u32 = 0o600;
pub const PERMISSION_BITS_MASK: u32 = 0o777;
const TEMPORARY_FILE_SUFFIX: &str = "tmp";
const HEX_KEY_LENGTH: usize = 64;
const RELAY_URL_SCHEME: &str = "https";
const HOST_RELAY_FIELD: &str = "host relay";
const HOST_ID_FIELD: &str = "host id";
const SERDE_MISSING_FIELD_PREFIX: &str = "missing field `";
const SERDE_UNKNOWN_FIELD_PREFIX: &str = "unknown field `";
const SERDE_WRONG_VALUE_PREFIXES: [&str; 3] =
    ["invalid type: ", "invalid value: ", "invalid length "];
const SERDE_EXPECTED_SEPARATOR: &str = ", expected ";
const SERDE_NAME_QUOTE: char = '`';
const NEWLINE: u8 = b'\n';

/// A timestamp of spec 3.1: milliseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMillis(pub u64);

impl UnixMillis {
    /// The timestamp at the start of a whole second since the Unix epoch.
    const fn at_unix_second(unix_seconds: u64) -> Self {
        Self(unix_seconds.saturating_mul(MILLIS_PER_SECOND))
    }

    /// Whether more than the span has passed from this timestamp to `now`.
    const fn is_older_than(self, span_millis: u64, now: Self) -> bool {
        now.0.saturating_sub(self.0) > span_millis
    }
}

/// Where a host came from: the owner configured it and nothing removes it, or this member learned it and it expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostOrigin {
    Configured,
    Learned { heard: UnixMillis },
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
    heard: Option<UnixMillis>,
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

/// Where in a configuration file a refusal points: a line and a column, both counted from one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilePosition {
    pub line: usize,
    pub column: usize,
}

/// What a file that is no network configuration gets wrong, in words of the configuration's own shape and never of the file's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MalformedProblem {
    MissingField { field: String },
    UnknownKey { accepted: String },
    WrongValue { expected: String },
    NotToml,
}

impl fmt::Display for MalformedProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingField { field } => write!(formatter, "the field `{field}` is missing"),
            Self::UnknownKey { accepted } => {
                write!(formatter, "a key it does not know; it accepts {accepted}")
            }
            Self::WrongValue { expected } => {
                write!(
                    formatter,
                    "a value of the wrong type or range; expected {expected}"
                )
            }
            Self::NotToml => formatter.write_str("the text is not valid TOML"),
        }
    }
}

impl MalformedProblem {
    /// The problem a TOML decoding message names, keeping only the words the configuration's shape supplied.
    fn of_decoding_message(message: &str) -> Self {
        let shape_words = || {
            message
                .rsplit_once(SERDE_EXPECTED_SEPARATOR)
                .map(|(_, expected)| expected.to_owned())
        };
        if let Some(field) = message
            .strip_prefix(SERDE_MISSING_FIELD_PREFIX)
            .and_then(|rest| rest.split_once(SERDE_NAME_QUOTE))
            .map(|(field, _)| field.to_owned())
        {
            return Self::MissingField { field };
        }
        if message.starts_with(SERDE_UNKNOWN_FIELD_PREFIX)
            && let Some(accepted) = shape_words()
        {
            return Self::UnknownKey { accepted };
        }
        if SERDE_WRONG_VALUE_PREFIXES
            .iter()
            .any(|prefix| message.starts_with(prefix))
            && let Some(expected) = shape_words()
        {
            return Self::WrongValue { expected };
        }
        Self::NotToml
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
        position: Option<FilePosition>,
        problem: MalformedProblem,
    },
    Unserializable(toml::ser::Error),
    InvalidField {
        path: PathBuf,
        field: &'static str,
        accepted: AcceptedValue,
    },
    InvalidHost {
        path: PathBuf,
        entry: usize,
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
            Self::InvalidHost {
                path,
                entry,
                field,
                accepted,
            } => write!(
                formatter,
                "{}: the {field} of entry {entry} in hosts must be {accepted}",
                path.display()
            ),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Malformed {
                path,
                position: Some(FilePosition { line, column }),
                problem,
            } => write!(
                formatter,
                "{}: not a valid network configuration at line {line}, column {column}: {problem}",
                path.display()
            ),
            Self::Malformed {
                path,
                position: None,
                problem,
            } => write!(
                formatter,
                "{}: not a valid network configuration: {problem}",
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
    /// Reads the relay of the host at that index of `hosts` as the wire's relay URL, naming the file and the host's entry when it is refused.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::InvalidHost` when the relay is over the wire's limit.
    pub fn wire_relay(
        &self,
        index_in_hosts: usize,
        path: &Path,
    ) -> Result<wire::RelayUrl, ConfigError> {
        wire::RelayUrl::try_from(self.relay.clone()).map_err(|_over_limit| {
            host_refusal(
                path,
                index_in_hosts,
                HOST_RELAY_FIELD,
                AcceptedValue::RelayUrl,
            )
        })
    }

    /// Refuses the host at that index of `hosts` unless its id is a lowercase hex key and its relay is a host relay.
    fn validate(&self, index_in_hosts: usize, path: &Path) -> Result<(), ConfigError> {
        if !is_lowercase_hex_key(&self.id) {
            return Err(host_refusal(
                path,
                index_in_hosts,
                HOST_ID_FIELD,
                AcceptedValue::HexKey,
            ));
        }
        if host_relay_url(&self.relay).is_none() {
            return Err(host_refusal(
                path,
                index_in_hosts,
                HOST_RELAY_FIELD,
                AcceptedValue::RelayUrl,
            ));
        }
        Ok(())
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

    const fn is_learned_and_expired(&self, now: UnixMillis) -> bool {
        match self.origin {
            HostOrigin::Configured => false,
            HostOrigin::Learned { heard } => heard.is_older_than(LEARNED_HOST_EXPIRY_MILLIS, now),
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
            .without_expired_hosts(UnixMillis::at_unix_second(now_unix_seconds));
        let text = toml::to_string(&merged).map_err(ConfigError::Unserializable)?;
        write_atomically(path, &text).map_err(io_error(path))?;
        Ok(merged)
    }

    fn validate(&self, path: &Path) -> Result<(), ConfigError> {
        require_hex_key(path, "secret", &self.secret)?;
        require_hex_key(path, "owner", &self.owner)?;
        for (index_in_hosts, host) in self.hosts.iter().enumerate() {
            host.validate(index_in_hosts, path)?;
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

    fn without_expired_hosts(mut self, now: UnixMillis) -> Self {
        self.hosts.retain(|host| !host.is_learned_and_expired(now));
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
    let config: NetworkConfig =
        toml::from_str(text).map_err(|decoding| ConfigError::Malformed {
            path: path.to_owned(),
            position: decoding
                .span()
                .and_then(|span| position_in(text, span.start)),
            problem: MalformedProblem::of_decoding_message(decoding.message()),
        })?;
    config.validate(path)?;
    Ok(config)
}

/// The line and column of a byte offset in the text, both counted from one.
fn position_in(text: &str, offset: usize) -> Option<FilePosition> {
    let before = text.as_bytes().get(..offset)?;
    let lines_begun = before.split(|byte| *byte == NEWLINE).count();
    let bytes_into_line = before
        .rsplit(|byte| *byte == NEWLINE)
        .next()
        .map_or(0, <[u8]>::len);
    Some(FilePosition {
        line: lines_begun,
        column: bytes_into_line.saturating_add(1),
    })
}

/// The refusal of a host's field, naming the file and the host's entry in `hosts` counted from one, never anything the file holds.
fn host_refusal(
    path: &Path,
    index_in_hosts: usize,
    field: &'static str,
    accepted: AcceptedValue,
) -> ConfigError {
    ConfigError::InvalidHost {
        path: path.to_owned(),
        entry: index_in_hosts.saturating_add(1),
        field,
        accepted,
    }
}

fn is_lowercase_hex_key(value: &str) -> bool {
    value.len() == HEX_KEY_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn require_hex_key(path: &Path, field: &'static str, value: &str) -> Result<(), ConfigError> {
    if is_lowercase_hex_key(value) {
        Ok(())
    } else {
        Err(ConfigError::InvalidField {
            path: path.to_owned(),
            field,
            accepted: AcceptedValue::HexKey,
        })
    }
}

/// The relay URL a host relay names, when it is an https URL within the wire's limit: the one rule for a host relay, in a record and in the configuration.
#[must_use]
pub fn host_relay_url(relay: &str) -> Option<RelayUrl> {
    if relay.len() > MAX_RELAY_URL_BYTES {
        return None;
    }
    relay
        .parse::<RelayUrl>()
        .ok()
        .filter(|url| url.scheme() == RELAY_URL_SCHEME)
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
    use crate::crypto::KEY_LEN;
    use crate::keys::render_public_key;
    use iroh::SecretKey;
    use std::collections::BTreeSet;
    use std::fs;

    const NOW_SECONDS: u64 = 1_800_000_000;
    const SECOND_INDEX_IN_HOSTS: usize = 1;
    const VALID_RELAY: &str = "https://relay.example/";
    const NOW_MILLIS: UnixMillis = UnixMillis(NOW_SECONDS * MILLIS_PER_SECOND);
    const GROUP_READABLE_FILE_MODE: u32 = 0o644;
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

    fn host_id(seed: u8) -> String {
        render_public_key(&SecretKey::from_bytes(&[seed; KEY_LEN]).public())
    }

    fn heard_before(millis: u64) -> UnixMillis {
        UnixMillis(NOW_MILLIS.0.checked_sub(millis).unwrap())
    }

    fn learned(id: &str, relay: &str, ts: Option<u64>, heard: UnixMillis) -> Host {
        Host {
            id: id.to_owned(),
            relay: relay.to_owned(),
            ts,
            origin: HostOrigin::Learned { heard },
        }
    }

    /// Spec 3.4 and 3.6: a relay is a wire relay URL only up to the limit, and over it names the host's entry and its relay field.
    #[test]
    fn wire_relay_accepts_the_limit_and_refuses_one_byte_over_naming_the_entry() {
        let path = Path::new("net.toml");
        let relay_of_bytes = |bytes: usize| {
            let scheme_and_slash = "https:///".len();
            format!("https://{}/", "r".repeat(bytes - scheme_and_slash))
        };
        for bytes in (MAX_RELAY_URL_BYTES - 2)..=(MAX_RELAY_URL_BYTES + 2) {
            let host = Host {
                id: host_id(b'a'),
                relay: relay_of_bytes(bytes),
                ts: None,
                origin: HostOrigin::Configured,
            };

            let outcome = host.wire_relay(SECOND_INDEX_IN_HOSTS, path);

            if bytes <= MAX_RELAY_URL_BYTES {
                assert_eq!(outcome.unwrap().as_str(), host.relay, "{bytes} bytes");
            } else {
                match outcome.unwrap_err() {
                    ConfigError::InvalidHost {
                        path: refused_path,
                        entry,
                        field,
                        accepted,
                    } => {
                        assert_eq!(refused_path, path);
                        assert_eq!(entry, SECOND_INDEX_IN_HOSTS + 1);
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
            learned(&host_id(b'a'), "https://a/", Some(5), heard_before(10)),
            learned(&host_id(b'b'), "https://b/", Some(8), heard_before(40)),
            learned(&host_id(b'c'), "https://c/", Some(7), heard_before(20)),
        ]);
        on_disk.rewrite(&path, NOW_SECONDS).unwrap();
        let held = network(vec![
            learned(&host_id(b'a'), "https://a/", Some(9), heard_before(50)),
            learned(&host_id(b'b'), "https://b/", Some(4), heard_before(5)),
            learned(&host_id(b'd'), "https://m/", Some(3), heard_before(30)),
        ]);

        let merged = held.rewrite(&path, NOW_SECONDS).unwrap();

        let mut expected_hosts = vec![
            learned(&host_id(b'a'), "https://a/", Some(9), heard_before(10)),
            learned(&host_id(b'b'), "https://b/", Some(8), heard_before(5)),
            learned(&host_id(b'c'), "https://c/", Some(7), heard_before(20)),
            learned(&host_id(b'd'), "https://m/", Some(3), heard_before(30)),
        ];
        expected_hosts.sort_by(|first, second| first.id.cmp(&second.id));
        let expected = network(expected_hosts);
        assert_eq!(merged, expected);
        assert_eq!(NetworkConfig::load(&path).unwrap(), expected);
    }

    /// Spec 3.1, 3.3 and 3.14: a learned host heard more than 30 days of milliseconds ago is dropped, one heard exactly 30 days ago and a configured host stay.
    #[test]
    fn rewrite_drops_expired_learned_hosts_and_keeps_configured_ones() {
        let directory = scratch_directory("expiry");
        let path = directory.join("net.toml");
        let held = network(vec![
            learned(
                &host_id(b'1'),
                "https://s/",
                Some(1),
                heard_before(LEARNED_HOST_EXPIRY_MILLIS + 1),
            ),
            learned(
                &host_id(b'2'),
                "https://b/",
                Some(2),
                heard_before(LEARNED_HOST_EXPIRY_MILLIS),
            ),
            Host {
                id: host_id(b'3'),
                relay: "https://k/".to_owned(),
                ts: Some(3),
                origin: HostOrigin::Configured,
            },
        ]);

        let written = held.rewrite(&path, NOW_SECONDS).unwrap();

        let ids: BTreeSet<String> = written.hosts.iter().map(|host| host.id.clone()).collect();
        assert_eq!(ids, BTreeSet::from([host_id(b'2'), host_id(b'3')]));
        assert_eq!(NetworkConfig::load(&path).unwrap(), written);
    }

    /// Spec 3.3: a host configured on one side stays configured when the other side learned it.
    #[test]
    fn rewrite_keeps_a_host_configured_when_either_side_holds_it_configured() {
        let directory = scratch_directory("configured-wins");
        let path = directory.join("net.toml");
        let owner_configured = Host {
            id: host_id(b'a'),
            relay: "https://a/".to_owned(),
            ts: Some(1),
            origin: HostOrigin::Configured,
        };
        network(vec![owner_configured.clone()])
            .rewrite(&path, NOW_SECONDS)
            .unwrap();
        let learned_long_ago = network(vec![learned(
            &host_id(b'a'),
            "https://a/",
            Some(1),
            heard_before(LEARNED_HOST_EXPIRY_MILLIS + 5),
        )]);

        let merged = learned_long_ago.rewrite(&path, NOW_SECONDS).unwrap();

        assert_eq!(merged.hosts, [owner_configured]);
    }

    /// Spec 3.3: the file is 0600 after a rewrite, also when it existed with a looser mode.
    #[test]
    fn rewrite_leaves_the_file_owner_only() {
        let directory = scratch_directory("mode");
        let path = directory.join("net.toml");
        let held = network(vec![learned(
            &host_id(b'a'),
            "https://a/",
            Some(1),
            NOW_MILLIS,
        )]);
        held.rewrite(&path, NOW_SECONDS).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(GROUP_READABLE_FILE_MODE)).unwrap();

        held.rewrite(&path, NOW_SECONDS).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & PERMISSION_BITS_MASK, OWNER_ONLY_FILE_MODE);
    }

    /// Spec 3.3: a rewrite renames a temporary file over the old one and leaves nothing beside it.
    #[test]
    fn rewrite_leaves_only_the_configuration_file() {
        let directory = scratch_directory("atomic");
        let path = directory.join("net.toml");

        network(vec![learned(
            &host_id(b'a'),
            "https://a/",
            Some(1),
            NOW_MILLIS,
        )])
        .rewrite(&path, NOW_SECONDS)
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

    const CHOSEN_SECRET: &str = "c0ffee5ec2e7c0ffee5ec2e7c0ffee5ec2e7c0ffee5ec2e7c0ffee5ec2e7c0ff";

    fn rendered_with_its_sources(refusal: &ConfigError) -> String {
        use std::fmt::Write as _;
        let mut rendered = format!("{refusal}\n{refusal:?}");
        let mut source = std::error::Error::source(refusal);
        while let Some(cause) = source {
            write!(rendered, "\n{cause}\n{cause:?}").unwrap();
            source = cause.source();
        }
        rendered
    }

    /// Conventions interfaces-and-refusals and spec 3.3: a refusal at load names the file and what is wrong, never anything the file holds.
    #[test]
    fn load_refusal_never_quotes_the_configuration_file() {
        let directory = scratch_directory("never-quoted");
        let owner = hex_key('b');
        let id = host_id(b'c');
        let second_id = host_id(b'd');
        let network_secret = hex_key('a');
        let malformed_files = [
            (
                "missing-field",
                "line 1, column 1: the field `hosts` is missing",
                format!("secret = \"{CHOSEN_SECRET}\"\nowner = \"{owner}\"\n"),
            ),
            (
                "wrong-type",
                "line 3, column 124: a value of the wrong type or range; expected u64",
                format!(
                    "secret = \"{CHOSEN_SECRET}\"\nowner = \"{owner}\"\nhosts = [{{ id = \"{id}\", relay = \"{VALID_RELAY}\", ts = \"{CHOSEN_SECRET}\" }}]\n"
                ),
            ),
            (
                "unknown-key",
                "line 4, column 1: a key it does not know; it accepts one of `secret`, `owner`, `hosts`",
                format!(
                    "secret = \"{CHOSEN_SECRET}\"\nowner = \"{owner}\"\nhosts = []\n{CHOSEN_SECRET} = 1\n"
                ),
            ),
            (
                "syntax-on-the-secret-line",
                "line 1, column 75: the text is not valid TOML",
                format!("secret = \"{CHOSEN_SECRET}\nowner = \"{owner}\"\nhosts = []\n"),
            ),
            (
                "bad-key-text",
                "secret must be 64 lowercase hexadecimal characters",
                format!("secret = \"{CHOSEN_SECRET}0\"\nowner = \"{owner}\"\nhosts = []\n"),
            ),
            (
                "secret-pasted-as-a-host-id",
                "the host id of entry 2 in hosts must be 64 lowercase hexadecimal characters",
                format!(
                    "secret = \"{network_secret}\"\nowner = \"{owner}\"\nhosts = [{{ id = \"{id}\", relay = \"{VALID_RELAY}\" }}, {{ id = \"{CHOSEN_SECRET}0\", relay = \"{VALID_RELAY}\" }}]\n"
                ),
            ),
            (
                "secret-pasted-as-a-host-relay",
                "the host relay of entry 2 in hosts must be an https URL of at most 128 bytes",
                format!(
                    "secret = \"{network_secret}\"\nowner = \"{owner}\"\nhosts = [{{ id = \"{id}\", relay = \"{VALID_RELAY}\" }}, {{ id = \"{second_id}\", relay = \"{CHOSEN_SECRET}\" }}]\n"
                ),
            ),
        ];

        for (name, what_is_wrong, text) in malformed_files {
            let path = directory.join(format!("{name}.toml"));
            fs::write(&path, text).unwrap();

            let refusal = NetworkConfig::load(&path).unwrap_err();

            let rendered = rendered_with_its_sources(&refusal);
            assert!(!rendered.contains(CHOSEN_SECRET), "{name}: {rendered}");
            assert!(
                rendered.contains(&path.display().to_string()),
                "{name}: {rendered}"
            );
            assert!(
                refusal.to_string().ends_with(what_is_wrong),
                "{name}: {refusal}"
            );
        }
    }

    /// Spec 3.3: a rewrite over a malformed file refuses and leaves the file as it was.
    #[test]
    fn rewrite_refuses_a_malformed_file_and_leaves_it_untouched() {
        let directory = scratch_directory("malformed");
        let path = directory.join("net.toml");
        fs::write(&path, "secret = ").unwrap();

        let outcome = network(vec![]).rewrite(&path, NOW_SECONDS);

        assert!(matches!(outcome, Err(ConfigError::Malformed { .. })));
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret = ");
    }

    /// Spec 3.3: two processes rewriting the same content at once both succeed.
    #[test]
    fn rewrite_by_concurrent_writers_of_the_same_content_all_succeed() {
        let directory = scratch_directory("concurrent");
        let path = directory.join("net.toml");
        let held = network(vec![learned(
            &host_id(b'a'),
            "https://a/",
            Some(1),
            NOW_MILLIS,
        )]);

        for _round in 0..CONCURRENT_ROUNDS {
            std::thread::scope(|scope| {
                let mut writers = Vec::new();
                for _writer in 0..CONCURRENT_WRITERS {
                    writers.push(scope.spawn(|| held.rewrite(&path, NOW_SECONDS)));
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

    fn configuration_text(secret: &str, owner: &str, id: &str, relay: &str) -> String {
        format!(
            "secret = \"{secret}\"\nowner = \"{owner}\"\nhosts = [{{ id = \"{id}\", relay = \"{relay}\" }}]\n"
        )
    }

    /// Spec 3.1 and 3.3: load refuses a secret or owner of the wrong shape, naming the field and the accepted set.
    #[test]
    fn load_refuses_a_secret_or_owner_of_the_wrong_shape() {
        let directory = scratch_directory("shapes");
        let refused_cases = [
            (hex_key('a').to_uppercase(), hex_key('b'), "secret"),
            ("abc".to_owned(), hex_key('b'), "secret"),
            (hex_key('a'), String::new(), "owner"),
        ];

        for (index, (secret, owner, field)) in refused_cases.into_iter().enumerate() {
            let path = directory.join(format!("case-{index}.toml"));
            fs::write(
                &path,
                configuration_text(&secret, &owner, &host_id(b'c'), VALID_RELAY),
            )
            .unwrap();

            let refusal = NetworkConfig::load(&path).unwrap_err();

            assert!(
                matches!(
                    refusal,
                    ConfigError::InvalidField { field: refused, accepted: AcceptedValue::HexKey, .. }
                        if refused == field
                ),
                "case {index}: {refusal}"
            );
        }
    }

    /// Spec 3.1, 3.3 and 3.14: load refuses a host whose id is no lowercase hex key or whose relay is no https URL within the limit, naming its entry and never its text.
    #[test]
    fn load_refuses_a_host_whose_id_or_relay_is_refused_naming_its_entry() {
        let directory = scratch_directory("host-shapes");
        let long_relay = format!("https://{}/", "r".repeat(MAX_RELAY_URL_BYTES));
        let refused_cases = [
            (
                "g".repeat(HEX_KEY_LENGTH),
                VALID_RELAY.to_owned(),
                "host id",
                AcceptedValue::HexKey,
            ),
            (
                host_id(b'c').to_uppercase(),
                VALID_RELAY.to_owned(),
                "host id",
                AcceptedValue::HexKey,
            ),
            (
                host_id(b'c'),
                "http://relay.example/".to_owned(),
                "host relay",
                AcceptedValue::RelayUrl,
            ),
            (
                host_id(b'c'),
                "not a url".to_owned(),
                "host relay",
                AcceptedValue::RelayUrl,
            ),
            (
                host_id(b'c'),
                long_relay,
                "host relay",
                AcceptedValue::RelayUrl,
            ),
        ];

        for (index, (id, relay, field, accepted)) in refused_cases.into_iter().enumerate() {
            let path = directory.join(format!("case-{index}.toml"));
            fs::write(
                &path,
                configuration_text(&hex_key('a'), &hex_key('b'), &id, &relay),
            )
            .unwrap();

            let refusal = NetworkConfig::load(&path).unwrap_err();

            assert!(
                matches!(
                    &refusal,
                    ConfigError::InvalidHost { entry: 1, field: refused, accepted: set, .. }
                        if *refused == field && *set == accepted
                ),
                "case {index}: {refusal}"
            );
            assert!(
                !rendered_with_its_sources(&refusal).contains(&id),
                "case {index}: {refusal}"
            );
        }
    }

    /// Spec 3.1 and 3.3: the refusal message renders the accepted set from the limits that enforce it.
    #[test]
    fn refusal_message_names_the_enforced_limits() {
        let directory = scratch_directory("message");
        let path = directory.join("net.toml");
        let mut held = network(vec![learned(
            &host_id(b'a'),
            "http://insecure/",
            None,
            NOW_MILLIS,
        )]);
        held.secret = "short".to_owned();
        let secret_refusal = held.rewrite(&path, NOW_SECONDS).unwrap_err().to_string();
        held.secret = hex_key('5');
        let relay_refusal = held.rewrite(&path, NOW_SECONDS).unwrap_err().to_string();

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

        let outcome = held.rewrite(&path, NOW_SECONDS);

        assert!(matches!(outcome, Err(ConfigError::InvalidField { .. })));
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    }

    /// Spec 3.3: a rewrite creates the file when none exists.
    #[test]
    fn rewrite_creates_a_missing_file() {
        let directory = scratch_directory("missing");
        let path = directory.join("net.toml");

        let written = network(vec![learned(
            &host_id(b'a'),
            "https://a/",
            None,
            NOW_MILLIS,
        )])
        .rewrite(&path, NOW_SECONDS)
        .unwrap();

        assert_eq!(NetworkConfig::load(&path).unwrap(), written);
    }
}
