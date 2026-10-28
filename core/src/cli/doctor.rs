use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use clap::Args;
use iroh::{PublicKey, RelayUrl};

use crate::card::{Card, CardInvalid};
use crate::cli::cards::CardRegistryReader;
use crate::cli::network::NetworkFilesystem;
use crate::cli::observer::{self, ObserverCommand, ObserverEffects, OperatorEffects};
use crate::config::{NetworkConfig, OWNER_ONLY_FILE_MODE};
use crate::dirs::{CARD_EXTENSION, Directories, DirectoriesError, OperatingSystemFilesystem};
use crate::errors::ExitStatus;
use crate::handle::{AnnouncedHandle, Handle, HandleError};
use crate::keys::{KeyError, parse_public_key};
use crate::node::{RelayProbe, known_host_addresses};
use crate::sidecar::{Pause, SidecarError, claim};

const PERMISSION_BITS_MASK: u32 = 0o777;
const RELAY_ANSWER_DEADLINE: Duration = Duration::from_secs(5);
const FIELD_SEPARATOR: char = '\t';
const LINE_END: &str = "\n";
const ITEM_SEPARATOR: &str = "; ";
const PASSED_WORD: &str = "pass";
const FAILED_WORD: &str = "fail";

/// The network whose node the doctor checks.
#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    pub network: Handle,
}

/// The reads the doctor performs beyond the card registry and the network files, supplied by the composition root.
pub trait DoctorFilesystem: CardRegistryReader + NetworkFilesystem {
    /// Returns the permission bits of the file, such as 0o600.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be inspected; `NotFound` when it does not exist.
    fn permission_bits(&self, path: &Path) -> io::Result<u32>;
}

impl DoctorFilesystem for OperatingSystemFilesystem {
    fn permission_bits(&self, path: &Path) -> io::Result<u32> {
        Ok(fs::metadata(path)?.permissions().mode() & PERMISSION_BITS_MASK)
    }
}

#[derive(Debug)]
enum Failure {
    Directories(DirectoriesError),
    Output(io::Error),
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directories(source) => write!(formatter, "{source}"),
            Self::Output(source) => write!(formatter, "cannot write the result: {source}"),
        }
    }
}

/// A doctor run that could not report, stated for whoever ran it.
#[derive(Debug)]
pub struct DoctorError(Failure);

impl fmt::Display for DoctorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for DoctorError {}

#[derive(Debug, Clone, Copy)]
enum Check {
    ConfigurationFile,
    Cards,
    Sidecars,
    KnownRelays,
    VisiblePeer,
}

impl Check {
    const fn name(self) -> &'static str {
        match self {
            Self::ConfigurationFile => "config",
            Self::Cards => "cards",
            Self::Sidecars => "sidecars",
            Self::KnownRelays => "relays",
            Self::VisiblePeer => "peers",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Passed(String),
    Failed(String),
}

impl Verdict {
    fn of_failures(passed_detail: String, failures: &[String]) -> Self {
        if failures.is_empty() {
            Self::Passed(passed_detail)
        } else {
            Self::Failed(failures.join(ITEM_SEPARATOR))
        }
    }
}

/// Prints each check as one line the moment it is decided, and remembers whether any failed.
struct Report<'stream, Stream: Write> {
    stdout: &'stream mut Stream,
    any_failed: bool,
}

impl<'stream, Stream: Write> Report<'stream, Stream> {
    const fn new(stdout: &'stream mut Stream) -> Self {
        Self {
            stdout,
            any_failed: false,
        }
    }

    fn record(&mut self, check: Check, verdict: &Verdict) -> Result<(), Failure> {
        let (word, detail) = match verdict {
            Verdict::Passed(detail) => (PASSED_WORD, detail),
            Verdict::Failed(detail) => {
                self.any_failed = true;
                (FAILED_WORD, detail)
            }
        };
        let line = [check.name(), word, &detail.escape_debug().to_string()]
            .join(&FIELD_SEPARATOR.to_string())
            + LINE_END;
        self.stdout
            .write_all(line.as_bytes())
            .and_then(|()| self.stdout.flush())
            .map_err(Failure::Output)
    }

    const fn exit_status(&self) -> ExitStatus {
        if self.any_failed {
            ExitStatus::DoctorFoundFailedCheck
        } else {
            ExitStatus::Success
        }
    }
}

/// Runs the five checks of spec 3.13, printing one line per check and returning the exit status.
///
/// # Errors
///
/// Returns [`DoctorError`] when the network name or the result stream refuses; a failed check is a line and an exit status, not an error.
pub async fn run(
    arguments: &DoctorArgs,
    directories: &Directories,
    filesystem: &(impl DoctorFilesystem + Sync),
    pause: &(impl Pause + Sync),
    effects: ObserverEffects,
    operator_effects: OperatorEffects,
    stdout: &mut impl Write,
) -> Result<ExitStatus, DoctorError> {
    run_checks(
        arguments,
        directories,
        filesystem,
        pause,
        (effects, operator_effects),
        stdout,
    )
    .await
    .map_err(DoctorError)
}

async fn run_checks(
    arguments: &DoctorArgs,
    directories: &Directories,
    filesystem: &(impl DoctorFilesystem + Sync),
    pause: &(impl Pause + Sync),
    (effects, operator_effects): (ObserverEffects, OperatorEffects),
    stdout: &mut impl Write,
) -> Result<ExitStatus, Failure> {
    let network = arguments.network.as_str();
    let config_path = directories
        .network_config_file(network)
        .map_err(Failure::Directories)?;
    let cards_directory = directories
        .cards_directory(network)
        .map_err(Failure::Directories)?;
    let probe_relay = operator_effects.probe_relay.clone();
    let mut report = Report::new(stdout);
    report.record(
        Check::ConfigurationFile,
        &configuration_file_verdict(filesystem, &config_path),
    )?;
    let registry = read_registry(filesystem, &cards_directory);
    report.record(Check::Cards, &cards_verdict(&registry))?;
    report.record(
        Check::Sidecars,
        &sidecars_verdict(directories, network, pause, &registry)?,
    )?;
    report.record(
        Check::KnownRelays,
        &known_relays_verdict(&config_path, &probe_relay).await,
    )?;
    let visible_peer = visible_peer_verdict(
        arguments,
        directories,
        filesystem,
        pause,
        (effects, operator_effects),
    )
    .await;
    report.record(Check::VisiblePeer, &visible_peer)?;
    Ok(report.exit_status())
}

fn configuration_file_verdict(filesystem: &impl DoctorFilesystem, path: &Path) -> Verdict {
    match filesystem.permission_bits(path) {
        Ok(OWNER_ONLY_FILE_MODE) => Verdict::Passed(format!(
            "{} has permissions {OWNER_ONLY_FILE_MODE:04o}",
            path.display()
        )),
        Ok(bits) => Verdict::Failed(format!(
            "{} has permissions {bits:04o}; only {OWNER_ONLY_FILE_MODE:04o} is accepted",
            path.display()
        )),
        Err(source) => Verdict::Failed(format!("{}: {source}", path.display())),
    }
}

/// A card file of the registry, read and validated.
struct RegistryCard {
    file: PathBuf,
    validated: Result<ValidCard, CardProblem>,
}

struct ValidCard {
    name: Handle,
    public_key: PublicKey,
}

#[derive(Debug)]
enum CardProblem {
    NameRefused(HandleError),
    Unreadable(io::Error),
    Invalid(CardInvalid),
    HandleRefused(HandleError),
    KeyRefused(KeyError),
}

impl fmt::Display for CardProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameRefused(source) => write!(formatter, "not a valid card name: {source}"),
            Self::Unreadable(source) => write!(formatter, "cannot be read: {source}"),
            Self::Invalid(source) => write!(formatter, "not a valid card: {source}"),
            Self::HandleRefused(source) => {
                write!(formatter, "holds a handle that is not usable: {source}")
            }
            Self::KeyRefused(source) => {
                write!(formatter, "holds a public key that is not usable: {source}")
            }
        }
    }
}

struct RegistryUnreadable {
    directory: PathBuf,
    source: io::Error,
}

impl fmt::Display for RegistryUnreadable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.directory.display(), self.source)
    }
}

fn read_registry(
    filesystem: &impl CardRegistryReader,
    cards_directory: &Path,
) -> Result<Vec<RegistryCard>, RegistryUnreadable> {
    let entries = match filesystem.read_directory(cards_directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(RegistryUnreadable {
                directory: cards_directory.to_owned(),
                source,
            });
        }
    };
    let mut cards: Vec<RegistryCard> = entries
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == CARD_EXTENSION)
        })
        .map(|file| RegistryCard {
            validated: validate_card_file(filesystem, &file),
            file,
        })
        .collect();
    cards.sort_by(|first, second| first.file.cmp(&second.file));
    Ok(cards)
}

fn validate_card_file(
    filesystem: &impl CardRegistryReader,
    file: &Path,
) -> Result<ValidCard, CardProblem> {
    let name = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or(HandleError::NotAHandle)
        .and_then(Handle::from_str)
        .map_err(CardProblem::NameRefused)?;
    let text = filesystem
        .read_text(file)
        .map_err(CardProblem::Unreadable)?;
    let card = Card::from_str(&text).map_err(CardProblem::Invalid)?;
    card.handle
        .parse::<AnnouncedHandle>()
        .map_err(CardProblem::HandleRefused)?;
    let public_key = parse_public_key(&card.public_key).map_err(CardProblem::KeyRefused)?;
    Ok(ValidCard { name, public_key })
}

fn cards_verdict(registry: &Result<Vec<RegistryCard>, RegistryUnreadable>) -> Verdict {
    match registry {
        Ok(cards) => {
            let failures: Vec<String> = cards
                .iter()
                .filter_map(|card| {
                    card.validated
                        .as_ref()
                        .err()
                        .map(|problem| format!("{} is {problem}", card.file.display()))
                })
                .collect();
            Verdict::of_failures(format!("cards valid: {}", cards.len()), &failures)
        }
        Err(unreadable) => Verdict::Failed(unreadable.to_string()),
    }
}

fn sidecars_verdict(
    directories: &Directories,
    network: &str,
    pause: &impl Pause,
    registry: &Result<Vec<RegistryCard>, RegistryUnreadable>,
) -> Result<Verdict, Failure> {
    let Ok(cards) = registry else {
        return Ok(Verdict::Failed(String::from(
            "the card registry cannot be read",
        )));
    };
    let valid_cards: Vec<&ValidCard> = cards
        .iter()
        .filter_map(|card| card.validated.as_ref().ok())
        .collect();
    let mut failures = Vec::new();
    for valid in &valid_cards {
        let sidecar = directories
            .card_sidecar_file(network, valid.name.as_str())
            .map_err(Failure::Directories)?;
        match claim(&sidecar, valid.public_key.as_bytes(), pause) {
            Ok(_) | Err(SidecarError::CardHeld) => {}
            Err(problem) => failures.push(format!("{}: {problem}", sidecar.display())),
        }
    }
    Ok(Verdict::of_failures(
        format!("sidecars matching: {}", valid_cards.len()),
        &failures,
    ))
}

async fn known_relays_verdict(config_path: &Path, probe_relay: &RelayProbe) -> Verdict {
    let relays = match known_relays(config_path) {
        Ok(relays) => relays,
        Err(refusal) => return Verdict::Failed(refusal),
    };
    let mut silent = Vec::new();
    for relay in &relays {
        let answers = tokio::time::timeout(RELAY_ANSWER_DEADLINE, probe_relay(relay.clone())).await;
        if !answers.unwrap_or(false) {
            silent.push(format!("{relay} does not answer"));
        }
    }
    Verdict::of_failures(format!("relays answering: {}", relays.len()), &silent)
}

fn known_relays(config_path: &Path) -> Result<Vec<RelayUrl>, String> {
    let config = NetworkConfig::load(config_path).map_err(|source| source.to_string())?;
    let hosts = known_host_addresses(&config).map_err(|source| source.to_string())?;
    Ok(hosts.into_iter().map(|host| host.relay).collect())
}

async fn visible_peer_verdict(
    arguments: &DoctorArgs,
    directories: &Directories,
    filesystem: &(impl DoctorFilesystem + Sync),
    pause: &(impl Pause + Sync),
    (effects, operator_effects): (ObserverEffects, OperatorEffects),
) -> Verdict {
    let mut listing = Vec::new();
    let command = ObserverCommand::ShowPeers {
        network: arguments.network.clone(),
    };
    let observed = observer::run(
        &command,
        directories,
        filesystem,
        pause,
        effects,
        operator_effects,
        &mut listing,
    )
    .await;
    match observed {
        Ok(()) => peers_listed_verdict(&listing),
        Err(refusal) => Verdict::Failed(refusal.to_string()),
    }
}

fn peers_listed_verdict(listing: &[u8]) -> Verdict {
    let peers = String::from_utf8_lossy(listing).lines().count();
    if peers == 0 {
        Verdict::Failed(String::from("the observer saw no peer"))
    } else {
        Verdict::Passed(format!("peers seen by the observer: {peers}"))
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use iroh::{EndpointAddr, SecretKey};
    use tokio::time::Instant;

    use super::*;
    use crate::cli::observer::{ObserverEffects, OperatorEffects};
    use crate::config::{Host, HostOrigin, NetworkConfig};
    use crate::crypto::{KEY_LEN, NetworkKeys, NetworkSecret};
    use crate::handle::AnnouncedHandle;
    use crate::inbox::socket::{InboxSocket, ProcessTable};
    use crate::keys::render_public_key;
    use crate::net::endpoint::NodeEndpoint;
    use crate::node::{Identity, Network, Node, NodeEffects, NodeRecord};
    use crate::sidecar::{SEED_BYTES, create_card};
    use crate::wire::Nonce;

    const NETWORK_NAME: &str = "lab";
    const NETWORK_SECRET: [u8; KEY_LEN] = [33; KEY_LEN];
    const OWNER_SEED: u8 = 60;
    const MEMBER_SEED: u8 = 61;
    const OBSERVER_SEED: u8 = 70;
    const MEMBER_HANDLE: &str = "amy";
    const MEMBER_PUBLIC: &[u8] = br#"{"role":"member"}"#;
    const LIVE_RELAY: &str = "https://relay.estafette.invalid/";
    const PROBE_NONCE: Nonce = [7; 16];
    const LOOSE_CONFIG_MODE: u32 = 0o644;
    const CARD_NAME: &str = "zed";
    const CARD_SEED: [u8; SEED_BYTES] = [90; SEED_BYTES];
    const OTHER_CARD_NAME: &str = "ned";
    const OTHER_CARD_SEED: [u8; SEED_BYTES] = [91; SEED_BYTES];
    const UNRELATED_SEED: [u8; SEED_BYTES] = [92; SEED_BYTES];

    struct EveryProcessRuns;

    impl ProcessTable for EveryProcessRuns {
        fn is_running(&self, _process_id: u32) -> io::Result<bool> {
            Ok(true)
        }
    }

    struct NoPause;

    impl Pause for NoPause {
        fn pause(&self, _duration: Duration) {}
    }

    static LAB_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

    struct Lab {
        root: PathBuf,
        directories: Arc<Directories>,
        member: Option<(Node, EndpointAddr)>,
        _slot: tokio::sync::SemaphorePermit<'static>,
    }

    fn secret_key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; KEY_LEN])
    }

    fn wall_clock_ms() -> u64 {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap()
    }

    fn network_keys() -> NetworkKeys {
        NetworkKeys::derive(&NetworkSecret::from_bytes(NETWORK_SECRET))
    }

    fn network_config_with_member_host() -> NetworkConfig {
        NetworkConfig {
            secret: data_encoding::HEXLOWER.encode(&NETWORK_SECRET),
            owner: render_public_key(&secret_key(OWNER_SEED).public()),
            hosts: vec![Host {
                id: render_public_key(&secret_key(MEMBER_SEED).public()),
                relay: LIVE_RELAY.to_owned(),
                ts: None,
                origin: HostOrigin::Configured,
            }],
        }
    }

    async fn bind_endpoint(seed: u8) -> NodeEndpoint {
        NodeEndpoint::bind(
            secret_key(seed),
            *network_keys().gate_key(),
            Vec::new(),
            |nonce: &mut [u8; KEY_LEN]| *nonce = [OBSERVER_SEED; KEY_LEN],
            |_ignored| {},
        )
        .await
        .unwrap()
    }

    impl Lab {
        async fn healthy(tag: &str) -> Self {
            Self::without_peers(tag).await.start_member().await
        }

        async fn without_peers(tag: &str) -> Self {
            let slot = LAB_SLOT.acquire().await.unwrap();
            let root = std::env::temp_dir().join(format!("u28-{tag}-{}", std::process::id()));
            drop(fs::remove_dir_all(&root));
            fs::create_dir_all(&root).unwrap();
            let base = root.clone();
            let environment = move |name: &str| match name {
                "XDG_CONFIG_HOME" => Some(base.join("c").into_os_string()),
                "XDG_DATA_HOME" => Some(base.join("d").into_os_string()),
                "XDG_RUNTIME_DIR" => Some(base.join("r").into_os_string()),
                _ => None::<OsString>,
            };
            let directories =
                Directories::establish(environment, &OperatingSystemFilesystem, None).unwrap();
            let lab = Self {
                root,
                directories: Arc::new(directories),
                member: None,
                _slot: slot,
            };
            lab.write_config();
            lab.write_card(CARD_NAME, CARD_SEED);
            lab
        }

        fn config_path(&self) -> PathBuf {
            self.directories.network_config_file(NETWORK_NAME).unwrap()
        }

        fn write_config(&self) {
            let path = self.config_path();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            network_config_with_member_host()
                .rewrite(&path, wall_clock_ms() / 1000)
                .unwrap();
        }

        fn write_card(&self, name: &str, seed: [u8; SEED_BYTES]) {
            let cards = self.directories.cards_directory(NETWORK_NAME).unwrap();
            fs::create_dir_all(&cards).unwrap();
            create_card(
                &self.directories.card_file(NETWORK_NAME, name).unwrap(),
                &self
                    .directories
                    .card_sidecar_file(NETWORK_NAME, name)
                    .unwrap(),
                name,
                seed,
            )
            .unwrap();
        }

        async fn start_member(mut self) -> Self {
            let endpoint = bind_endpoint(MEMBER_SEED).await;
            let address = endpoint.endpoint().addr();
            endpoint.replace_address(address.clone());
            let inbox = Arc::new(
                InboxSocket::start(&self.directories, u32::from(MEMBER_SEED), &EveryProcessRuns)
                    .unwrap(),
            );
            let node = Node::start(
                endpoint,
                Identity {
                    card_key: secret_key(MEMBER_SEED),
                    requested_handle: MEMBER_HANDLE.parse::<AnnouncedHandle>().unwrap(),
                    public: MEMBER_PUBLIC.to_vec(),
                },
                Network::new(
                    NetworkConfig {
                        hosts: Vec::new(),
                        ..network_config_with_member_host()
                    },
                    self.root.join("member-network.toml"),
                )
                .unwrap(),
                NodeEffects {
                    clock: Arc::new(wall_clock_ms),
                    monotonic: Arc::new(Instant::now),
                    fill_probe_nonce: Arc::new(|nonce: &mut Nonce| *nonce = PROBE_NONCE),
                    report: Arc::new(|_record: NodeRecord| {}),
                    inbox,
                    probe_relay: Arc::new(|_relay| Box::pin(async { true })),
                },
            )
            .await
            .unwrap();
            self.member = Some((node, address));
            self
        }

        fn effects(&self) -> (ObserverEffects, OperatorEffects) {
            let member_addresses: Vec<EndpointAddr> = self
                .member
                .iter()
                .map(|(_node, address)| address.clone())
                .collect();
            let directories = Arc::clone(&self.directories);
            let effects = ObserverEffects {
                clock: Arc::new(wall_clock_ms),
                monotonic: Arc::new(Instant::now),
                fill_seed: Arc::new(|seed| *seed = [OBSERVER_SEED; SEED_BYTES]),
                bind_endpoint: Box::new(move |key, gate_key, relays| {
                    Box::pin(async move {
                        let endpoint = NodeEndpoint::bind(
                            key,
                            gate_key,
                            relays,
                            |nonce: &mut [u8; KEY_LEN]| *nonce = [OBSERVER_SEED; KEY_LEN],
                            |_ignored| {},
                        )
                        .await?;
                        for address in member_addresses {
                            endpoint.replace_address(address);
                        }
                        Ok(endpoint)
                    })
                }),
                interrupted: Box::pin(std::future::pending()),
            };
            let operator_effects = OperatorEffects {
                fill_probe_nonce: Arc::new(|nonce: &mut Nonce| *nonce = PROBE_NONCE),
                report: Arc::new(|_record: NodeRecord| {}),
                probe_relay: Arc::new(|_relay| Box::pin(async { true })),
                start_inbox: Box::new(move || {
                    InboxSocket::start(&directories, u32::from(OBSERVER_SEED), &EveryProcessRuns)
                        .map(Arc::new)
                }),
            };
            (effects, operator_effects)
        }

        async fn diagnose(&self) -> (ExitStatus, Vec<String>) {
            let (effects, operator_effects) = self.effects();
            self.diagnose_with(effects, operator_effects).await
        }

        async fn diagnose_with(
            &self,
            effects: ObserverEffects,
            operator_effects: OperatorEffects,
        ) -> (ExitStatus, Vec<String>) {
            let mut output = Vec::new();
            let status = run(
                &DoctorArgs {
                    network: NETWORK_NAME.parse().unwrap(),
                },
                &self.directories,
                &OperatingSystemFilesystem,
                &NoPause,
                effects,
                operator_effects,
                &mut output,
            )
            .await
            .unwrap();
            let lines = String::from_utf8(output)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect();
            (status, lines)
        }
    }

    impl Drop for Lab {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.root));
        }
    }

    fn line_of_check<'a>(lines: &'a [String], check: &str) -> &'a str {
        lines
            .iter()
            .find(|line| line.starts_with(&format!("{check}\t")))
            .unwrap_or_else(|| panic!("no line for the {check} check in {lines:?}"))
    }

    #[tokio::test]
    async fn spec_3_13_doctor_reports_a_config_file_with_permissions_0644_as_failed_and_exits_3() {
        let lab = Lab::healthy("loose-config").await;
        fs::set_permissions(
            lab.config_path(),
            fs::Permissions::from_mode(LOOSE_CONFIG_MODE),
        )
        .unwrap();

        let (status, lines) = lab.diagnose().await;
        drop(lab);

        assert_eq!(status, ExitStatus::DoctorFoundFailedCheck);
        assert!(
            line_of_check(&lines, "config").starts_with("config\tfail\t"),
            "{lines:?}"
        );
        assert!(line_of_check(&lines, "config").contains("0644"));
    }

    fn detail_of_check<'a>(lines: &'a [String], check: &str) -> &'a str {
        line_of_check(lines, check)
            .split(FIELD_SEPARATOR)
            .nth(2)
            .unwrap()
    }

    fn verdict_word_of_check<'a>(lines: &'a [String], check: &str) -> &'a str {
        line_of_check(lines, check)
            .split(FIELD_SEPARATOR)
            .nth(1)
            .unwrap()
    }

    #[tokio::test]
    async fn spec_3_13_doctor_prints_one_pass_line_per_check_in_order_and_exits_0_on_a_healthy_network()
     {
        let lab = Lab::healthy("healthy").await;

        let (status, lines) = lab.diagnose().await;
        drop(lab);

        assert_eq!(status, ExitStatus::Success);
        let checks: Vec<&str> = lines
            .iter()
            .map(|line| line.split(FIELD_SEPARATOR).next().unwrap())
            .collect();
        assert_eq!(checks, ["config", "cards", "sidecars", "relays", "peers"]);
        for check in checks {
            assert_eq!(verdict_word_of_check(&lines, check), "pass", "{lines:?}");
        }
        assert_eq!(detail_of_check(&lines, "cards"), "cards valid: 1");
        assert_eq!(detail_of_check(&lines, "sidecars"), "sidecars matching: 1");
        assert_eq!(detail_of_check(&lines, "relays"), "relays answering: 1");
        assert_eq!(
            detail_of_check(&lines, "peers"),
            "peers seen by the observer: 1"
        );
    }

    #[tokio::test]
    async fn spec_3_13_doctor_fails_the_cards_check_naming_the_invalid_card_file_and_exits_3() {
        let lab = Lab::healthy("invalid-card").await;
        let invalid_card = lab
            .directories
            .card_file(NETWORK_NAME, OTHER_CARD_NAME)
            .unwrap();
        fs::write(&invalid_card, "handle: [unclosed").unwrap();

        let (status, lines) = lab.diagnose().await;
        drop(lab);

        assert_eq!(status, ExitStatus::DoctorFoundFailedCheck);
        assert_eq!(verdict_word_of_check(&lines, "cards"), "fail");
        assert!(
            detail_of_check(&lines, "cards").contains(&format!("{OTHER_CARD_NAME}.yaml")),
            "{lines:?}"
        );
        assert_eq!(detail_of_check(&lines, "sidecars"), "sidecars matching: 1");
    }

    #[tokio::test]
    async fn spec_3_13_doctor_fails_the_sidecars_check_when_a_sidecar_belongs_to_another_key_and_exits_3()
     {
        let lab = Lab::healthy("mismatched-sidecar").await;
        lab.write_card(OTHER_CARD_NAME, OTHER_CARD_SEED);
        let sidecar = lab
            .directories
            .card_sidecar_file(NETWORK_NAME, OTHER_CARD_NAME)
            .unwrap();
        fs::write(
            &sidecar,
            format!("{}\n", data_encoding::HEXLOWER.encode(&UNRELATED_SEED)),
        )
        .unwrap();

        let (status, lines) = lab.diagnose().await;
        drop(lab);

        assert_eq!(status, ExitStatus::DoctorFoundFailedCheck);
        assert_eq!(verdict_word_of_check(&lines, "cards"), "pass");
        assert_eq!(verdict_word_of_check(&lines, "sidecars"), "fail");
        assert!(
            detail_of_check(&lines, "sidecars").contains(&format!("{OTHER_CARD_NAME}.key")),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn spec_3_13_doctor_counts_a_sidecar_held_by_a_running_session_as_matching() {
        let lab = Lab::healthy("held-sidecar").await;
        let sidecar = lab
            .directories
            .card_sidecar_file(NETWORK_NAME, CARD_NAME)
            .unwrap();
        let public_key = SecretKey::from_bytes(&CARD_SEED).public();
        let _session = claim(&sidecar, public_key.as_bytes(), &NoPause).unwrap();

        let (status, lines) = lab.diagnose().await;
        drop(lab);

        assert_eq!(status, ExitStatus::Success);
        assert_eq!(detail_of_check(&lines, "sidecars"), "sidecars matching: 1");
    }

    #[tokio::test]
    async fn spec_3_13_doctor_fails_the_relays_check_naming_a_relay_that_does_not_answer_and_exits_3()
     {
        let lab = Lab::healthy("silent-relay").await;
        let (effects, mut operator_effects) = lab.effects();
        operator_effects.probe_relay = Arc::new(|_relay| Box::pin(async { false }));

        let (status, lines) = lab.diagnose_with(effects, operator_effects).await;
        drop(lab);

        assert_eq!(status, ExitStatus::DoctorFoundFailedCheck);
        assert_eq!(verdict_word_of_check(&lines, "relays"), "fail");
        assert!(detail_of_check(&lines, "relays").contains(LIVE_RELAY));
        assert_eq!(verdict_word_of_check(&lines, "peers"), "pass");
    }

    #[tokio::test]
    async fn spec_3_13_doctor_fails_the_peers_check_when_the_observer_sees_no_peer_and_exits_3() {
        let lab = Lab::without_peers("no-peer").await;

        let (status, lines) = lab.diagnose().await;
        drop(lab);

        assert_eq!(status, ExitStatus::DoctorFoundFailedCheck);
        assert_eq!(verdict_word_of_check(&lines, "peers"), "fail");
        assert_eq!(verdict_word_of_check(&lines, "config"), "pass");
    }

    #[tokio::test]
    async fn spec_3_13_doctor_fails_the_config_check_for_a_network_without_a_configuration_file() {
        let lab = Lab::without_peers("no-config").await;
        fs::remove_file(lab.config_path()).unwrap();

        let (status, lines) = lab.diagnose().await;
        drop(lab);

        assert_eq!(status, ExitStatus::DoctorFoundFailedCheck);
        assert_eq!(verdict_word_of_check(&lines, "config"), "fail");
        assert_eq!(verdict_word_of_check(&lines, "relays"), "fail");
        assert_eq!(verdict_word_of_check(&lines, "peers"), "fail");
    }
}
