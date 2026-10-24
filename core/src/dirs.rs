use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

const PRODUCT_DIRECTORY: &str = "estafette";
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const HOME_VARIABLE: &str = "HOME";
const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
const CONFIG_HOME_UNDER_HOME: &str = ".config";
const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
const DATA_HOME_UNDER_HOME: &str = ".local/share";
const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
const TEMPORARY_DIRECTORY_VARIABLE: &str = "TMPDIR";
const SYSTEM_TEMPORARY_DIRECTORY: &str = "/tmp";
const CARDS_DIRECTORY_VARIABLE: &str = "ESTAFETTE_CARDS_DIR";
const CARDS_SUBDIRECTORY: &str = "cards";
const NETWORKS_SUBDIRECTORY: &str = "networks";
const INFRASTRUCTURE_SUBDIRECTORY: &str = "infra";
const NETWORK_CONFIG_EXTENSION: &str = "toml";
const OWNER_KEY_EXTENSION: &str = "owner.key";
const CARD_EXTENSION: &str = "yaml";
const KEY_EXTENSION: &str = "key";
const CERTIFICATE_EXTENSION: &str = "cert";
const LAST_RECORD_TIMESTAMP_EXTENSION: &str = "ts";
const CERTIFICATE_CACHE_EXTENSION: &str = "certs";
const INBOX_SOCKET_EXTENSION: &str = "sock";

/// The operations on directories that `Directories::establish` performs, supplied by the composition root.
pub trait DirectoryFilesystem {
    /// Creates the directory with mode 0700 and its missing parents, leaving an existing directory untouched.
    ///
    /// # Errors
    ///
    /// Fails when a directory cannot be created or a non-directory is in the way.
    fn create_private_directory(&self, path: &Path) -> io::Result<()>;

    /// Returns the numeric user id owning the directory.
    ///
    /// # Errors
    ///
    /// Fails when the directory cannot be inspected.
    fn owner_user_id(&self, path: &Path) -> io::Result<u32>;
}

/// The operating system's filesystem.
pub struct OperatingSystemFilesystem;

impl DirectoryFilesystem for OperatingSystemFilesystem {
    fn create_private_directory(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::DirBuilder::new()
            .mode(PRIVATE_DIRECTORY_MODE)
            .create(path)
        {
            Err(failure) if failure.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {
                Ok(())
            }
            outcome => outcome,
        }
    }

    fn owner_user_id(&self, path: &Path) -> io::Result<u32> {
        Ok(fs::metadata(path)?.uid())
    }
}

/// A directory could not be resolved or created.
#[derive(Debug)]
pub enum DirectoriesError {
    BaseUnresolvable { variable: &'static str },
    CreationFailed { path: PathBuf, source: io::Error },
    OwnerUnreadable { path: PathBuf, source: io::Error },
}

impl fmt::Display for DirectoriesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BaseUnresolvable { variable } => write!(
                formatter,
                "{variable} is unset or empty and {HOME_VARIABLE} is unset: set one of them"
            ),
            Self::CreationFailed { path, source } => {
                write!(formatter, "cannot create {}: {source}", path.display())
            }
            Self::OwnerUnreadable { path, source } => {
                write!(
                    formatter,
                    "cannot read the owner of {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for DirectoriesError {}

/// The three base directories and the cards directory of one run, and every path derived from them.
pub struct Directories {
    config: PathBuf,
    data: PathBuf,
    runtime: PathBuf,
    cards: PathBuf,
}

impl Directories {
    /// Resolves the base directories, creates them with mode 0700, and applies the cards directory override.
    ///
    /// # Errors
    ///
    /// Fails when a base cannot be resolved or created, or the owner of the config directory cannot be read.
    pub fn establish(
        environment: impl Fn(&str) -> Option<OsString>,
        filesystem: &impl DirectoryFilesystem,
        cards_directory_flag: Option<PathBuf>,
    ) -> Result<Self, DirectoriesError> {
        let config =
            home_based_directory(&environment, CONFIG_HOME_VARIABLE, CONFIG_HOME_UNDER_HOME)?
                .join(PRODUCT_DIRECTORY);
        let data = home_based_directory(&environment, DATA_HOME_VARIABLE, DATA_HOME_UNDER_HOME)?
            .join(PRODUCT_DIRECTORY);
        create_private_directory(filesystem, &config)?;
        create_private_directory(filesystem, &data)?;
        let user_id = filesystem.owner_user_id(&config).map_err(|source| {
            DirectoriesError::OwnerUnreadable {
                path: config.clone(),
                source,
            }
        })?;
        let runtime = runtime_parent(&environment).join(format!("{PRODUCT_DIRECTORY}-{user_id}"));
        create_private_directory(filesystem, &runtime)?;
        let cards = cards_directory_flag
            .or_else(|| non_empty_variable(&environment, CARDS_DIRECTORY_VARIABLE))
            .unwrap_or_else(|| data.join(CARDS_SUBDIRECTORY));
        Ok(Self {
            config,
            data,
            runtime,
            cards,
        })
    }

    /// The network configuration file of a network.
    #[must_use]
    pub fn network_config_file(&self, network: &str) -> PathBuf {
        self.config
            .join(NETWORKS_SUBDIRECTORY)
            .join(format!("{network}.{NETWORK_CONFIG_EXTENSION}"))
    }

    /// The file holding the owner secret key of a network.
    #[must_use]
    pub fn owner_key_file(&self, network: &str) -> PathBuf {
        self.config
            .join(NETWORKS_SUBDIRECTORY)
            .join(format!("{network}.{OWNER_KEY_EXTENSION}"))
    }

    /// The directory holding the cards of a network.
    #[must_use]
    pub fn cards_directory(&self, network: &str) -> PathBuf {
        self.cards.join(network)
    }

    /// The file of a card.
    #[must_use]
    pub fn card_file(&self, network: &str, card: &str) -> PathBuf {
        self.cards_directory(network)
            .join(format!("{card}.{CARD_EXTENSION}"))
    }

    /// The sidecar key file of a card.
    #[must_use]
    pub fn card_sidecar_file(&self, network: &str, card: &str) -> PathBuf {
        self.cards_directory(network)
            .join(format!("{card}.{KEY_EXTENSION}"))
    }

    /// The bootstrap node key file of this host for a network.
    #[must_use]
    pub fn infrastructure_key_file(&self, network: &str) -> PathBuf {
        self.infrastructure_file(network, KEY_EXTENSION)
    }

    /// The host certificate file of this host for a network.
    #[must_use]
    pub fn infrastructure_certificate_file(&self, network: &str) -> PathBuf {
        self.infrastructure_file(network, CERTIFICATE_EXTENSION)
    }

    /// The file holding the last record timestamp this host sent on a network.
    #[must_use]
    pub fn infrastructure_last_record_timestamp_file(&self, network: &str) -> PathBuf {
        self.infrastructure_file(network, LAST_RECORD_TIMESTAMP_EXTENSION)
    }

    /// The relay certificate cache directory of this host for a network.
    #[must_use]
    pub fn infrastructure_certificate_cache_directory(&self, network: &str) -> PathBuf {
        self.infrastructure_file(network, CERTIFICATE_CACHE_EXTENSION)
    }

    /// The inbox socket of the process with the given process id.
    #[must_use]
    pub fn inbox_socket_file(&self, process_id: u32) -> PathBuf {
        self.runtime
            .join(format!("{process_id}.{INBOX_SOCKET_EXTENSION}"))
    }

    fn infrastructure_file(&self, network: &str, extension: &str) -> PathBuf {
        self.data
            .join(INFRASTRUCTURE_SUBDIRECTORY)
            .join(format!("{network}.{extension}"))
    }
}

fn non_empty_variable(
    environment: &impl Fn(&str) -> Option<OsString>,
    name: &str,
) -> Option<PathBuf> {
    environment(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_based_directory(
    environment: &impl Fn(&str) -> Option<OsString>,
    variable: &'static str,
    under_home: &str,
) -> Result<PathBuf, DirectoriesError> {
    non_empty_variable(environment, variable)
        .or_else(|| {
            non_empty_variable(environment, HOME_VARIABLE).map(|home| home.join(under_home))
        })
        .ok_or(DirectoriesError::BaseUnresolvable { variable })
}

fn runtime_parent(environment: &impl Fn(&str) -> Option<OsString>) -> PathBuf {
    non_empty_variable(environment, RUNTIME_DIRECTORY_VARIABLE)
        .or_else(|| non_empty_variable(environment, TEMPORARY_DIRECTORY_VARIABLE))
        .unwrap_or_else(|| PathBuf::from(SYSTEM_TEMPORARY_DIRECTORY))
}

fn create_private_directory(
    filesystem: &impl DirectoryFilesystem,
    path: &Path,
) -> Result<(), DirectoriesError> {
    filesystem
        .create_private_directory(path)
        .map_err(|source| DirectoriesError::CreationFailed {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const FAKE_USER_ID: u32 = 4242;
    const SPEC_PRIVATE_DIRECTORY_MODE: u32 = 0o700;
    const EXISTING_DIRECTORY_MODE: u32 = 0o755;
    const MODE_BITS: u32 = 0o777;

    struct RecordingFilesystem {
        created: RefCell<Vec<PathBuf>>,
    }

    impl RecordingFilesystem {
        fn new() -> Self {
            Self {
                created: RefCell::new(Vec::new()),
            }
        }
    }

    impl DirectoryFilesystem for RecordingFilesystem {
        fn create_private_directory(&self, path: &Path) -> io::Result<()> {
            self.created.borrow_mut().push(path.to_path_buf());
            Ok(())
        }

        fn owner_user_id(&self, _path: &Path) -> io::Result<u32> {
            Ok(FAKE_USER_ID)
        }
    }

    fn environment_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| {
            owned
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    fn established(pairs: &[(&str, &str)]) -> (Directories, Vec<PathBuf>) {
        let filesystem = RecordingFilesystem::new();
        let directories = Directories::establish(environment_of(pairs), &filesystem, None).unwrap();
        (directories, filesystem.created.into_inner())
    }

    fn scratch_root(test_name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("estafette-dirs-{test_name}-{}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn scratch_environment(root: &Path) -> impl Fn(&str) -> Option<OsString> {
        let base = |name: &str| root.join(name).into_os_string();
        let config = base("config");
        let data = base("data");
        let runtime = base("runtime");
        move |name| match name {
            CONFIG_HOME_VARIABLE => Some(config.clone()),
            DATA_HOME_VARIABLE => Some(data.clone()),
            RUNTIME_DIRECTORY_VARIABLE => Some(runtime.clone()),
            _ => None,
        }
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & MODE_BITS
    }

    #[test]
    fn config_base_follows_xdg_config_home_spec_3_3() {
        let (directories, created) = established(&[
            ("XDG_CONFIG_HOME", "/xc"),
            ("XDG_DATA_HOME", "/xd"),
            ("XDG_RUNTIME_DIR", "/xr"),
            ("HOME", "/h"),
        ]);
        assert_eq!(
            directories.network_config_file("n"),
            Path::new("/xc/estafette/networks/n.toml")
        );
        assert!(created.contains(&PathBuf::from("/xc/estafette")));
    }

    #[test]
    fn config_base_falls_back_to_home_dot_config_spec_3_3() {
        let (directories, created) = established(&[("HOME", "/h")]);
        assert_eq!(
            directories.network_config_file("n"),
            Path::new("/h/.config/estafette/networks/n.toml")
        );
        assert!(created.contains(&PathBuf::from("/h/.config/estafette")));
    }

    #[test]
    fn data_base_follows_xdg_data_home_spec_3_3() {
        let (directories, created) =
            established(&[("XDG_DATA_HOME", "/xd"), ("XDG_CONFIG_HOME", "/xc")]);
        assert_eq!(
            directories.infrastructure_key_file("n"),
            Path::new("/xd/estafette/infra/n.key")
        );
        assert!(created.contains(&PathBuf::from("/xd/estafette")));
    }

    #[test]
    fn data_base_falls_back_to_home_local_share_spec_3_3() {
        let (directories, created) = established(&[("HOME", "/h")]);
        assert_eq!(
            directories.infrastructure_key_file("n"),
            Path::new("/h/.local/share/estafette/infra/n.key")
        );
        assert!(created.contains(&PathBuf::from("/h/.local/share/estafette")));
    }

    #[test]
    fn runtime_base_follows_xdg_runtime_dir_with_user_id_in_name_spec_3_3() {
        let (directories, created) =
            established(&[("XDG_RUNTIME_DIR", "/xr"), ("TMPDIR", "/t"), ("HOME", "/h")]);
        assert_eq!(
            directories.inbox_socket_file(7001),
            Path::new("/xr/estafette-4242/7001.sock")
        );
        assert!(created.contains(&PathBuf::from("/xr/estafette-4242")));
    }

    #[test]
    fn runtime_base_falls_back_to_tmpdir_spec_3_3() {
        let (directories, _) = established(&[("TMPDIR", "/t"), ("HOME", "/h")]);
        assert_eq!(
            directories.inbox_socket_file(7001),
            Path::new("/t/estafette-4242/7001.sock")
        );
    }

    #[test]
    fn runtime_base_falls_back_to_tmp_spec_3_3() {
        let (directories, created) = established(&[("HOME", "/h")]);
        assert_eq!(
            directories.inbox_socket_file(7001),
            Path::new("/tmp/estafette-4242/7001.sock")
        );
        assert!(created.contains(&PathBuf::from("/tmp/estafette-4242")));
    }

    #[test]
    fn empty_variables_count_as_unset_spec_3_3() {
        let (directories, _) = established(&[
            ("XDG_CONFIG_HOME", ""),
            ("XDG_DATA_HOME", ""),
            ("XDG_RUNTIME_DIR", ""),
            ("TMPDIR", ""),
            ("HOME", "/h"),
        ]);
        assert_eq!(
            directories.network_config_file("n"),
            Path::new("/h/.config/estafette/networks/n.toml")
        );
        assert_eq!(
            directories.infrastructure_key_file("n"),
            Path::new("/h/.local/share/estafette/infra/n.key")
        );
        assert_eq!(
            directories.inbox_socket_file(1),
            Path::new("/tmp/estafette-4242/1.sock")
        );
    }

    #[test]
    fn missing_home_with_no_xdg_variable_is_refused_naming_the_variable_spec_3_3() {
        let filesystem = RecordingFilesystem::new();
        let failure = Directories::establish(
            environment_of(&[("XDG_DATA_HOME", "/xd")]),
            &filesystem,
            None,
        )
        .err()
        .unwrap();
        assert_eq!(
            failure.to_string(),
            "XDG_CONFIG_HOME is unset or empty and HOME is unset: set one of them"
        );
        assert!(filesystem.created.borrow().is_empty());
    }

    #[test]
    fn every_file_path_of_spec_3_3_derives_from_the_bases() {
        let (directories, _) = established(&[
            ("XDG_CONFIG_HOME", "/c"),
            ("XDG_DATA_HOME", "/d"),
            ("XDG_RUNTIME_DIR", "/r"),
        ]);
        assert_eq!(
            directories.network_config_file("net"),
            Path::new("/c/estafette/networks/net.toml")
        );
        assert_eq!(
            directories.owner_key_file("net"),
            Path::new("/c/estafette/networks/net.owner.key")
        );
        assert_eq!(
            directories.cards_directory("net"),
            Path::new("/d/estafette/cards/net")
        );
        assert_eq!(
            directories.card_file("net", "alice"),
            Path::new("/d/estafette/cards/net/alice.yaml")
        );
        assert_eq!(
            directories.card_sidecar_file("net", "alice"),
            Path::new("/d/estafette/cards/net/alice.key")
        );
        assert_eq!(
            directories.infrastructure_key_file("net"),
            Path::new("/d/estafette/infra/net.key")
        );
        assert_eq!(
            directories.infrastructure_certificate_file("net"),
            Path::new("/d/estafette/infra/net.cert")
        );
        assert_eq!(
            directories.infrastructure_last_record_timestamp_file("net"),
            Path::new("/d/estafette/infra/net.ts")
        );
        assert_eq!(
            directories.infrastructure_certificate_cache_directory("net"),
            Path::new("/d/estafette/infra/net.certs")
        );
        assert_eq!(
            directories.inbox_socket_file(31337),
            Path::new("/r/estafette-4242/31337.sock")
        );
    }

    #[test]
    fn cards_directory_flag_overrides_environment_and_default_spec_3_3() {
        let filesystem = RecordingFilesystem::new();
        let directories = Directories::establish(
            environment_of(&[
                ("XDG_DATA_HOME", "/d"),
                ("ESTAFETTE_CARDS_DIR", "/from-environment"),
                ("HOME", "/h"),
            ]),
            &filesystem,
            Some(PathBuf::from("/from-flag")),
        )
        .unwrap();
        assert_eq!(
            directories.card_file("net", "alice"),
            Path::new("/from-flag/net/alice.yaml")
        );
        assert_eq!(
            directories.infrastructure_key_file("net"),
            Path::new("/d/estafette/infra/net.key")
        );
        assert!(
            !filesystem
                .created
                .borrow()
                .contains(&PathBuf::from("/from-flag"))
        );
    }

    #[test]
    fn cards_directory_environment_overrides_default_spec_3_3() {
        let (directories, _) = established(&[
            ("XDG_DATA_HOME", "/d"),
            ("ESTAFETTE_CARDS_DIR", "/from-environment"),
            ("HOME", "/h"),
        ]);
        assert_eq!(
            directories.card_sidecar_file("net", "alice"),
            Path::new("/from-environment/net/alice.key")
        );
    }

    #[test]
    fn fresh_directories_get_mode_0700_spec_3_3() {
        let root = scratch_root("fresh");
        let directories =
            Directories::establish(scratch_environment(&root), &OperatingSystemFilesystem, None)
                .unwrap();
        assert_eq!(
            mode_of(&root.join("config/estafette")),
            SPEC_PRIVATE_DIRECTORY_MODE
        );
        assert_eq!(
            mode_of(&root.join("data/estafette")),
            SPEC_PRIVATE_DIRECTORY_MODE
        );
        let runtime_directory = directories
            .inbox_socket_file(1)
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(mode_of(&runtime_directory), SPEC_PRIVATE_DIRECTORY_MODE);
        let user_id = OperatingSystemFilesystem
            .owner_user_id(&root.join("config/estafette"))
            .unwrap();
        assert_eq!(
            runtime_directory,
            root.join(format!("runtime/estafette-{user_id}"))
        );
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn existing_directory_keeps_its_mode_spec_3_3() {
        let root = scratch_root("existing");
        let existing = root.join("config/estafette");
        fs::create_dir_all(root.join("config")).unwrap();
        fs::DirBuilder::new().create(&existing).unwrap();
        fs::set_permissions(
            &existing,
            fs::Permissions::from_mode(EXISTING_DIRECTORY_MODE),
        )
        .unwrap();
        Directories::establish(scratch_environment(&root), &OperatingSystemFilesystem, None)
            .unwrap();
        assert_eq!(mode_of(&existing), EXISTING_DIRECTORY_MODE);
        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn a_file_in_place_of_a_directory_is_refused_naming_the_path_spec_3_3() {
        let root = scratch_root("blocked");
        fs::write(root.join("config"), b"").unwrap();
        let failure =
            Directories::establish(scratch_environment(&root), &OperatingSystemFilesystem, None)
                .err()
                .unwrap();
        assert!(failure.to_string().starts_with(&format!(
            "cannot create {}",
            root.join("config/estafette").display()
        )));
        drop(fs::remove_dir_all(&root));
    }
}
