use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
const LEARNED_HOST_EXPIRY_DAYS: u64 = 30;
const LEARNED_HOST_EXPIRY_SECONDS: u64 = LEARNED_HOST_EXPIRY_DAYS * SECONDS_PER_DAY;
const OWNER_ONLY_FILE_MODE: u32 = 0o600;
const TEMPORARY_FILE_SUFFIX: &str = "tmp";

/// One host of a network: its node id, relay, newest accepted record `ts`, and when it was last heard if learned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub id: String,
    pub relay: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heard: Option<u64>,
}

/// The durable state of one network: its secret, its owner key, and its hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    pub secret: String,
    pub owner: String,
    pub hosts: Vec<Host>,
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
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
    /// Combines two records of one host: the newer's relay, the larger `ts`, the later `heard`, configured if either is.
    fn merged_with(&self, newer: &Self) -> Self {
        let heard = match (self.heard, newer.heard) {
            (Some(earlier), Some(later)) => Some(earlier.max(later)),
            (None, _) | (_, None) => None,
        };
        Self {
            id: newer.id.clone(),
            relay: newer.relay.clone(),
            ts: self.ts.max(newer.ts),
            heard,
        }
    }

    fn is_learned_and_expired(&self, now_unix_seconds: u64) -> bool {
        self.heard.is_some_and(|heard| {
            now_unix_seconds.saturating_sub(heard) > LEARNED_HOST_EXPIRY_SECONDS
        })
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
    /// Returns a `ConfigError` when the file cannot be read, is malformed, or cannot be written.
    pub fn rewrite(&self, path: &Path, now_unix_seconds: u64) -> Result<Self, ConfigError> {
        let merged = self
            .merged_with(read_current(path)?)
            .without_expired_hosts(now_unix_seconds);
        let text = toml::to_string(&merged).map_err(ConfigError::Unserializable)?;
        write_atomically(path, &text).map_err(io_error(path))?;
        Ok(merged)
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
    toml::from_str(text).map_err(|source| ConfigError::Malformed {
        path: path.to_owned(),
        source,
    })
}

fn read_current(path: &Path) -> Result<Option<NetworkConfig>, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => parse(path, &text).map(Some),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path)(source)),
    }
}

fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    let (file, temporary_path) = create_unique_temporary_file(path)?;
    fill_and_rename(file, &temporary_path, path, contents)
        .inspect_err(|_| remove_temporary_file(&temporary_path))
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

fn remove_temporary_file(temporary_path: &Path) {
    fs::remove_file(temporary_path).unwrap_or_default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const NOW: u64 = 10_000_000;
    const CONCURRENT_WRITERS: usize = 8;
    const CONCURRENT_ROUNDS: usize = 50;
    const EXPIRY: u64 = 30 * 24 * 60 * 60;

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

    fn learned(id: &str, relay: &str, ts: Option<u64>, heard: u64) -> Host {
        Host {
            id: id.to_owned(),
            relay: relay.to_owned(),
            ts,
            heard: Some(heard),
        }
    }

    fn network(hosts: Vec<Host>) -> NetworkConfig {
        NetworkConfig {
            secret: "secret-s".to_owned(),
            owner: "owner-o".to_owned(),
            hosts,
        }
    }

    /// Spec 3.3: a rewrite merges a concurrent writer's extra host, the larger ts and the later heard.
    #[test]
    fn rewrite_unions_hosts_and_takes_larger_ts_and_later_heard() {
        let directory = scratch_directory("merge");
        let path = directory.join("net.toml");
        let on_disk = network(vec![
            learned("alpha", "https://a/", Some(5), NOW - 10),
            learned("beta", "https://b/", Some(8), NOW - 40),
            learned("concurrent", "https://c/", Some(7), NOW - 20),
        ]);
        on_disk.rewrite(&path, NOW).unwrap();
        let held = network(vec![
            learned("alpha", "https://a/", Some(9), NOW - 50),
            learned("beta", "https://b/", Some(4), NOW - 5),
            learned("mine", "https://m/", Some(3), NOW - 30),
        ]);

        let merged = held.rewrite(&path, NOW).unwrap();

        let expected = network(vec![
            learned("alpha", "https://a/", Some(9), NOW - 10),
            learned("beta", "https://b/", Some(8), NOW - 5),
            learned("concurrent", "https://c/", Some(7), NOW - 20),
            learned("mine", "https://m/", Some(3), NOW - 30),
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
            learned("stale", "https://s/", Some(1), NOW - EXPIRY - 1),
            learned("borderline", "https://b/", Some(2), NOW - EXPIRY),
            Host {
                id: "configured".to_owned(),
                relay: "https://k/".to_owned(),
                ts: Some(3),
                heard: None,
            },
        ]);

        let written = held.rewrite(&path, NOW).unwrap();

        let ids: Vec<&str> = written.hosts.iter().map(|host| host.id.as_str()).collect();
        assert_eq!(ids, ["borderline", "configured"]);
        assert_eq!(NetworkConfig::load(&path).unwrap(), written);
    }

    /// Spec 3.3: a host configured on one side stays configured when the other side learned it.
    #[test]
    fn rewrite_keeps_a_host_configured_when_either_side_holds_it_configured() {
        let directory = scratch_directory("configured-wins");
        let path = directory.join("net.toml");
        let owner_configured = Host {
            id: "alpha".to_owned(),
            relay: "https://a/".to_owned(),
            ts: Some(1),
            heard: None,
        };
        network(vec![owner_configured.clone()])
            .rewrite(&path, NOW)
            .unwrap();
        let learned_long_ago = network(vec![learned(
            "alpha",
            "https://a/",
            Some(1),
            NOW - EXPIRY - 5,
        )]);

        let merged = learned_long_ago.rewrite(&path, NOW).unwrap();

        assert_eq!(merged.hosts, [owner_configured]);
    }

    /// Spec 3.3: the file is 0600 after a rewrite, also when it existed with a looser mode.
    #[test]
    fn rewrite_leaves_the_file_owner_only() {
        let directory = scratch_directory("mode");
        let path = directory.join("net.toml");
        let held = network(vec![learned("alpha", "https://a/", Some(1), NOW)]);
        held.rewrite(&path, NOW).unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

        held.rewrite(&path, NOW).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// Spec 3.3: a rewrite renames a temporary file over the old one and leaves nothing beside it.
    #[test]
    fn rewrite_leaves_only_the_configuration_file() {
        let directory = scratch_directory("atomic");
        let path = directory.join("net.toml");

        network(vec![learned("alpha", "https://a/", Some(1), NOW)])
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
        let held = network(vec![learned("alpha", "https://a/", Some(1), NOW)]);

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

    /// Spec 3.3: a rewrite creates the file when none exists.
    #[test]
    fn rewrite_creates_a_missing_file() {
        let directory = scratch_directory("missing");
        let path = directory.join("net.toml");

        let written = network(vec![learned("alpha", "https://a/", None, NOW)])
            .rewrite(&path, NOW)
            .unwrap();

        assert_eq!(NetworkConfig::load(&path).unwrap(), written);
    }
}
