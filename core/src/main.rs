use std::fmt;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use estafette::cli::cards::{self, CardCommand, RegistryArgs};
use estafette::cli::network::{self, NetworkCommand, NetworkCommandError};
use estafette::cli::observer::{
    self, ObserverCommand, ObserverCommandError, ObserverEffects, OperatorEffects,
};
use estafette::config::{ConfigError, NetworkConfig};
use estafette::dirs::{Directories, DirectoriesError, OperatingSystemFilesystem};
use estafette::errors::ExitStatus;
use estafette::inbox::socket::{InboxSocket, InboxSocketError, OperatingSystemProcessTable};
use estafette::mcp::{self, ServedNetwork, ServerEffects, ServerRecord};
use estafette::net::endpoint::{GateFailure, NodeEndpoint};
use estafette::node::{
    Exit, Network, NodeEffects, NodeError, NodeRecord, RuntimeShutdown, ShutdownSignal,
    relay_answers,
};
use estafette::sidecar::ThreadSleep;
use estafette::wire::Nonce;
use iroh::SecretKey;
use tokio::runtime::Runtime;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::time::Instant;

const MILLISECONDS_PER_SECOND: u64 = 1000;
const RANDOM_BYTES_PER_SECRET_KEY: usize = 32;
/// The longest an awaited shutdown waits for the runtime's blocking work, which after SIGINT or SIGHUP includes a read of standard input that may never return.
const AWAITED_SHUTDOWN_LIMIT: Duration = Duration::from_millis(1000);

#[derive(Debug, Parser)]
#[command(name = "estafette")]
struct EstafetteArguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(flatten)]
    Cards(CardCommand),
    /// Create or join a network, add a host to it, print its invitation, or certify a host
    Network {
        #[command(subcommand)]
        command: NetworkCommand,
    },
    #[command(flatten)]
    Observer(ObserverCommand),
    /// Serve the six MCP tools of one network on standard input and output, reading its configuration and cards; refuses a network whose configuration is missing or invalid
    Mcp {
        #[command(flatten)]
        registry: RegistryArgs,
    },
}

#[derive(Debug)]
enum CommandLineFailure {
    Directories(DirectoriesError),
    ClockBeforeUnixEpoch,
    Network(NetworkCommandError),
    NetworkConfig(ConfigError),
    NetworkRefused(NodeError),
    Runtime(io::Error),
    InboxSocket(InboxSocketError),
    Signals(io::Error),
    Observer(ObserverCommandError),
}

impl fmt::Display for CommandLineFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directories(source) => write!(formatter, "{source}"),
            Self::ClockBeforeUnixEpoch => write!(
                formatter,
                "the system clock reads a time before 1970-01-01: set the clock and run the command again"
            ),
            Self::Network(source) => write!(formatter, "{source}"),
            Self::NetworkConfig(source) => write!(formatter, "{source}"),
            Self::NetworkRefused(source) => write!(formatter, "{source}"),
            Self::Runtime(source) => write!(formatter, "cannot start the async runtime: {source}"),
            Self::InboxSocket(source) => write!(formatter, "{source}"),
            Self::Signals(source) => {
                write!(formatter, "cannot listen for shutdown signals: {source}")
            }
            Self::Observer(source) => write!(formatter, "{source}"),
        }
    }
}

fn main() -> ExitCode {
    let status = match EstafetteArguments::parse().command {
        Command::Cards(command) => cards::run(
            command,
            |name: &str| std::env::var_os(name),
            &OperatingSystemFilesystem,
            |seed| *seed = SecretKey::generate().to_bytes(),
            &mut io::stdout(),
            &mut io::stderr(),
        ),
        Command::Network { command } => {
            let mut result = io::stdout().lock();
            let mut record = io::stderr().lock();
            match run_network_command(&command, &mut result, &mut record) {
                Ok(()) => ExitStatus::Success,
                Err(failure) => {
                    drop(writeln!(record, "{failure}"));
                    ExitStatus::Failure
                }
            }
        }
        Command::Observer(command) => {
            let mut record = io::stderr().lock();
            match run_observer_command(&command, &mut io::stdout()) {
                Ok(()) => ExitStatus::Success,
                Err(failure) => {
                    drop(writeln!(record, "{failure}"));
                    ExitStatus::Failure
                }
            }
        }
        Command::Mcp { registry } => serve_mcp(registry).unwrap_or_else(|failure| {
            drop(writeln!(io::stderr(), "{failure}"));
            ExitStatus::Failure
        }),
    };
    ExitCode::from(u8::from(status))
}

/// Serves the MCP tools of the network until the session ends, building every side effect the server runs on.
fn serve_mcp(registry: RegistryArgs) -> Result<ExitStatus, CommandLineFailure> {
    let directories = Directories::establish(
        |name| std::env::var_os(name),
        &OperatingSystemFilesystem,
        registry.cards_dir,
    )
    .map_err(CommandLineFailure::Directories)?;
    let config_path = directories
        .network_config_file(registry.network.as_str())
        .map_err(CommandLineFailure::Directories)?;
    let config = NetworkConfig::load(&config_path).map_err(CommandLineFailure::NetworkConfig)?;
    Network::new(config.clone(), config_path.clone())
        .map_err(CommandLineFailure::NetworkRefused)?;
    unix_seconds_of(SystemTime::now())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CommandLineFailure::Runtime)?;
    let process_id = std::process::id();
    let exit = runtime.block_on(async move {
        let inbox = InboxSocket::start(&directories, process_id, &OperatingSystemProcessTable)
            .map_err(CommandLineFailure::InboxSocket)?;
        let signals = forward_shutdown_signals().map_err(CommandLineFailure::Signals)?;
        let served = ServedNetwork {
            inbox_path: directories.inbox_socket_file(process_id),
            directories,
            network: registry.network,
            config,
            config_path,
        };
        Ok(mcp::serve(
            tokio::io::stdin(),
            tokio::io::stdout(),
            signals,
            served,
            server_effects(Arc::new(inbox)),
        )
        .await)
    })?;
    shut_down(runtime, exit);
    Ok(exit_status_of(exit))
}

/// Forwards SIGTERM, SIGINT and SIGHUP, as they arrive, to the session.
fn forward_shutdown_signals() -> io::Result<mpsc::UnboundedReceiver<ShutdownSignal>> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let (signals, received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let arrived = tokio::select! {
                Some(()) = terminate.recv() => ShutdownSignal::Terminate,
                Some(()) = interrupt.recv() => ShutdownSignal::Interrupt,
                Some(()) = hangup.recv() => ShutdownSignal::Hangup,
                else => return,
            };
            if signals.send(arrived).is_err() {
                return;
            }
        }
    });
    Ok(received)
}

/// The side effects of the MCP server and its node: the clocks, a secure random source, the endpoint binding, the stderr run record, and the filesystem.
fn server_effects(inbox: Arc<InboxSocket>) -> ServerEffects {
    ServerEffects {
        node: NodeEffects {
            clock: Arc::new(wall_clock_milliseconds),
            monotonic: Arc::new(tokio::time::Instant::now),
            fill_probe_nonce: Arc::new(|nonce: &mut Nonce| fill_with_secure_random_bytes(nonce)),
            report: Arc::new(|record: NodeRecord| drop(writeln!(io::stderr(), "{record}"))),
            inbox,
            probe_relay: Arc::new(|relay| Box::pin(relay_answers(relay))),
        },
        bind_endpoint: Arc::new(|card_key, gate_key, relays| {
            Box::pin(NodeEndpoint::bind(
                card_key,
                gate_key,
                relays,
                fill_with_secure_random_bytes,
                |failure| report_gate_closing(&failure),
            ))
        }),
        registry: Arc::new(OperatingSystemFilesystem),
        pause_between_lock_tries: Arc::new(std::thread::sleep),
        report: Arc::new(|record: ServerRecord| drop(writeln!(io::stderr(), "{record}"))),
    }
}

/// Milliseconds since the Unix epoch by this machine's clock, 0 for a clock set before it.
fn wall_clock_milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Fills the bytes from the operating system's secure random source, by way of fresh secret keys.
fn fill_with_secure_random_bytes<const LENGTH: usize>(bytes: &mut [u8; LENGTH]) {
    for chunk in bytes.chunks_mut(RANDOM_BYTES_PER_SECRET_KEY) {
        for (byte, random) in chunk.iter_mut().zip(SecretKey::generate().to_bytes()) {
            *byte = random;
        }
    }
}

/// Writes the reason a gossip connection was closed at the network secret gate to the run record.
fn report_gate_closing(failure: &GateFailure) {
    drop(writeln!(
        io::stderr(),
        "a gossip connection was closed at the network secret gate: {failure}"
    ));
}

/// Shuts the runtime down as the session asked: awaited within its limit, or in the background, so a pending read of standard input never holds the exit.
fn shut_down(runtime: Runtime, exit: Exit) {
    match exit {
        Exit::Stopped(RuntimeShutdown::Awaited) => {
            runtime.shutdown_timeout(AWAITED_SHUTDOWN_LIMIT);
        }
        Exit::Stopped(RuntimeShutdown::InBackground) | Exit::AtOnce => {
            runtime.shutdown_background();
        }
    }
}

/// The process exit of a session: success once it stopped, failure when a second signal ended it at once.
const fn exit_status_of(exit: Exit) -> ExitStatus {
    match exit {
        Exit::Stopped(RuntimeShutdown::Awaited | RuntimeShutdown::InBackground) => {
            ExitStatus::Success
        }
        Exit::AtOnce => ExitStatus::Failure,
    }
}

fn run_network_command(
    command: &NetworkCommand,
    result: &mut impl Write,
    record: &mut impl Write,
) -> Result<(), CommandLineFailure> {
    let directories = Directories::establish(
        |name| std::env::var_os(name),
        &OperatingSystemFilesystem,
        None,
    )
    .map_err(CommandLineFailure::Directories)?;
    let now_unix_seconds = unix_seconds_of(SystemTime::now())?;
    network::run(
        command,
        &directories,
        &OperatingSystemFilesystem,
        |bytes| *bytes = SecretKey::generate().to_bytes(),
        now_unix_seconds,
        result,
        record,
    )
    .map_err(CommandLineFailure::Network)
}

fn run_observer_command(
    command: &ObserverCommand,
    result: &mut (impl Write + Send),
) -> Result<(), CommandLineFailure> {
    let directories = Arc::new(
        Directories::establish(
            |name| std::env::var_os(name),
            &OperatingSystemFilesystem,
            None,
        )
        .map_err(CommandLineFailure::Directories)?,
    );
    let started_at_milliseconds = unix_milliseconds_of(SystemTime::now())?;
    let started = Instant::now();
    let clock = move || {
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        started_at_milliseconds.saturating_add(elapsed)
    };
    let (effects, operator_effects) = observer_effects(Arc::clone(&directories), clock);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CommandLineFailure::Runtime)?;
    runtime
        .block_on(observer::run(
            command,
            &directories,
            &OperatingSystemFilesystem,
            &ThreadSleep,
            effects,
            operator_effects,
            result,
        ))
        .map_err(CommandLineFailure::Observer)
}

fn observer_effects(
    directories: Arc<Directories>,
    clock: impl Fn() -> u64 + Send + Sync + 'static,
) -> (ObserverEffects, OperatorEffects) {
    let effects = ObserverEffects {
        clock: Arc::new(clock),
        monotonic: Arc::new(Instant::now),
        fill_seed: Arc::new(|seed| *seed = SecretKey::generate().to_bytes()),
        bind_endpoint: Box::new(|key, gate_key, relays| {
            Box::pin(NodeEndpoint::bind(
                key,
                gate_key,
                relays,
                |nonce| *nonce = SecretKey::generate().to_bytes(),
                |failure| drop(writeln!(io::stderr(), "{failure}")),
            ))
        }),
        interrupted: Box::pin(async {
            drop(tokio::signal::ctrl_c().await);
        }),
    };
    let operator_effects = OperatorEffects {
        fill_probe_nonce: Arc::new(|nonce: &mut Nonce| {
            for (slot, byte) in nonce.iter_mut().zip(SecretKey::generate().to_bytes()) {
                *slot = byte;
            }
        }),
        report: Arc::new(|record| drop(writeln!(io::stderr(), "{record}"))),
        probe_relay: Arc::new(|relay| Box::pin(relay_answers(relay))),
        start_inbox: Box::new(move || {
            InboxSocket::start(
                &directories,
                std::process::id(),
                &OperatingSystemProcessTable,
            )
            .map(Arc::new)
        }),
    };
    (effects, operator_effects)
}

fn unix_milliseconds_of(instant: SystemTime) -> Result<u64, CommandLineFailure> {
    instant
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .map_err(|_before_epoch| CommandLineFailure::ClockBeforeUnixEpoch)
}

fn unix_seconds_of(instant: SystemTime) -> Result<u64, CommandLineFailure> {
    unix_milliseconds_of(instant).map(|milliseconds| milliseconds / MILLISECONDS_PER_SECOND)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const SECONDS_AFTER_EPOCH: u64 = 1_700_000_000;
    const MILLISECONDS_AFTER_EPOCH: u64 = 1_700_000_000_123;

    #[test]
    fn spec_3_13_a_clock_after_the_unix_epoch_gives_its_seconds() {
        let instant = UNIX_EPOCH + Duration::from_secs(SECONDS_AFTER_EPOCH);

        assert_eq!(unix_seconds_of(instant).unwrap(), SECONDS_AFTER_EPOCH);
    }

    #[test]
    fn spec_3_13_a_clock_after_the_unix_epoch_gives_its_milliseconds() {
        let instant = UNIX_EPOCH + Duration::from_millis(MILLISECONDS_AFTER_EPOCH);

        assert_eq!(
            unix_milliseconds_of(instant).unwrap(),
            MILLISECONDS_AFTER_EPOCH
        );
    }

    #[test]
    fn spec_3_13_a_clock_before_the_unix_epoch_is_refused_not_read_as_zero() {
        let instant = UNIX_EPOCH - Duration::from_secs(1);

        let refusal = unix_seconds_of(instant).unwrap_err();

        assert!(matches!(refusal, CommandLineFailure::ClockBeforeUnixEpoch));
        assert!(refusal.to_string().starts_with("the system clock reads"));
    }

    #[test]
    fn spec_3_9_a_session_that_stopped_exits_zero_and_one_ended_at_once_by_a_second_signal_exits_one()
     {
        let expected = [
            (Exit::Stopped(RuntimeShutdown::Awaited), ExitStatus::Success),
            (
                Exit::Stopped(RuntimeShutdown::InBackground),
                ExitStatus::Success,
            ),
            (Exit::AtOnce, ExitStatus::Failure),
        ];
        for (exit, status) in expected {
            assert_eq!(exit_status_of(exit), status, "{exit:?}");
        }
    }
}
