use std::fmt;
use std::fs::{self, OpenOptions, Permissions};
use std::future::Future;
use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use clap::Subcommand;
use data_encoding::HEXLOWER;
use iroh::endpoint::BindError;
use iroh::protocol::Router;
use iroh::{RelayUrl, SecretKey, Signature};
use iroh_gossip::{ALPN as GOSSIP_ALPN, Gossip};
use tokio::time::{Instant, Interval, MissedTickBehavior, interval};

use super::network::NetworkFilesystem;
use crate::config::{ConfigError, NetworkConfig, OWNER_ONLY_FILE_MODE};
use crate::crypto::{GateKey, HOST_CERTIFICATE_HEX_CHARS, KEY_LEN, verify_host_certificate};
use crate::dirs::{Directories, DirectoriesError, OperatingSystemFilesystem};
use crate::handle::Handle;
use crate::invite::{InviteError, render_host_line};
use crate::keys::{KeyError, generate_secret_key, parse_secret_key, render_secret_key};
use crate::net::direct::{DirectHost, DirectServer};
use crate::net::endpoint::{DIRECT_ALPN, NodeEndpoint};
use crate::net::gossip::{GossipError, GossipEvent, NetworkTopic};
use crate::node::{
    KnownHostAddress, KnownHosts, Network, NodeError, NodeRecord, RecordVerdict, endpoint_addr,
    endpoint_id, known_host_addresses, seed_lookup_with_known_hosts,
};
use crate::payload::ZERO_MAC;
use crate::wire::{
    self, Addr, HostAddr, Infrastructure, IntroduceAck, MAX_RELAY_URL_BYTES, Message, Payload,
    SignatureBytes, Version,
};

const HTTPS_PORT: u16 = 443;
const ACME_CHALLENGE_PORT: u16 = 80;
const QUIC_ADDRESS_DISCOVERY_PORT: u16 = 7842;
const INFRASTRUCTURE_RECORD_INTERVAL: Duration = Duration::from_secs(5);
const LOST_RECORD_COUNTER: u64 = 0;
const MILLIS_PER_SECOND: u64 = 1000;
const RELAY_URL_SCHEME_PREFIX: &str = "https://";
const RELAY_URL_ROOT_PATH: &str = "/";
const LINE_END: char = '\n';
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const TEMPORARY_FILE_EXTENSION: &str = "tmp";
const UNSIGNED: SignatureBytes = SignatureBytes([0; Signature::LENGTH]);
const RUN_ABOUT: &str = "Run this host's relay and bootstrap node for a network";

fn run_long_about() -> String {
    format!(
        "Run this host's embedded relay and bootstrap node for a network until interrupted.\n\nReads: the network configuration file; the host key, created on the first run; the host certificate, written from the output of certify host; the last record counter.\nProduces: the host address line on stdout at start; HTTPS on TCP {HTTPS_PORT}, the ACME challenge on TCP {ACME_CHALLENGE_PORT} and QUIC address discovery on UDP {QUIC_ADDRESS_DISCOVERY_PORT} of the bind address, with a Let's Encrypt certificate for the domain; this host's signed record on the network when it joins and every {} seconds after; the hosts it learns, saved to the configuration; the run record on stderr.\nRefuses: a network with no configuration file here; a missing certificate, or one the owner key did not sign for this host, after printing the host address line; a domain {RelayDomainRefused}.",
        INFRASTRUCTURE_RECORD_INTERVAL.as_secs()
    )
}

/// A cryptographically secure source of 32 random bytes.
pub type FillFromSecureSource = Box<dyn FnOnce(&mut [u8; KEY_LEN]) + Send>;

/// Milliseconds since the Unix epoch by this machine's clock.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// The run record, written to standard error by the command-line layer; it must not block.
pub type RunRecord = Arc<dyn Fn(NodeRecord) + Send + Sync>;

/// Resolves when the process is to stop.
pub type StopRequested = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Binds the host's endpoint under its key and the network's gate key, with its own relay alone in the relay map.
pub type BindHostEndpoint = Box<
    dyn FnOnce(
            SecretKey,
            GateKey,
            RelayUrl,
        ) -> Pin<Box<dyn Future<Output = Result<NodeEndpoint, BindError>> + Send>>
        + Send,
>;

/// What keeps the embedded relay serving until it is dropped.
pub type RunningRelay = Box<dyn std::any::Any + Send>;

/// Why the embedded relay did not start, as the relay library states it.
pub type RelayNotStarted = Box<dyn std::error::Error + Send + Sync>;

/// Starts the embedded relay at the site and returns what keeps it serving.
pub type StartRelay = Box<
    dyn FnOnce(
            RelaySite,
        )
            -> Pin<Box<dyn Future<Output = Result<RunningRelay, RelayNotStarted>> + Send>>
        + Send,
>;

/// The infrastructure command of spec 3.13.
#[derive(Debug, Clone, Subcommand)]
pub enum InfrastructureCommand {
    #[command(about = RUN_ABOUT, long_about = run_long_about())]
    RunInfrastructure {
        network: Handle,
        bind_address: IpAddr,
        #[arg(value_parser = parse_relay_domain)]
        domain: RelayDomain,
    },
}

/// The domain name of this host's relay and the relay URL `https://<domain>/` it gives, within the wire's relay URL limit.
#[derive(Debug, Clone)]
pub struct RelayDomain {
    name: String,
    url: RelayUrl,
    wire_url: wire::RelayUrl,
}

/// A domain name refused because it is not the whole host of its relay URL or that URL is over the wire's limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayDomainRefused;

impl fmt::Display for RelayDomainRefused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "must be a lowercase host name with no port or path whose relay URL {RELAY_URL_SCHEME_PREFIX}<domain>{RELAY_URL_ROOT_PATH} is at most {MAX_RELAY_URL_BYTES} bytes"
        )
    }
}

impl std::error::Error for RelayDomainRefused {}

/// Where the embedded relay serves: HTTPS, the ACME challenge and QUIC address discovery on the bind address, for the domain, caching its certificate in the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySite {
    pub https: SocketAddr,
    pub acme_challenge: SocketAddr,
    pub quic_address_discovery: SocketAddr,
    pub domain: String,
    pub certificate_cache: PathBuf,
}

/// The side effects the infrastructure command runs on, initialized by the composition root.
pub struct InfrastructureEffects {
    /// Makes the host key on the first run.
    pub fill_from_secure_source: FillFromSecureSource,
    /// Stamps each record counter and each host learned.
    pub clock: Clock,
    /// Binds the endpoint, with the gate's nonce source and closing sink the composition root chose.
    pub bind_endpoint: BindHostEndpoint,
    /// Starts the embedded relay.
    pub start_relay: StartRelay,
    /// Ends the run.
    pub stopped: StopRequested,
    /// Receives every failure met while serving.
    pub report: RunRecord,
}

/// The file operations of the infrastructure command beyond those of the network commands, supplied by the composition root.
pub trait InfrastructureFilesystem: NetworkFilesystem + Sync {
    /// Writes the contents to a new file readable by its owner alone and renames it over the path.
    ///
    /// # Errors
    ///
    /// Fails with the operating system's error when the file cannot be written or renamed.
    fn replace_owner_only_file(&self, path: &Path, contents: &str) -> io::Result<()>;

    /// Creates the directory and its missing parents, and sets its mode to 0700 whatever mode it had.
    ///
    /// # Errors
    ///
    /// Fails with the operating system's error when the directory cannot be created or its mode set.
    fn restrict_directory_to_owner(&self, path: &Path) -> io::Result<()>;
}

impl InfrastructureFilesystem for OperatingSystemFilesystem {
    fn replace_owner_only_file(&self, path: &Path, contents: &str) -> io::Result<()> {
        let temporary = path.with_added_extension(TEMPORARY_FILE_EXTENSION);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(OWNER_ONLY_FILE_MODE)
            .open(&temporary)?;
        file.set_permissions(Permissions::from_mode(OWNER_ONLY_FILE_MODE))?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    }

    fn restrict_directory_to_owner(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)?;
        fs::set_permissions(path, Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
    }
}

#[derive(Debug)]
enum Failure {
    Directories(DirectoriesError),
    NetworkNotConfigured { network: String },
    Config(ConfigError),
    Node(NodeError),
    Io { path: PathBuf, source: io::Error },
    Output(io::Error),
    HostKeyUnreadable { path: PathBuf, source: KeyError },
    HostLine(InviteError),
    CertificateMissing { path: PathBuf },
    CertificateMalformed { path: PathBuf },
    CertificateNotForThisHost { path: PathBuf },
    LastRecordUnreadable { path: PathBuf },
    RelayNotStarted(RelayNotStarted),
    EndpointNotBound(BindError),
    TopicClosed,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directories(source) => write!(formatter, "{source}"),
            Self::NetworkNotConfigured { network } => write!(
                formatter,
                "network {network} has no configuration file here: copy it from the owner's machine, never with the owner key file"
            ),
            Self::Config(source) => write!(formatter, "{source}"),
            Self::Node(source) => write!(formatter, "{source}"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Output(source) => {
                write!(formatter, "cannot write the host address line: {source}")
            }
            Self::HostKeyUnreadable { path, source } => write!(
                formatter,
                "{}: the host key is not usable: {source}",
                path.display()
            ),
            Self::HostLine(source) => {
                write!(formatter, "cannot render the host address line: {source}")
            }
            Self::CertificateMissing { path } => write!(
                formatter,
                "{} holds no certificate: run certify host with the host address line above on the owner's machine and write its output there",
                path.display()
            ),
            Self::CertificateMalformed { path } => write!(
                formatter,
                "{}: the certificate must be {HOST_CERTIFICATE_HEX_CHARS} lowercase hexadecimal characters and one newline, as certify host prints it",
                path.display()
            ),
            Self::CertificateNotForThisHost { path } => write!(
                formatter,
                "{}: the certificate is not the network owner's signature of this host's node id: certify the host address line above on the owner's machine and write its output there",
                path.display()
            ),
            Self::LastRecordUnreadable { path } => write!(
                formatter,
                "{}: the last record counter must be a decimal integer and one newline: restore it, or remove the file to count on from the clock",
                path.display()
            ),
            Self::RelayNotStarted(source) => {
                write!(formatter, "the embedded relay did not start: {source}")
            }
            Self::EndpointNotBound(source) => write!(
                formatter,
                "the bootstrap node's endpoint was not bound: {source}"
            ),
            Self::TopicClosed => {
                formatter.write_str("the gossip topic closed, so this host announces nothing more")
            }
        }
    }
}

impl From<DirectoriesError> for Failure {
    fn from(source: DirectoriesError) -> Self {
        Self::Directories(source)
    }
}

/// An infrastructure command that could not run or stopped running, stated for whoever ran it.
#[derive(Debug)]
pub struct InfrastructureCommandError(Failure);

impl fmt::Display for InfrastructureCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for InfrastructureCommandError {}

/// Runs the relay and the bootstrap node of this host for the network until stopped, printing the host address line to `result` at start (spec 3.14).
///
/// # Errors
///
/// Returns [`InfrastructureCommandError`] naming what the operator must change, the missing or invalid certificate included.
pub async fn run(
    command: InfrastructureCommand,
    directories: &Directories,
    filesystem: &impl InfrastructureFilesystem,
    effects: InfrastructureEffects,
    result: &mut (impl Write + Send),
) -> Result<(), InfrastructureCommandError> {
    let InfrastructureCommand::RunInfrastructure {
        network,
        bind_address,
        domain,
    } = command;
    serve_host(
        &network,
        bind_address,
        &domain,
        directories,
        filesystem,
        effects,
        result,
    )
    .await
    .map_err(InfrastructureCommandError)
}

/// Reads the host's files, starts the relay and the bootstrap node, and serves until stopped.
async fn serve_host(
    network: &Handle,
    bind_address: IpAddr,
    domain: &RelayDomain,
    directories: &Directories,
    filesystem: &impl InfrastructureFilesystem,
    effects: InfrastructureEffects,
    result: &mut (impl Write + Send),
) -> Result<(), Failure> {
    let files = read_host_files(
        network,
        domain,
        directories,
        filesystem,
        effects.fill_from_secure_source,
        result,
    )?;
    let certificate_cache =
        directories.infrastructure_certificate_cache_directory(network.as_str())?;
    filesystem
        .restrict_directory_to_owner(&certificate_cache)
        .map_err(io_failure(&certificate_cache))?;
    let relay = (effects.start_relay)(relay_site(bind_address, domain, certificate_cache))
        .await
        .map_err(Failure::RelayNotStarted)?;
    let endpoint = (effects.bind_endpoint)(files.key.clone(), files.gate_key(), domain.url.clone())
        .await
        .map_err(Failure::EndpointNotBound)?;
    let mut node =
        BootstrapNode::start(files, endpoint, domain, effects.clock, effects.report).await?;
    let outcome = node.serve_until(effects.stopped, filesystem).await;
    node.stop().await;
    drop(relay);
    outcome
}

/// Reads a domain name that is the whole host of its relay URL `https://<domain>/`, which fits the wire's relay URL limit.
fn parse_relay_domain(text: &str) -> Result<RelayDomain, RelayDomainRefused> {
    let written = format!("{RELAY_URL_SCHEME_PREFIX}{text}{RELAY_URL_ROOT_PATH}");
    let url: RelayUrl = written.parse().map_err(|_unparsed| RelayDomainRefused)?;
    if url.host_str() != Some(text) {
        return Err(RelayDomainRefused);
    }
    let wire_url = wire::RelayUrl::try_from(written).map_err(|_over_limit| RelayDomainRefused)?;
    Ok(RelayDomain {
        name: text.to_owned(),
        url,
        wire_url,
    })
}

/// What a host reads before it serves: its key and certificate, its network, the other hosts it joins through, and its last record counter.
struct HostFiles {
    key: SecretKey,
    certificate: Signature,
    network: Network,
    other_hosts: Vec<KnownHostAddress>,
    last_record: LastRecord,
}

impl HostFiles {
    /// The gate key derived from the network secret.
    const fn gate_key(&self) -> GateKey {
        *self.network.keys().gate_key()
    }
}

/// Reads the network, the host key and the certificate, printing the host address line once the key is known and before the certificate is read.
fn read_host_files(
    network: &Handle,
    domain: &RelayDomain,
    directories: &Directories,
    filesystem: &impl InfrastructureFilesystem,
    fill_from_secure_source: FillFromSecureSource,
    result: &mut impl Write,
) -> Result<HostFiles, Failure> {
    let config_file = directories.network_config_file(network.as_str())?;
    let config = load_configured_network(network, &config_file)?;
    let known_hosts = known_host_addresses(&config).map_err(Failure::Node)?;
    let network_keys = Network::new(config, config_file).map_err(Failure::Node)?;
    let key_file = directories.infrastructure_key_file(network.as_str())?;
    let key = host_key(&key_file, filesystem, fill_from_secure_source)?;
    print_host_line(&key.public(), domain, result)?;
    let certificate_file = directories.infrastructure_certificate_file(network.as_str())?;
    let certificate = certifying_certificate(
        &certificate_file,
        filesystem,
        &network_keys.owner(),
        &key.public(),
    )?;
    let counter_file = directories.infrastructure_last_record_timestamp_file(network.as_str())?;
    let last_record = LastRecord::read(counter_file, filesystem)?;
    let other_hosts = known_hosts
        .into_iter()
        .filter(|host| host.id != key.public())
        .collect();
    Ok(HostFiles {
        key,
        certificate,
        network: network_keys,
        other_hosts,
        last_record,
    })
}

/// The network's configuration, refusing a network with no configuration file here.
fn load_configured_network(network: &Handle, config_file: &Path) -> Result<NetworkConfig, Failure> {
    NetworkConfig::load(config_file).map_err(|refusal| match refusal {
        ConfigError::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
            Failure::NetworkNotConfigured {
                network: network.as_str().to_owned(),
            }
        }
        refusal => Failure::Config(refusal),
    })
}

/// The host key from its file, or a new one written there readable by its owner alone when the file is missing.
fn host_key(
    path: &Path,
    filesystem: &impl InfrastructureFilesystem,
    fill_from_secure_source: FillFromSecureSource,
) -> Result<SecretKey, Failure> {
    match filesystem.read_to_string(path) {
        Ok(text) => parse_secret_key(&text).map_err(|source| Failure::HostKeyUnreadable {
            path: path.to_owned(),
            source,
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            create_host_key(path, filesystem, fill_from_secure_source)
        }
        Err(source) => Err(io_failure(path)(source)),
    }
}

/// Makes a host key from the secure source and writes it to a new file readable by its owner alone.
fn create_host_key(
    path: &Path,
    filesystem: &impl InfrastructureFilesystem,
    fill_from_secure_source: FillFromSecureSource,
) -> Result<SecretKey, Failure> {
    if let Some(directory) = path.parent() {
        filesystem
            .create_private_directory(directory)
            .map_err(io_failure(directory))?;
    }
    let key = generate_secret_key(fill_from_secure_source);
    filesystem
        .create_new_owner_only_file(path, &render_secret_key(&key))
        .map_err(io_failure(path))?;
    Ok(key)
}

/// Writes the host address line of the host's node id and relay.
fn print_host_line(
    host: &iroh::PublicKey,
    domain: &RelayDomain,
    result: &mut impl Write,
) -> Result<(), Failure> {
    let line = render_host_line(&HostAddr {
        v: Version::CURRENT,
        id: *host.as_bytes(),
        relay: domain.wire_url.clone(),
    })
    .map_err(Failure::HostLine)?;
    writeln!(result, "{line}").map_err(Failure::Output)
}

/// The certificate from its file, refusing one that is missing, malformed, or not the owner's signature of this host's node id.
fn certifying_certificate(
    path: &Path,
    filesystem: &impl InfrastructureFilesystem,
    owner: &iroh::PublicKey,
    host: &iroh::PublicKey,
) -> Result<Signature, Failure> {
    let text = filesystem
        .read_to_string(path)
        .map_err(|source| match source.kind() {
            io::ErrorKind::NotFound => Failure::CertificateMissing {
                path: path.to_owned(),
            },
            _ => io_failure(path)(source),
        })?;
    let certificate =
        parse_host_certificate(&text).ok_or_else(|| Failure::CertificateMalformed {
            path: path.to_owned(),
        })?;
    verify_host_certificate(owner, host, &certificate).map_err(|_mismatch| {
        Failure::CertificateNotForThisHost {
            path: path.to_owned(),
        }
    })?;
    Ok(certificate)
}

/// Reads 128 lowercase hexadecimal characters and an optional final newline as a signature.
fn parse_host_certificate(text: &str) -> Option<Signature> {
    let digits = text.strip_suffix(LINE_END).unwrap_or(text);
    let bytes: [u8; Signature::LENGTH] =
        HEXLOWER.decode(digits.as_bytes()).ok()?.try_into().ok()?;
    Some(Signature::from_bytes(&bytes))
}

/// The last record counter this host sent and the file that keeps it, starting from zero when the file is lost.
struct LastRecord {
    ts: u64,
    file: PathBuf,
}

impl LastRecord {
    /// Reads the counter from its file, refusing content that is not a decimal integer and one newline.
    fn read(file: PathBuf, filesystem: &impl InfrastructureFilesystem) -> Result<Self, Failure> {
        let ts = match filesystem.read_to_string(&file) {
            Ok(text) => text
                .strip_suffix(LINE_END)
                .filter(|digits| {
                    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
                })
                .and_then(|digits| digits.parse().ok())
                .ok_or_else(|| Failure::LastRecordUnreadable { path: file.clone() })?,
            Err(source) if source.kind() == io::ErrorKind::NotFound => LOST_RECORD_COUNTER,
            Err(source) => return Err(io_failure(&file)(source)),
        };
        Ok(Self { ts, file })
    }

    /// The `ts` of the next record: one above the last, or the wall clock in seconds when that is later (spec 3.14).
    fn next_ts(&self, now_seconds: u64) -> u64 {
        self.ts.saturating_add(1).max(now_seconds)
    }

    /// Writes the counter to its file and holds it as the last sent.
    fn write(
        &mut self,
        ts: u64,
        filesystem: &impl InfrastructureFilesystem,
    ) -> Result<(), Failure> {
        filesystem
            .replace_owner_only_file(&self.file, &format!("{ts}{LINE_END}"))
            .map_err(io_failure(&self.file))?;
        self.ts = ts;
        Ok(())
    }
}

/// Where the relay serves on the bind address for the domain, caching its certificate in the directory.
fn relay_site(bind_address: IpAddr, domain: &RelayDomain, certificate_cache: PathBuf) -> RelaySite {
    RelaySite {
        https: SocketAddr::new(bind_address, HTTPS_PORT),
        acme_challenge: SocketAddr::new(bind_address, ACME_CHALLENGE_PORT),
        quic_address_discovery: SocketAddr::new(bind_address, QUIC_ADDRESS_DISCOVERY_PORT),
        domain: domain.name.clone(),
        certificate_cache,
    }
}

/// What the bootstrap node answers on the direct protocol: no card, never attached, its own relay in every address.
#[derive(Clone)]
struct HostAnswers {
    endpoint: iroh::Endpoint,
    relay: wire::RelayUrl,
}

impl HostAnswers {
    /// This host's address: its own relay alone, whatever the endpoint reports, and the endpoint's direct addresses.
    fn address(&self) -> Addr {
        Addr::from_endpoint_report(
            [self.relay.as_str().to_owned()],
            self.endpoint.addr().ip_addrs().copied(),
        )
    }
}

impl DirectHost for HostAnswers {
    fn is_stopping(&self) -> bool {
        false
    }

    fn own_card(&self) -> Option<wire::PublicKey> {
        None
    }

    fn is_attached(&self) -> bool {
        false
    }

    fn receive_message(&self, _sender: wire::PublicKey, _message: Message) {}

    fn receive_introduce(
        &self,
        _sender: wire::PublicKey,
        _introduce: wire::Introduce,
    ) -> IntroduceAck {
        IntroduceAck {
            v: Version::CURRENT,
            handle: None,
            public: Vec::new(),
            attached: false,
            addr: self.address(),
            mac: ZERO_MAC,
        }
    }
}

/// What woke the bootstrap node.
enum Wake {
    Stopped,
    Topic(Option<Result<GossipEvent, GossipError>>),
    RecordDue,
}

/// The bootstrap node of spec 3.14: joined to the topic, broadcasting this host's record, learning other hosts, never rebinding.
struct BootstrapNode {
    key: SecretKey,
    certificate: Signature,
    answers: HostAnswers,
    endpoint: NodeEndpoint,
    router: Router,
    topic: NetworkTopic,
    known_hosts: KnownHosts,
    last_record: LastRecord,
    records_due: Option<Interval>,
    clock: Clock,
    report: RunRecord,
}

impl BootstrapNode {
    /// Serves the direct protocol and gossip on the endpoint and joins the topic through the other hosts, or alone.
    async fn start(
        files: HostFiles,
        endpoint: NodeEndpoint,
        domain: &RelayDomain,
        clock: Clock,
        report: RunRecord,
    ) -> Result<Self, Failure> {
        let keys = files.network.keys().clone();
        let owner = files.network.owner();
        seed_lookup_with_known_hosts(&endpoint, &files.other_hosts);
        let answers = HostAnswers {
            endpoint: endpoint.endpoint().clone(),
            relay: domain.wire_url.clone(),
        };
        let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());
        let router = Router::builder(endpoint.endpoint().clone())
            .accept(
                DIRECT_ALPN,
                DirectServer::new(keys.clone(), answers.clone()),
            )
            .accept(GOSSIP_ALPN, gossip.clone())
            .spawn();
        let bootstrap = files.other_hosts.iter().map(|host| host.id).collect();
        let topic = NetworkTopic::join(&gossip, keys, owner, bootstrap, Instant::now())
            .await
            .map_err(|source| Failure::Node(NodeError::Topic(source)))?;
        Ok(Self {
            key: files.key,
            certificate: files.certificate,
            answers,
            endpoint,
            router,
            topic,
            known_hosts: files.network.into_known_hosts(),
            last_record: files.last_record,
            records_due: None,
            clock,
            report,
        })
    }

    /// Handles every wake until the process is to stop, failing once the topic closes or a record counter cannot be written.
    async fn serve_until(
        &mut self,
        mut stopped: StopRequested,
        filesystem: &impl InfrastructureFilesystem,
    ) -> Result<(), Failure> {
        loop {
            match self.next_wake(&mut stopped).await {
                Wake::Stopped => return Ok(()),
                Wake::Topic(event) => self.take_topic_event(event)?,
                Wake::RecordDue => self.broadcast_record(filesystem).await?,
            }
        }
    }

    /// Waits for the stop, the next topic event, or the next record due.
    async fn next_wake(&mut self, stopped: &mut StopRequested) -> Wake {
        tokio::select! {
            biased;
            () = stopped => Wake::Stopped,
            event = self.topic.next_event() => Wake::Topic(event),
            () = next_tick(self.records_due.as_mut()) => Wake::RecordDue,
        }
    }

    /// Starts the record broadcasts at the join, learns from a host record, and ignores every other event.
    fn take_topic_event(
        &mut self,
        event: Option<Result<GossipEvent, GossipError>>,
    ) -> Result<(), Failure> {
        match event {
            None => return Err(Failure::TopicClosed),
            Some(Err(failure)) => (self.report)(NodeRecord::TopicFailed(failure)),
            Some(Ok(GossipEvent::Joined)) => self.records_due = Some(record_interval()),
            Some(Ok(GossipEvent::Payload(payload))) => match *payload {
                Payload::Infrastructure(record) if record.host != *self.key.public().as_bytes() => {
                    self.learn_host(&record);
                }
                Payload::Infrastructure(_)
                | Payload::Presence(_)
                | Payload::Departure(_)
                | Payload::Unreachable(_) => {}
            },
            Some(Ok(
                GossipEvent::JoinTimedOut
                | GossipEvent::NeighborUp(_)
                | GossipEvent::NeighborDown(_),
            )) => {}
        }
        Ok(())
    }

    /// Applies the Infrastructure rows of spec 3.8 to another host's record, adding no relay to the relay map (spec 3.14).
    fn learn_host(&mut self, record: &Infrastructure) {
        let now_ms = (self.clock)();
        match self.known_hosts.take_record(record, now_ms) {
            RecordVerdict::Dropped => {}
            RecordVerdict::FromKnownHost => self.point_lookup_at(record),
            RecordVerdict::FromNewHost => {
                self.point_lookup_at(record);
                if let Err(failure) = self.known_hosts.save(now_ms) {
                    (self.report)(NodeRecord::KnownHostsNotSaved(failure));
                }
            }
        }
    }

    /// Replaces the lookup entry of the record's host with the address it carries.
    fn point_lookup_at(&self, record: &Infrastructure) {
        if let Some(id) = endpoint_id(&record.host) {
            self.endpoint
                .replace_address(endpoint_addr(id, &record.addr));
        }
    }

    /// Writes the next record counter, then broadcasts this host's record carrying it.
    async fn broadcast_record(
        &mut self,
        filesystem: &impl InfrastructureFilesystem,
    ) -> Result<(), Failure> {
        let ts = self.last_record.next_ts((self.clock)() / MILLIS_PER_SECOND);
        self.last_record.write(ts, filesystem)?;
        let record = Payload::Infrastructure(Infrastructure {
            v: Version::CURRENT,
            host: *self.key.public().as_bytes(),
            relay: self.answers.relay.clone(),
            addr: self.answers.address(),
            ts,
            cert: SignatureBytes(self.certificate.to_bytes()),
            mac: ZERO_MAC,
            sig: UNSIGNED,
        });
        if let Err(failure) = self.topic.broadcast(&self.key, record).await {
            (self.report)(NodeRecord::BroadcastFailed(failure));
        }
        Ok(())
    }

    /// Saves the known hosts when a held record is newer than the file's, and shuts the endpoint's listeners down.
    async fn stop(mut self) {
        if let Err(failure) = self.known_hosts.save_if_newer_than_file((self.clock)()) {
            (self.report)(NodeRecord::KnownHostsNotSaved(failure));
        }
        if let Err(failure) = self.router.shutdown().await {
            (self.report)(NodeRecord::RouterShutdownFailed(failure));
        }
    }
}

/// Waits for the next tick of the record interval, or forever before the first join.
async fn next_tick(records_due: Option<&mut Interval>) {
    match records_due {
        Some(records_due) => {
            records_due.tick().await;
        }
        None => std::future::pending().await,
    }
}

/// The interval of this host's records, ticking at once for the record due at the join (spec 3.14).
fn record_interval() -> Interval {
    let mut records_due = interval(INFRASTRUCTURE_RECORD_INTERVAL);
    records_due.set_missed_tick_behavior(MissedTickBehavior::Delay);
    records_due
}

/// The failure of an operation on the file at the path.
fn io_failure(path: &Path) -> impl FnOnce(io::Error) -> Failure {
    |source| Failure::Io {
        path: path.to_owned(),
        source,
    }
}
#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::net::Ipv4Addr;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    use n0_future::StreamExt;

    use super::*;
    use crate::config::{Host, HostOrigin, NetworkConfig};
    use crate::crypto::{
        NetworkKeys, NetworkSecret, render_host_certificate, sign_host_certificate,
    };
    use crate::dirs::OperatingSystemFilesystem;
    use crate::inbox::socket::{InboxSocket, ProcessTable};
    use crate::invite::render_host_line;
    use crate::keys::{render_public_key, render_secret_key};
    use crate::net::direct::{ClientTimeouts, DirectClient};
    use crate::net::endpoint::NodeEndpoint;
    use crate::node::{Identity, Node, NodeEffects};
    use crate::wire::{HostAddr, Version};

    const NETWORK: &str = "lab";
    const DOMAIN: &str = "relay-a.estafette.invalid";
    const SPEC_RELAY_URL: &str = "https://relay-a.estafette.invalid/";
    const BIND_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7));
    const NETWORK_SECRET: [u8; KEY_LEN] = [57; KEY_LEN];
    const OWNER_SEED: u8 = 120;
    const HOST_SEED: u8 = 121;
    const SECOND_RUN_SEED: u8 = 122;
    const IMPOSTOR_OWNER_SEED: u8 = 123;
    const OTHER_HOST_SEED: u8 = 124;
    const LOOSE_DIRECTORY_MODE: u32 = 0o755;
    const SPEC_PRIVATE_DIRECTORY_MODE: u32 = 0o700;
    const SPEC_HTTPS_ADDRESS: &str = "192.0.2.7:443";
    const SPEC_ACME_CHALLENGE_ADDRESS: &str = "192.0.2.7:80";
    const SPEC_QUIC_ADDRESS_DISCOVERY_ADDRESS: &str = "192.0.2.7:7842";
    const FROZEN_CLOCK_SECONDS: u64 = FROZEN_CLOCK_MS / 1000;
    const PEER_SEED: u8 = 125;
    const SECOND_PEER_SEED: u8 = 126;
    const BIND_WAIT: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(20);
    const PEER_JOIN_WAIT: Duration = Duration::from_secs(8);
    const RECORD_AT_JOIN_WAIT: Duration = Duration::from_millis(4000);
    const SPEC_RECORD_INTERVAL_EARLIEST: Duration = Duration::from_millis(4500);
    const SPEC_RECORD_INTERVAL_LATEST: Duration = Duration::from_millis(6500);
    const SPEC_JOIN_TIMEOUT_PAST: Duration = Duration::from_millis(6000);
    const MESSAGE_ID: wire::MessageId = [5; 16];
    const PEER_HANDLE: &str = "pat";
    const PEER_PUBLIC: &[u8] = br#"{"role":"peer"}"#;
    const MESSAGE_BODY: &str = "a message to a host, which holds no card";
    const PROBE_NONCE: wire::Nonce = [8; 16];
    const SPEC_REFUSED_REASON: &str = "refused";
    const SECOND_HOST_SEED: u8 = 127;
    const SECOND_DOMAIN: &str = "relay-b.estafette.invalid";
    const SPEC_SECOND_RELAY_URL: &str = "https://relay-b.estafette.invalid/";
    const HOST_LEARNED_WAIT: Duration = Duration::from_secs(12);
    const MEMBER_SEED: u8 = 128;
    const MEMBER_HANDLE: &str = "mia";
    const MEMBER_PUBLIC: &[u8] = br#"{"role":"member"}"#;
    const MEMBER_CONFIG_FILE: &str = "member-network.toml";
    const REPLAYED_OWN_TS: u64 = FROZEN_CLOCK_SECONDS + 100;
    const SECOND_RUN_TS: u64 = FROZEN_CLOCK_SECONDS + 1;
    const OTHER_HOST_RELAY_URL: &str = "https://relay-c.estafette.invalid/";
    const SPEC_BIND_ADDRESS_TEXT: &str = "192.0.2.7";
    const LONGEST_LABEL: usize = 63;
    const LONGEST_DOMAIN: usize = 119;
    const SPEC_OWNER_ONLY_MODE: u32 = 0o600;
    const FILE_MODE_BITS: u32 = 0o777;
    const FROZEN_CLOCK_MS: u64 = 1_800_000_000_000;

    fn secret_key(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; KEY_LEN])
    }

    struct Scratch {
        root: PathBuf,
        directories: Directories,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("u29-{tag}-{}", std::process::id()));
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
            Self { root, directories }
        }

        fn configure_network(&self) {
            self.configure_network_with(Vec::new());
        }

        fn configure_network_with(&self, hosts: Vec<Host>) {
            let path = self.config_file();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            NetworkConfig {
                secret: data_encoding::HEXLOWER.encode(&NETWORK_SECRET),
                owner: render_public_key(&secret_key(OWNER_SEED).public()),
                hosts,
            }
            .rewrite(&path, FROZEN_CLOCK_MS / 1000)
            .unwrap();
        }

        fn config_file(&self) -> PathBuf {
            self.directories.network_config_file(NETWORK).unwrap()
        }

        fn saved_host(&self, host_seed: u8) -> Option<Host> {
            let id = render_public_key(&secret_key(host_seed).public());
            NetworkConfig::load(&self.config_file())
                .unwrap()
                .hosts
                .into_iter()
                .find(|host| host.id == id)
        }

        fn key_file(&self) -> PathBuf {
            self.directories.infrastructure_key_file(NETWORK).unwrap()
        }

        fn certificate_file(&self) -> PathBuf {
            self.directories
                .infrastructure_certificate_file(NETWORK)
                .unwrap()
        }

        fn certify(&self, host_seed: u8) {
            let path = self.certificate_file();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, certificate_line(OWNER_SEED, host_seed)).unwrap();
        }

        fn certificate_cache(&self) -> PathBuf {
            self.directories
                .infrastructure_certificate_cache_directory(NETWORK)
                .unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            drop(fs::remove_dir_all(&self.root));
        }
    }

    fn command_for(domain: &str) -> InfrastructureCommand {
        InfrastructureCommand::RunInfrastructure {
            network: NETWORK.parse().unwrap(),
            bind_address: BIND_ADDRESS,
            domain: parse_relay_domain(domain).unwrap(),
        }
    }

    #[derive(Clone, Default)]
    struct Seen {
        relay_sites: Arc<Mutex<Vec<(RelaySite, u32)>>>,
        bound: Arc<Mutex<Vec<Bound>>>,
    }

    #[derive(Clone)]
    struct Bound {
        host: iroh::PublicKey,
        gate_key: GateKey,
        relay: RelayUrl,
        endpoint: iroh::Endpoint,
    }

    struct Plan {
        seed: u8,
        domain: &'static str,
        clock: Clock,
        reachable: Vec<iroh::EndpointAddr>,
        stopped: StopRequested,
    }

    impl Plan {
        fn new(seed: u8) -> Self {
            Self {
                seed,
                domain: DOMAIN,
                clock: Arc::new(|| FROZEN_CLOCK_MS),
                reachable: Vec::new(),
                stopped: Box::pin(std::future::ready(())),
            }
        }

        fn effects(self, seen: &Seen) -> InfrastructureEffects {
            let Self {
                seed,
                domain: _,
                clock,
                reachable,
                stopped,
            } = self;
            let relay_sites = Arc::clone(&seen.relay_sites);
            let bound = Arc::clone(&seen.bound);
            InfrastructureEffects {
                fill_from_secure_source: Box::new(move |bytes| *bytes = [seed; KEY_LEN]),
                clock,
                bind_endpoint: Box::new(move |key, gate_key, relay| {
                    Box::pin(async move {
                        let host = key.public();
                        let endpoint = NodeEndpoint::bind(
                            key,
                            gate_key,
                            [relay.clone()],
                            move |nonce: &mut [u8; KEY_LEN]| *nonce = [seed; KEY_LEN],
                            |_ignored| {},
                        )
                        .await?;
                        for address in reachable {
                            endpoint.replace_address(address);
                        }
                        bound.lock().unwrap().push(Bound {
                            host,
                            gate_key,
                            relay,
                            endpoint: endpoint.endpoint().clone(),
                        });
                        Ok(endpoint)
                    })
                }),
                start_relay: Box::new(move |site| {
                    Box::pin(async move {
                        let mode = mode_of(&site.certificate_cache);
                        relay_sites.lock().unwrap().push((site, mode));
                        let serving: RunningRelay = Box::new(());
                        Ok(serving)
                    })
                }),
                stopped,
                report: Arc::new(|_record| {}),
            }
        }
    }

    fn spec_host_line(seed: u8) -> String {
        let host = HostAddr {
            v: Version::CURRENT,
            id: *secret_key(seed).public().as_bytes(),
            relay: wire::RelayUrl::try_from(SPEC_RELAY_URL.to_owned()).unwrap(),
        };
        format!("{}\n", render_host_line(&host).unwrap())
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & FILE_MODE_BITS
    }

    async fn run_once(
        scratch: &Scratch,
        host_seed: u8,
        seen: &Seen,
    ) -> (String, Result<(), InfrastructureCommandError>) {
        run_planned(scratch, Plan::new(host_seed), seen).await
    }

    async fn run_planned(
        scratch: &Scratch,
        plan: Plan,
        seen: &Seen,
    ) -> (String, Result<(), InfrastructureCommandError>) {
        let mut printed = Vec::new();
        let outcome = run(
            command_for(plan.domain),
            &scratch.directories,
            &OperatingSystemFilesystem,
            plan.effects(seen),
            &mut printed,
        )
        .await;
        (String::from_utf8(printed).unwrap(), outcome)
    }

    async fn bound_endpoint(seen: &Seen, index: usize) -> iroh::Endpoint {
        tokio::time::timeout(BIND_WAIT, async {
            loop {
                if let Some(bound) = seen.bound.lock().unwrap().get(index) {
                    return bound.endpoint.clone();
                }
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .unwrap()
    }

    fn network_keys() -> NetworkKeys {
        NetworkKeys::derive(&NetworkSecret::from_bytes(NETWORK_SECRET))
    }

    struct Peer {
        endpoint: NodeEndpoint,
        topic: NetworkTopic,
        _gossip: Gossip,
        _router: Router,
    }

    impl Peer {
        async fn joined_through(seed: u8, host: &iroh::Endpoint) -> Self {
            let endpoint = NodeEndpoint::bind(
                secret_key(seed),
                *network_keys().gate_key(),
                Vec::new(),
                move |nonce: &mut [u8; KEY_LEN]| *nonce = [seed; KEY_LEN],
                |_ignored| {},
            )
            .await
            .unwrap();
            endpoint.replace_address(host.addr());
            let gossip = Gossip::builder().spawn(endpoint.endpoint().clone());
            let router = Router::builder(endpoint.endpoint().clone())
                .accept(GOSSIP_ALPN, gossip.clone())
                .spawn();
            let mut topic = NetworkTopic::join(
                &gossip,
                network_keys(),
                secret_key(OWNER_SEED).public(),
                vec![host.id()],
                Instant::now(),
            )
            .await
            .unwrap();
            tokio::time::timeout(PEER_JOIN_WAIT, async {
                while topic.next_event().await.unwrap().unwrap() != GossipEvent::Joined {}
            })
            .await
            .unwrap();
            Self {
                endpoint,
                topic,
                _gossip: gossip,
                _router: router,
            }
        }

        async fn next_host_record(&mut self, within: Duration) -> Option<Infrastructure> {
            tokio::time::timeout(within, async {
                loop {
                    if let Some(Ok(GossipEvent::Payload(payload))) = self.topic.next_event().await
                        && let Payload::Infrastructure(record) = *payload
                    {
                        return record;
                    }
                }
            })
            .await
            .ok()
        }
    }

    async fn record_heard_in_one_run(scratch: &Scratch, peer_seed: u8) -> Infrastructure {
        let seen = Seen::default();
        let (stop, stopped) = stopped_on_signal();
        let plan = Plan {
            stopped,
            ..Plan::new(HOST_SEED)
        };
        let ((_printed, outcome), record) =
            tokio::join!(run_planned(scratch, plan, &seen), async {
                let host = bound_endpoint(&seen, 0).await;
                let mut peer = Peer::joined_through(peer_seed, &host).await;
                let record = peer.next_host_record(RECORD_AT_JOIN_WAIT).await;
                stop.send(()).unwrap();
                record
            });
        outcome.unwrap();
        record.unwrap()
    }

    fn spec_certificate_bytes(host_seed: u8) -> SignatureBytes {
        SignatureBytes(
            sign_host_certificate(&secret_key(OWNER_SEED), &secret_key(host_seed).public())
                .to_bytes(),
        )
    }

    #[tokio::test]
    async fn spec_3_14_the_record_sent_at_the_join_carries_a_ts_written_first_and_increasing_across_runs()
     {
        let scratch = Scratch::new("record-counter");
        scratch.configure_network();
        scratch.certify(HOST_SEED);
        let counter_file = scratch
            .directories
            .infrastructure_last_record_timestamp_file(NETWORK)
            .unwrap();

        let first = Box::pin(record_heard_in_one_run(&scratch, PEER_SEED)).await;
        let first_file = fs::read_to_string(&counter_file).unwrap();
        let second = Box::pin(record_heard_in_one_run(&scratch, SECOND_PEER_SEED)).await;
        let second_file = fs::read_to_string(&counter_file).unwrap();

        assert_eq!(first.ts, FROZEN_CLOCK_SECONDS);
        assert_eq!(first_file, format!("{FROZEN_CLOCK_SECONDS}\n"));
        assert_eq!(second.ts, SECOND_RUN_TS);
        assert_eq!(second_file, format!("{SECOND_RUN_TS}\n"));
        assert_eq!(mode_of(&counter_file), SPEC_OWNER_ONLY_MODE);
        for record in [first, second] {
            assert_eq!(record.host, *secret_key(HOST_SEED).public().as_bytes());
            assert_eq!(record.relay.as_str(), SPEC_RELAY_URL);
            assert_eq!(
                record
                    .addr
                    .relays()
                    .iter()
                    .map(wire::RelayUrl::as_str)
                    .collect::<Vec<_>>(),
                vec![SPEC_RELAY_URL]
            );
            assert_eq!(record.cert, spec_certificate_bytes(HOST_SEED));
        }
    }

    #[tokio::test]
    async fn spec_3_14_the_first_run_creates_the_host_key_owner_only_and_a_later_run_keeps_it() {
        let scratch = Scratch::new("first-run");
        scratch.configure_network();
        let seen = Seen::default();

        let (first_line, first) = run_once(&scratch, HOST_SEED, &seen).await;
        let (second_line, second) = run_once(&scratch, SECOND_RUN_SEED, &seen).await;

        assert_eq!(
            fs::read_to_string(scratch.key_file()).unwrap(),
            render_secret_key(&secret_key(HOST_SEED))
        );
        assert_eq!(mode_of(&scratch.key_file()), SPEC_OWNER_ONLY_MODE);
        assert_eq!(first_line, spec_host_line(HOST_SEED));
        assert_eq!(second_line, spec_host_line(HOST_SEED));
        for outcome in [first, second] {
            let refusal = outcome.unwrap_err().to_string();
            assert!(refusal.contains(&scratch.certificate_file().display().to_string()));
        }
    }

    fn certificate_line(owner_seed: u8, host_seed: u8) -> String {
        let certificate =
            sign_host_certificate(&secret_key(owner_seed), &secret_key(host_seed).public());
        format!("{}\n", render_host_certificate(&certificate))
    }

    #[tokio::test]
    async fn spec_3_14_without_a_valid_certificate_the_host_line_is_printed_and_the_run_fails_before_serving()
     {
        let scratch = Scratch::new("invalid-certificate");
        scratch.configure_network();
        let seen = Seen::default();
        let invalid_certificates = [
            (None, "holds no certificate"),
            (
                Some(certificate_line(OWNER_SEED, HOST_SEED).to_uppercase()),
                "lowercase hexadecimal",
            ),
            (
                Some("not a certificate\n".to_owned()),
                "lowercase hexadecimal",
            ),
            (
                Some(certificate_line(IMPOSTOR_OWNER_SEED, HOST_SEED)),
                "not the network owner's signature",
            ),
            (
                Some(certificate_line(OWNER_SEED, OTHER_HOST_SEED)),
                "not the network owner's signature",
            ),
        ];

        for (certificate, refusal_names) in invalid_certificates {
            drop(fs::remove_file(scratch.certificate_file()));
            if let Some(certificate) = certificate {
                fs::create_dir_all(scratch.certificate_file().parent().unwrap()).unwrap();
                fs::write(scratch.certificate_file(), certificate).unwrap();
            }

            let (printed, outcome) = run_once(&scratch, HOST_SEED, &seen).await;

            assert_eq!(printed, spec_host_line(HOST_SEED));
            assert!(outcome.unwrap_err().to_string().contains(refusal_names));
        }
        assert!(seen.relay_sites.lock().unwrap().is_empty());
        assert!(seen.bound.lock().unwrap().is_empty());
    }

    fn spec_gate_key() -> GateKey {
        *NetworkKeys::derive(&NetworkSecret::from_bytes(NETWORK_SECRET)).gate_key()
    }

    #[tokio::test]
    async fn spec_3_14_a_certified_host_serves_its_relay_on_443_80_and_7842_with_a_private_cache_and_binds_with_its_own_relay_alone()
     {
        let scratch = Scratch::new("serving");
        scratch.configure_network();
        scratch.certify(HOST_SEED);
        fs::create_dir_all(scratch.certificate_cache()).unwrap();
        fs::set_permissions(
            scratch.certificate_cache(),
            fs::Permissions::from_mode(LOOSE_DIRECTORY_MODE),
        )
        .unwrap();
        let seen = Seen::default();

        let (printed, outcome) = run_once(&scratch, HOST_SEED, &seen).await;

        outcome.unwrap();
        assert_eq!(printed, spec_host_line(HOST_SEED));
        let expected_site = RelaySite {
            https: SPEC_HTTPS_ADDRESS.parse().unwrap(),
            acme_challenge: SPEC_ACME_CHALLENGE_ADDRESS.parse().unwrap(),
            quic_address_discovery: SPEC_QUIC_ADDRESS_DISCOVERY_ADDRESS.parse().unwrap(),
            domain: DOMAIN.to_owned(),
            certificate_cache: scratch.certificate_cache(),
        };
        assert_eq!(
            *seen.relay_sites.lock().unwrap(),
            vec![(expected_site, SPEC_PRIVATE_DIRECTORY_MODE)]
        );
        let bound = seen.bound.lock().unwrap();
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].host, secret_key(HOST_SEED).public());
        assert_eq!(bound[0].gate_key.as_bytes(), spec_gate_key().as_bytes());
        assert_eq!(bound[0].relay, SPEC_RELAY_URL.parse().unwrap());
    }

    fn stopped_on_signal() -> (tokio::sync::oneshot::Sender<()>, StopRequested) {
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        (stop, Box::pin(async move { stopped.await.unwrap_or(()) }))
    }

    #[tokio::test]
    async fn spec_3_14_a_joined_host_broadcasts_its_record_again_every_infrastructure_record_interval()
     {
        let scratch = Scratch::new("record-interval");
        scratch.configure_network();
        scratch.certify(HOST_SEED);
        let seen = Seen::default();
        let (stop, stopped) = stopped_on_signal();
        let plan = Plan {
            stopped,
            ..Plan::new(HOST_SEED)
        };

        let ((_printed, outcome), records) =
            tokio::join!(run_planned(&scratch, plan, &seen), async {
                let host = bound_endpoint(&seen, 0).await;
                let mut peer = Peer::joined_through(PEER_SEED, &host).await;
                let first = peer.next_host_record(RECORD_AT_JOIN_WAIT).await.unwrap();
                let first_heard = Instant::now();
                let second = peer
                    .next_host_record(SPEC_RECORD_INTERVAL_LATEST)
                    .await
                    .unwrap();
                let spacing = first_heard.elapsed();
                stop.send(()).unwrap();
                (first, second, spacing)
            });

        outcome.unwrap();
        let (first, second, spacing) = records;
        assert_eq!(Some(second.ts), first.ts.checked_add(1));
        assert!(SPEC_RECORD_INTERVAL_EARLIEST <= spacing && spacing < SPEC_RECORD_INTERVAL_LATEST);
    }

    #[tokio::test]
    async fn spec_3_14_a_host_alone_past_the_join_timeout_neither_rebinds_nor_gives_up_and_announces_at_the_first_join()
     {
        let scratch = Scratch::new("no-join-timeout");
        scratch.configure_network();
        scratch.certify(HOST_SEED);
        let seen = Seen::default();
        let (stop, stopped) = stopped_on_signal();
        let plan = Plan {
            stopped,
            ..Plan::new(HOST_SEED)
        };

        let ((_printed, outcome), record) =
            tokio::join!(run_planned(&scratch, plan, &seen), async {
                let host = bound_endpoint(&seen, 0).await;
                tokio::time::sleep(SPEC_JOIN_TIMEOUT_PAST).await;
                let mut peer = Peer::joined_through(PEER_SEED, &host).await;
                let record = peer.next_host_record(RECORD_AT_JOIN_WAIT).await;
                stop.send(()).unwrap();
                record
            });

        outcome.unwrap();
        assert_eq!(record.unwrap().ts, FROZEN_CLOCK_SECONDS);
        assert_eq!(seen.bound.lock().unwrap().len(), 1);
    }

    fn message_to(card: wire::PublicKey) -> Message {
        Message {
            v: Version::CURRENT,
            id: MESSAGE_ID,
            to: card,
            handle: PEER_HANDLE.to_owned(),
            attached: true,
            sent_at: FROZEN_CLOCK_MS,
            body: MESSAGE_BODY.to_owned(),
            mac: ZERO_MAC,
        }
    }

    fn introduction_from_peer() -> wire::Introduce {
        wire::Introduce {
            v: Version::CURRENT,
            handle: Some(PEER_HANDLE.to_owned()),
            public: PEER_PUBLIC.to_vec(),
            attached: true,
            addr: Addr::from_endpoint_report([], []),
            mac: ZERO_MAC,
        }
    }

    #[tokio::test]
    async fn spec_3_14_the_host_refuses_every_message_answers_probe_and_answers_introduce_without_a_handle()
     {
        let scratch = Scratch::new("direct");
        scratch.configure_network();
        scratch.certify(HOST_SEED);
        let seen = Seen::default();
        let (stop, stopped) = stopped_on_signal();
        let plan = Plan {
            stopped,
            ..Plan::new(HOST_SEED)
        };

        let ((_printed, outcome), answers) =
            tokio::join!(run_planned(&scratch, plan, &seen), async {
                let host = bound_endpoint(&seen, 0).await;
                let peer = Peer::joined_through(PEER_SEED, &host).await;
                let client = DirectClient::new(
                    peer.endpoint.endpoint().clone(),
                    network_keys(),
                    ClientTimeouts::SPEC,
                );
                let to_host = client
                    .send_message(host.id(), message_to(*host.id().as_bytes()))
                    .await;
                let to_peer_card = client
                    .send_message(
                        host.id(),
                        message_to(*secret_key(PEER_SEED).public().as_bytes()),
                    )
                    .await;
                let probed = client.probe(host.id(), PROBE_NONCE).await;
                let introduced = client.introduce(host.id(), introduction_from_peer()).await;
                stop.send(()).unwrap();
                (to_host, to_peer_card, probed, introduced)
            });

        outcome.unwrap();
        let (to_host, to_peer_card, probed, introduced) = answers;
        for refused in [to_host.unwrap(), to_peer_card.unwrap()] {
            assert!(!refused.accepted);
            assert_eq!(refused.reason.as_deref(), Some(SPEC_REFUSED_REASON));
        }
        let probed = probed.unwrap();
        assert_eq!(probed.nonce, PROBE_NONCE);
        assert!(!probed.attached);
        let introduced = introduced.unwrap();
        assert_eq!(introduced.handle, None);
        assert_eq!(introduced.public, Vec::<u8>::new());
        assert!(!introduced.attached);
        assert_eq!(
            introduced
                .addr
                .relays()
                .iter()
                .map(wire::RelayUrl::as_str)
                .collect::<Vec<_>>(),
            vec![SPEC_RELAY_URL]
        );
    }

    fn configured_host(seed: u8, relay: &str) -> Host {
        Host {
            id: render_public_key(&secret_key(seed).public()),
            relay: relay.to_owned(),
            ts: None,
            origin: HostOrigin::Configured,
        }
    }

    async fn relay_map_holds(endpoint: &iroh::Endpoint, relay: &str) -> bool {
        let relay: RelayUrl = relay.parse().unwrap();
        let removed = endpoint.remove_relay(&relay).await;
        if let Some(entry) = &removed {
            endpoint.insert_relay(relay, Arc::clone(entry)).await;
        }
        removed.is_some()
    }

    async fn lookup_relays(endpoint: &iroh::Endpoint, seed: u8) -> Vec<String> {
        let lookup = endpoint.address_lookup().unwrap();
        match lookup.resolve(secret_key(seed).public()).next().await {
            Some(Ok(Ok(item))) => item
                .into_endpoint_addr()
                .relay_urls()
                .map(ToString::to_string)
                .collect(),
            _ => Vec::new(),
        }
    }

    async fn saved_within(scratch: &Scratch, host_seed: u8, within: Duration) -> Option<Host> {
        tokio::time::timeout(within, async {
            loop {
                if let Some(host) = scratch.saved_host(host_seed) {
                    return host;
                }
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .ok()
    }

    #[tokio::test]
    async fn spec_3_14_a_host_learns_a_host_joining_through_it_saves_it_and_keeps_its_relay_map_to_its_own_relay()
     {
        let first_scratch = Scratch::new("first-host");
        first_scratch.configure_network();
        first_scratch.certify(HOST_SEED);
        let second_scratch = Scratch::new("second-host");
        second_scratch.configure_network_with(vec![configured_host(HOST_SEED, SPEC_RELAY_URL)]);
        second_scratch.certify(SECOND_HOST_SEED);
        let (first_seen, second_seen) = (Seen::default(), Seen::default());
        let (stop_first, first_stopped) = stopped_on_signal();
        let first_plan = Plan {
            stopped: first_stopped,
            ..Plan::new(HOST_SEED)
        };

        let ((_printed, first_outcome), observed) = tokio::join!(
            run_planned(&first_scratch, first_plan, &first_seen),
            async {
                let first = bound_endpoint(&first_seen, 0).await;
                let (stop_second, second_stopped) = stopped_on_signal();
                let second_plan = Plan {
                    domain: SECOND_DOMAIN,
                    reachable: vec![first.addr()],
                    stopped: second_stopped,
                    ..Plan::new(SECOND_HOST_SEED)
                };
                let ((_printed, second_outcome), observed) = tokio::join!(
                    run_planned(&second_scratch, second_plan, &second_seen),
                    async {
                        let learned =
                            saved_within(&first_scratch, SECOND_HOST_SEED, HOST_LEARNED_WAIT).await;
                        tokio::time::sleep(SPEC_RECORD_INTERVAL_LATEST).await;
                        let first_holds_own = relay_map_holds(&first, SPEC_RELAY_URL).await;
                        let first_holds_second =
                            relay_map_holds(&first, SPEC_SECOND_RELAY_URL).await;
                        let looked_up = lookup_relays(&first, SECOND_HOST_SEED).await;
                        stop_second.send(()).unwrap();
                        (learned, first_holds_own, first_holds_second, looked_up)
                    }
                );
                second_outcome.unwrap();
                stop_first.send(()).unwrap();
                observed
            }
        );

        first_outcome.unwrap();
        let (learned, first_holds_own, first_holds_second, looked_up) = observed;
        let learned = learned.unwrap();
        assert_eq!(learned.relay, SPEC_SECOND_RELAY_URL);
        assert_eq!(learned.ts, Some(FROZEN_CLOCK_SECONDS));
        assert_eq!(
            learned.origin,
            HostOrigin::Learned {
                heard: FROZEN_CLOCK_MS
            }
        );
        assert!(first_holds_own);
        assert!(!first_holds_second);
        assert_eq!(looked_up, vec![SPEC_SECOND_RELAY_URL]);
        let kept = second_scratch.saved_host(HOST_SEED).unwrap();
        assert_eq!(kept.origin, HostOrigin::Configured);
        assert!(kept.ts.is_some_and(|ts| ts >= FROZEN_CLOCK_SECONDS));
    }

    struct EveryProcessRuns;

    impl ProcessTable for EveryProcessRuns {
        fn is_running(&self, _process_id: u32) -> io::Result<bool> {
            Ok(true)
        }
    }

    async fn member_joined_through(
        scratch: &Scratch,
        host: &iroh::Endpoint,
    ) -> (Node, iroh::Endpoint) {
        let endpoint = NodeEndpoint::bind(
            secret_key(MEMBER_SEED),
            *network_keys().gate_key(),
            Vec::new(),
            |nonce: &mut [u8; KEY_LEN]| *nonce = [MEMBER_SEED; KEY_LEN],
            |_ignored| {},
        )
        .await
        .unwrap();
        endpoint.replace_address(host.addr());
        let member_endpoint = endpoint.endpoint().clone();
        let config = NetworkConfig {
            secret: data_encoding::HEXLOWER.encode(&NETWORK_SECRET),
            owner: render_public_key(&secret_key(OWNER_SEED).public()),
            hosts: vec![configured_host(HOST_SEED, SPEC_RELAY_URL)],
        };
        let inbox = InboxSocket::start(
            &scratch.directories,
            u32::from(MEMBER_SEED),
            &EveryProcessRuns,
        )
        .unwrap();
        let node = Node::start(
            endpoint,
            Identity {
                card_key: secret_key(MEMBER_SEED),
                requested_handle: MEMBER_HANDLE.parse().unwrap(),
                public: MEMBER_PUBLIC.to_vec(),
            },
            Network::new(config, scratch.root.join(MEMBER_CONFIG_FILE)).unwrap(),
            NodeEffects {
                clock: Arc::new(|| FROZEN_CLOCK_MS),
                monotonic: Arc::new(Instant::now),
                fill_probe_nonce: Arc::new(|nonce: &mut wire::Nonce| *nonce = PROBE_NONCE),
                report: Arc::new(|_record| {}),
                inbox: Arc::new(inbox),
                probe_relay: Arc::new(|_relay| Box::pin(async { true })),
            },
        )
        .await
        .unwrap();
        (node, member_endpoint)
    }

    async fn relay_map_holds_within(
        endpoint: &iroh::Endpoint,
        relay: &str,
        within: Duration,
    ) -> bool {
        tokio::time::timeout(within, async {
            while !relay_map_holds(endpoint, relay).await {
                tokio::time::sleep(POLL).await;
            }
        })
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn spec_3_15_test_22_logic_a_member_in_process_receives_the_hosts_record_and_learns_its_relay_and_address()
     {
        let scratch = Scratch::new("member");
        scratch.configure_network();
        scratch.certify(HOST_SEED);
        let seen = Seen::default();
        let (stop, stopped) = stopped_on_signal();
        let plan = Plan {
            stopped,
            ..Plan::new(HOST_SEED)
        };

        let ((_printed, outcome), observed) =
            tokio::join!(run_planned(&scratch, plan, &seen), async {
                let host = bound_endpoint(&seen, 0).await;
                let (member, member_endpoint) = member_joined_through(&scratch, &host).await;
                let learned =
                    relay_map_holds_within(&member_endpoint, SPEC_RELAY_URL, HOST_LEARNED_WAIT)
                        .await;
                let looked_up = lookup_relays(&member_endpoint, HOST_SEED).await;
                let listed = member.peers().len();
                drop(member);
                stop.send(()).unwrap();
                (learned, looked_up, listed)
            });

        outcome.unwrap();
        let (learned, looked_up, listed) = observed;
        assert!(learned);
        assert_eq!(looked_up, vec![SPEC_RELAY_URL]);
        assert_eq!(listed, 0);
    }

    fn host_record(host_seed: u8, relay: &str, ts: u64) -> Payload {
        Payload::Infrastructure(Infrastructure {
            v: Version::CURRENT,
            host: *secret_key(host_seed).public().as_bytes(),
            relay: wire::RelayUrl::try_from(relay.to_owned()).unwrap(),
            addr: Addr::from_endpoint_report([relay.to_owned()], []),
            ts,
            cert: spec_certificate_bytes(host_seed),
            mac: ZERO_MAC,
            sig: UNSIGNED,
        })
    }

    #[tokio::test]
    async fn spec_3_14_a_host_hearing_its_own_earlier_record_learns_no_host_from_it() {
        let scratch = Scratch::new("own-record");
        scratch.configure_network();
        scratch.certify(HOST_SEED);
        let seen = Seen::default();
        let (stop, stopped) = stopped_on_signal();
        let plan = Plan {
            stopped,
            ..Plan::new(HOST_SEED)
        };

        let ((_printed, outcome), other_learned) =
            tokio::join!(run_planned(&scratch, plan, &seen), async {
                let host = bound_endpoint(&seen, 0).await;
                let peer = Peer::joined_through(PEER_SEED, &host).await;
                peer.topic
                    .broadcast(
                        &secret_key(HOST_SEED),
                        host_record(HOST_SEED, SPEC_RELAY_URL, REPLAYED_OWN_TS),
                    )
                    .await
                    .unwrap();
                peer.topic
                    .broadcast(
                        &secret_key(OTHER_HOST_SEED),
                        host_record(OTHER_HOST_SEED, OTHER_HOST_RELAY_URL, FROZEN_CLOCK_SECONDS),
                    )
                    .await
                    .unwrap();
                let other_learned =
                    saved_within(&scratch, OTHER_HOST_SEED, HOST_LEARNED_WAIT).await;
                stop.send(()).unwrap();
                other_learned
            });

        outcome.unwrap();
        assert_eq!(other_learned.unwrap().relay, OTHER_HOST_RELAY_URL);
        assert_eq!(scratch.saved_host(HOST_SEED), None);
    }

    #[tokio::test]
    async fn spec_3_3_a_network_with_no_configuration_file_here_is_refused_before_any_key_is_made()
    {
        let scratch = Scratch::new("unconfigured");
        let seen = Seen::default();

        let (printed, outcome) = run_once(&scratch, HOST_SEED, &seen).await;

        assert_eq!(printed, "");
        assert!(
            outcome
                .unwrap_err()
                .to_string()
                .contains(&format!("network {NETWORK} has no configuration file here"))
        );
        assert!(!scratch.key_file().exists());
    }

    #[tokio::test]
    async fn spec_3_3_a_record_counter_file_that_is_not_a_decimal_integer_and_one_newline_is_refused_before_serving()
     {
        let scratch = Scratch::new("bad-counter");
        scratch.configure_network();
        scratch.certify(HOST_SEED);
        let counter_file = scratch
            .directories
            .infrastructure_last_record_timestamp_file(NETWORK)
            .unwrap();
        let seen = Seen::default();

        for unreadable in ["", "\n", "12", "+12\n", "12 \n", "twelve\n"] {
            fs::write(&counter_file, unreadable).unwrap();

            let (_printed, outcome) = run_once(&scratch, HOST_SEED, &seen).await;

            assert!(
                outcome
                    .unwrap_err()
                    .to_string()
                    .contains("the last record counter must be a decimal integer")
            );
        }
        assert!(seen.relay_sites.lock().unwrap().is_empty());
    }

    #[derive(Debug, clap::Parser)]
    struct InfrastructureArguments {
        #[command(subcommand)]
        command: InfrastructureCommand,
    }

    fn parsed_domain(domain: &str) -> Result<String, clap::Error> {
        use clap::Parser;
        InfrastructureArguments::try_parse_from([
            "estafette",
            "run-infrastructure",
            NETWORK,
            SPEC_BIND_ADDRESS_TEXT,
            domain,
        ])
        .map(|arguments| match arguments.command {
            InfrastructureCommand::RunInfrastructure { domain, .. } => {
                domain.wire_url.as_str().to_owned()
            }
        })
    }

    #[test]
    fn spec_3_14_the_domain_is_refused_unless_it_is_the_whole_host_of_a_relay_url_within_128_bytes()
    {
        let longest = format!(
            "{}.{}",
            "a".repeat(LONGEST_LABEL),
            "b".repeat(LONGEST_DOMAIN - LONGEST_LABEL - 1)
        );
        let one_over = format!("{longest}c");

        assert_eq!(parsed_domain(DOMAIN).unwrap(), SPEC_RELAY_URL);
        assert_eq!(
            parsed_domain(&longest).unwrap().len(),
            wire::MAX_RELAY_URL_BYTES
        );
        for refused in [
            one_over.as_str(),
            "Relay-A.estafette.invalid",
            "relay-a.estafette.invalid:8443",
            "relay-a.estafette.invalid/path",
            "user@relay-a.estafette.invalid",
            "",
        ] {
            assert_eq!(
                parsed_domain(refused).unwrap_err().kind(),
                clap::error::ErrorKind::ValueValidation
            );
        }
    }
}
