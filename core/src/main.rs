use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use estafette::cli::cards::{self, CardCommand, RegistryArgs};
use estafette::cli::doctor::{self, DoctorArgs, DoctorError};
#[cfg(feature = "relay")]
use estafette::cli::infra::{
    self, InfrastructureCommand, InfrastructureCommandError, InfrastructureEffects, StopRequested,
};
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
use estafette::sidecar::{self, ThreadSleep};
use estafette::wire::Nonce;
use iroh::SecretKey;
use tokio::runtime::Runtime;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// The milliseconds in one second, to read a millisecond clock as seconds.
const MILLISECONDS_PER_SECOND: u64 = 1000;
/// The bytes one fresh secret key gives the secure random fill.
const RANDOM_BYTES_PER_SECRET_KEY: usize = 32;
/// The longest an awaited shutdown waits for the runtime's blocking work, which after SIGINT or SIGHUP includes a read of standard input that may never return.
const AWAITED_SHUTDOWN_LIMIT: Duration = Duration::from_millis(1000);

const PROGRAM_PURPOSE: &str = "Messaging between agents across machines and harnesses";
const INTERRUPT_SIGNAL: &str = "SIGINT";
const TERMINATE_SIGNAL: &str = "SIGTERM";
const HANGUP_SIGNAL: &str = "SIGHUP";

#[derive(Debug, Parser)]
#[command(name = "estafette", about = PROGRAM_PURPOSE, long_about = PROGRAM_PURPOSE)]
struct EstafetteArguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(flatten)]
    Cards(CardCommand),
    #[cfg(feature = "relay")]
    #[command(flatten)]
    Infrastructure(InfrastructureCommand),
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
    /// Check the configuration file permissions, each card, each sidecar, each known relay and the visibility of a peer, print one line per check, and exit 3 when any check fails
    Doctor(DoctorArgs),
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
    Observer(ObserverCommandError),
    SignalNotHeard {
        signal: &'static str,
        source: io::Error,
    },
    #[cfg(feature = "relay")]
    Infrastructure(InfrastructureCommandError),
    Doctor(DoctorError),
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
            Self::Observer(source) => write!(formatter, "{source}"),
            Self::SignalNotHeard { signal, source } => write!(
                formatter,
                "cannot listen for {signal}, so the command does not start: {source}"
            ),
            #[cfg(feature = "relay")]
            Self::Infrastructure(source) => write!(formatter, "{source}"),
            Self::Doctor(source) => write!(formatter, "{source}"),
        }
    }
}

fn main() -> ExitCode {
    let status = run_command(EstafetteArguments::parse().command, |name| {
        std::env::var_os(name)
    });
    ExitCode::from(u8::from(status))
}

/// Runs the command on the process's streams, reading the environment through the given lookup, and says how the process ends.
fn run_command(command: Command, environment: impl Fn(&str) -> Option<OsString>) -> ExitStatus {
    match command {
        Command::Cards(command) => cards::run(
            command,
            &environment,
            &OperatingSystemFilesystem,
            fill_with_secure_random_bytes,
            &mut io::stdout(),
            &mut io::stderr(),
        ),
        #[cfg(feature = "relay")]
        Command::Infrastructure(command) => {
            run_infrastructure_command(command, &environment, &mut io::stdout(), &mut io::stderr())
        }
        Command::Network { command } => exit_status_after(
            run_network_command(&command, &environment, &mut io::stdout(), &mut io::stderr())
                .map(|()| ExitStatus::Success),
            &mut io::stderr(),
        ),
        Command::Observer(command) => exit_status_after(
            run_observer_command(&command, &environment, &mut io::stdout())
                .map(|()| ExitStatus::Success),
            &mut io::stderr(),
        ),
        Command::Mcp { registry } => {
            exit_status_after(serve_mcp(registry, &environment), &mut io::stderr())
        }
        Command::Doctor(arguments) => exit_status_after(
            run_doctor_command(&arguments, &environment, &mut io::stdout()),
            &mut io::stderr(),
        ),
    }
}

/// Says how the process ends after a command: the status it reached, or failure after writing the failure's message to the record.
fn exit_status_after(
    outcome: Result<ExitStatus, CommandLineFailure>,
    record: &mut impl Write,
) -> ExitStatus {
    outcome.unwrap_or_else(|failure| {
        drop(writeln!(record, "{failure}"));
        ExitStatus::Failure
    })
}

/// Serves the MCP tools of the network until the session ends, building every side effect the server runs on.
fn serve_mcp(
    registry: RegistryArgs,
    environment: impl Fn(&str) -> Option<OsString>,
) -> Result<ExitStatus, CommandLineFailure> {
    let directories =
        Directories::establish(environment, &OperatingSystemFilesystem, registry.cards_dir)
            .map_err(CommandLineFailure::Directories)?;
    let config_path = directories
        .network_config_file(registry.network.as_str())
        .map_err(CommandLineFailure::Directories)?;
    let config = NetworkConfig::load(&config_path).map_err(CommandLineFailure::NetworkConfig)?;
    Network::new(config.clone(), config_path.clone())
        .map_err(CommandLineFailure::NetworkRefused)?;
    ensure_clock_after_unix_epoch()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CommandLineFailure::Runtime)?;
    let process_id = std::process::id();
    let exit = runtime.block_on(async move {
        let inbox = InboxSocket::start(
            &directories,
            process_id,
            &OperatingSystemProcessTable,
            |failure| report_accept_failure(&failure),
        )
        .map_err(CommandLineFailure::InboxSocket)?;
        let signals = forward_shutdown_signals()?;
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
fn forward_shutdown_signals() -> Result<mpsc::UnboundedReceiver<ShutdownSignal>, CommandLineFailure>
{
    let mut terminate = listen_for(SignalKind::terminate(), TERMINATE_SIGNAL)?;
    let mut interrupt = listen_for(SignalKind::interrupt(), INTERRUPT_SIGNAL)?;
    let mut hangup = listen_for(SignalKind::hangup(), HANGUP_SIGNAL)?;
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
            clock: Arc::new(recording_wall_clock()),
            monotonic: Arc::new(tokio::time::Instant::now),
            fill_probe_nonce: Arc::new(|nonce: &mut Nonce| fill_with_secure_random_bytes(nonce)),
            report: Arc::new(|record: NodeRecord| report_run_record(&record)),
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
        is_card_held: Arc::new(|sidecar_path: &Path| {
            sidecar::is_held(sidecar_path).is_ok_and(|held| held)
        }),
        report: Arc::new(|record: ServerRecord| report_run_record(&record)),
    }
}

/// The wall clock handed to a node: milliseconds since the Unix epoch, holding its last reading when the machine's clock is set back before the epoch.
fn recording_wall_clock() -> impl Fn() -> u64 + Send + Sync + 'static {
    let last_reading = AtomicU64::new(0);
    move || {
        wall_clock_reading(SystemTime::now(), &last_reading, |failure| {
            report_run_record(failure);
        })
    }
}

/// The milliseconds since the Unix epoch at `now`, remembered as the last reading; a `now` before the epoch is recorded and answered by the last reading.
fn wall_clock_reading(
    now: SystemTime,
    last_reading: &AtomicU64,
    record: impl Fn(&CommandLineFailure),
) -> u64 {
    match unix_milliseconds_of(now) {
        Ok(milliseconds) => {
            last_reading.store(milliseconds, Ordering::Relaxed);
            milliseconds
        }
        Err(failure) => {
            record(&failure);
            last_reading.load(Ordering::Relaxed)
        }
    }
}

/// Refuses a machine clock reading a time before the Unix epoch.
fn ensure_clock_after_unix_epoch() -> Result<(), CommandLineFailure> {
    unix_milliseconds_of(SystemTime::now()).map(|_milliseconds| ())
}

/// Fills the bytes from the operating system's secure random source, by way of fresh secret keys.
fn fill_with_secure_random_bytes<const LENGTH: usize>(bytes: &mut [u8; LENGTH]) {
    for chunk in bytes.chunks_mut(RANDOM_BYTES_PER_SECRET_KEY) {
        for (byte, random) in chunk.iter_mut().zip(SecretKey::generate().to_bytes()) {
            *byte = random;
        }
    }
}

/// Writes the reason a gossip connection was closed at the network secret gate to the record.
fn write_gate_closing(record: &mut impl Write, failure: &GateFailure) {
    drop(writeln!(
        record,
        "a gossip connection was closed at the network secret gate: {failure}"
    ));
}

/// Writes the reason a gossip connection was closed at the network secret gate to the run record on stderr.
fn report_gate_closing(failure: &GateFailure) {
    write_gate_closing(&mut io::stderr(), failure);
}

/// Writes the failure to accept a client of the inbox socket to the run record on stderr.
fn report_accept_failure(failure: &io::Error) {
    report_run_record(&format!(
        "the inbox socket could not accept a client: {failure}"
    ));
}

/// Writes one entry of a run record to stderr.
fn report_run_record(entry: &impl fmt::Display) {
    drop(writeln!(io::stderr(), "{entry}"));
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
    environment: impl Fn(&str) -> Option<OsString>,
    result: &mut impl Write,
    record: &mut impl Write,
) -> Result<(), CommandLineFailure> {
    let directories = Directories::establish(environment, &OperatingSystemFilesystem, None)
        .map_err(CommandLineFailure::Directories)?;
    let now_unix_seconds = unix_seconds_of(SystemTime::now())?;
    network::run(
        command,
        &directories,
        &OperatingSystemFilesystem,
        fill_with_secure_random_bytes,
        now_unix_seconds,
        result,
        record,
    )
    .map_err(CommandLineFailure::Network)
}

struct ObserverRuntime {
    directories: Arc<Directories>,
    runtime: tokio::runtime::Runtime,
    effects: ObserverEffects,
    operator_effects: OperatorEffects,
}

/// What SIGINT does while an observer runtime runs its command.
#[derive(Debug, Clone, Copy)]
enum Interruption {
    /// The command listens for SIGINT and ends its own run when it arrives.
    EndsTheCommand,
    /// SIGINT keeps its default and ends the process, for a command that never waits for it.
    EndsTheProcess,
}

/// How an observer command takes SIGINT: show peers and tail end their observation, test send leaves SIGINT to end the process.
const fn interruption_of(command: &ObserverCommand) -> Interruption {
    match command {
        ObserverCommand::ShowPeers { .. } | ObserverCommand::Tail { .. } => {
            Interruption::EndsTheCommand
        }
        ObserverCommand::TestSend { .. } => Interruption::EndsTheProcess,
    }
}

fn observer_runtime(
    environment: impl Fn(&str) -> Option<OsString>,
    cards_dir: Option<PathBuf>,
    interruption: Interruption,
) -> Result<ObserverRuntime, CommandLineFailure> {
    let directories = Arc::new(
        Directories::establish(environment, &OperatingSystemFilesystem, cards_dir)
            .map_err(CommandLineFailure::Directories)?,
    );
    let started_at_milliseconds = unix_milliseconds_of(SystemTime::now())?;
    let started = Instant::now();
    let clock = move || {
        let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        started_at_milliseconds.saturating_add(elapsed)
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CommandLineFailure::Runtime)?;
    let interrupted = {
        let _inside_runtime = runtime.enter();
        interrupt_for(interruption)?
    };
    let (effects, operator_effects) =
        observer_effects(Arc::clone(&directories), clock, interrupted);
    Ok(ObserverRuntime {
        directories,
        runtime,
        effects,
        operator_effects,
    })
}

fn run_observer_command(
    command: &ObserverCommand,
    environment: impl Fn(&str) -> Option<OsString>,
    result: &mut (impl Write + Send),
) -> Result<(), CommandLineFailure> {
    let ObserverRuntime {
        directories,
        runtime,
        effects,
        operator_effects,
    } = observer_runtime(environment, command.cards_dir(), interruption_of(command))?;
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

fn run_doctor_command(
    arguments: &DoctorArgs,
    environment: impl Fn(&str) -> Option<OsString>,
    result: &mut impl Write,
) -> Result<ExitStatus, CommandLineFailure> {
    let ObserverRuntime {
        directories,
        runtime,
        effects,
        operator_effects,
    } = observer_runtime(
        environment,
        arguments.registry.cards_dir.clone(),
        Interruption::EndsTheProcess,
    )?;
    runtime
        .block_on(doctor::run(
            arguments,
            &directories,
            &OperatingSystemFilesystem,
            &ThreadSleep,
            effects,
            operator_effects,
            result,
        ))
        .map_err(CommandLineFailure::Doctor)
}

fn observer_effects(
    directories: Arc<Directories>,
    clock: impl Fn() -> u64 + Send + Sync + 'static,
    interrupted: Pin<Box<dyn Future<Output = ()> + Send>>,
) -> (ObserverEffects, OperatorEffects) {
    let effects = ObserverEffects {
        clock: Arc::new(clock),
        monotonic: Arc::new(Instant::now),
        fill_seed: Arc::new(|seed: &mut [u8; RANDOM_BYTES_PER_SECRET_KEY]| {
            fill_with_secure_random_bytes(seed);
        }),
        bind_endpoint: Box::new(|key, gate_key, relays| {
            Box::pin(NodeEndpoint::bind(
                key,
                gate_key,
                relays,
                fill_with_secure_random_bytes,
                |failure| report_gate_closing(&failure),
            ))
        }),
        interrupted,
    };
    let operator_effects = OperatorEffects {
        fill_probe_nonce: Arc::new(|nonce: &mut Nonce| fill_with_secure_random_bytes(nonce)),
        report: Arc::new(|record| report_run_record(&record)),
        probe_relay: Arc::new(|relay| Box::pin(relay_answers(relay))),
        start_inbox: Box::new(move || {
            InboxSocket::start(
                &directories,
                std::process::id(),
                &OperatingSystemProcessTable,
                |failure| report_accept_failure(&failure),
            )
            .map(Arc::new)
        }),
    };
    (effects, operator_effects)
}

/// Milliseconds since the Unix epoch at the instant, refusing an instant before it.
fn unix_milliseconds_of(instant: SystemTime) -> Result<u64, CommandLineFailure> {
    instant
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .map_err(|_before_epoch| CommandLineFailure::ClockBeforeUnixEpoch)
}

/// Whole seconds since the Unix epoch at the instant, refusing an instant before it.
fn unix_seconds_of(instant: SystemTime) -> Result<u64, CommandLineFailure> {
    unix_milliseconds_of(instant).map(|milliseconds| milliseconds / MILLISECONDS_PER_SECOND)
}

/// Runs the infrastructure command on a runtime of its own, writing a failure to `record`, and says how the process ends.
#[cfg(feature = "relay")]
fn run_infrastructure_command(
    command: InfrastructureCommand,
    environment: impl Fn(&str) -> Option<OsString>,
    result: &mut (impl Write + Send),
    record: &mut impl Write,
) -> ExitStatus {
    exit_status_after(
        serve_infrastructure(command, environment, result).map(|()| ExitStatus::Success),
        record,
    )
}

/// Resolves the directories, refuses a clock before the Unix epoch, and serves the host until a stop signal.
#[cfg(feature = "relay")]
fn serve_infrastructure(
    command: InfrastructureCommand,
    environment: impl Fn(&str) -> Option<OsString>,
    result: &mut (impl Write + Send),
) -> Result<(), CommandLineFailure> {
    let directories = Directories::establish(environment, &OperatingSystemFilesystem, None)
        .map_err(CommandLineFailure::Directories)?;
    ensure_clock_after_unix_epoch()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CommandLineFailure::Runtime)?;
    let stopped = {
        let _inside_runtime = runtime.enter();
        stop_requested()?
    };
    runtime
        .block_on(infra::run(
            command,
            &directories,
            &OperatingSystemFilesystem,
            infrastructure_effects(stopped),
            result,
        ))
        .map_err(CommandLineFailure::Infrastructure)
}

/// The side effects of the infrastructure command: the secure random source, the wall clock, the gated endpoint, the embedded relay, the stop signals and the run record on stderr.
#[cfg(feature = "relay")]
fn infrastructure_effects(stopped: StopRequested) -> InfrastructureEffects {
    InfrastructureEffects {
        fill_from_secure_source: Box::new(|seed: &mut [u8; RANDOM_BYTES_PER_SECRET_KEY]| {
            fill_with_secure_random_bytes(seed);
        }),
        clock: Arc::new(recording_wall_clock()),
        bind_endpoint: Box::new(|key, gate_key, relay| {
            Box::pin(NodeEndpoint::bind(
                key,
                gate_key,
                [relay],
                fill_with_secure_random_bytes,
                |closing: GateFailure| report_gate_closing(&closing),
            ))
        }),
        start_relay: Box::new(|site| Box::pin(infra::start_embedded_relay(site))),
        stopped,
        report: Arc::new(|record| report_run_record(&record)),
    }
}

/// The interrupt a command waits for: SIGINT heard from now on, or never, leaving SIGINT its default.
fn interrupt_for(
    interruption: Interruption,
) -> Result<Pin<Box<dyn Future<Output = ()> + Send>>, CommandLineFailure> {
    match interruption {
        Interruption::EndsTheCommand => interrupt_requested(SignalKind::interrupt()),
        Interruption::EndsTheProcess => Ok(Box::pin(std::future::pending())),
    }
}

/// Listens for the interrupt signal now, refusing to start when it cannot be heard, and resolves when it arrives.
fn interrupt_requested(
    kind: SignalKind,
) -> Result<Pin<Box<dyn Future<Output = ()> + Send>>, CommandLineFailure> {
    let mut interrupt = listen_for(kind, INTERRUPT_SIGNAL)?;
    Ok(Box::pin(async move {
        interrupt.recv().await;
    }))
}

/// Listens for the interrupt and terminate signals now, refusing to start when either cannot be heard, and resolves at the first of them.
#[cfg(feature = "relay")]
fn stop_requested() -> Result<StopRequested, CommandLineFailure> {
    let mut interrupt = listen_for(SignalKind::interrupt(), INTERRUPT_SIGNAL)?;
    let mut terminate = listen_for(SignalKind::terminate(), TERMINATE_SIGNAL)?;
    Ok(Box::pin(async move {
        tokio::select! {
            _interrupted = interrupt.recv() => {}
            _terminated = terminate.recv() => {}
        }
    }))
}

/// The stream of one signal, refusing a signal this process cannot listen for.
fn listen_for(kind: SignalKind, name: &'static str) -> Result<Signal, CommandLineFailure> {
    signal(kind).map_err(|source| CommandLineFailure::SignalNotHeard {
        signal: name,
        source,
    })
}

#[cfg(test)]
#[path = "seeded_draws.rs"]
mod seeded_draws;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use std::sync::atomic::AtomicBool;

    use clap::CommandFactory;
    #[cfg(feature = "relay")]
    use estafette::crypto::KEY_LEN;

    use super::*;
    use crate::seeded_draws::Draws;

    const SECONDS_AFTER_EPOCH: u64 = 1_700_000_000;
    const MILLISECONDS_AFTER_EPOCH: u64 = 1_700_000_000_123;
    const BLOCKING_READ_THAT_NEVER_RETURNS_IN_TIME: Duration = Duration::from_secs(10);
    const AWAITED_SHUTDOWN_ENDS_WITHIN: Duration = Duration::from_millis(2000);
    const SPEC_PROGRAM_PURPOSE: &str = "Messaging between agents across machines and harnesses";

    /// A scratch directory tree that is removed when this value is dropped, whether the test passed or failed.
    struct ScratchRoot(PathBuf);

    impl Drop for ScratchRoot {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.0));
        }
    }

    #[test]
    fn spec_3_9_an_awaited_shutdown_ends_while_a_blocking_read_of_standard_input_is_still_pending()
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (started, blocking_read_started) = std::sync::mpsc::channel();
        drop(runtime.spawn_blocking(move || {
            started.send(()).unwrap();
            std::thread::sleep(BLOCKING_READ_THAT_NEVER_RETURNS_IN_TIME);
        }));
        blocking_read_started.recv().unwrap();
        let shutting_down = Instant::now();

        shut_down(runtime, Exit::Stopped(RuntimeShutdown::Awaited));

        assert!(shutting_down.elapsed() < AWAITED_SHUTDOWN_ENDS_WITHIN);
    }

    #[test]
    fn spec_3_13_the_program_help_opens_with_what_estafette_does_whatever_subcommands_it_flattens()
    {
        let help = EstafetteArguments::command().render_help().to_string();

        assert_eq!(help.lines().next(), Some(SPEC_PROGRAM_PURPOSE));
    }

    #[test]
    fn spec_3_13_a_clock_after_the_unix_epoch_gives_its_seconds() {
        let instant = UNIX_EPOCH + Duration::from_secs(SECONDS_AFTER_EPOCH);

        assert_eq!(unix_seconds_of(instant).unwrap(), SECONDS_AFTER_EPOCH);
    }

    #[test]
    fn spec_3_13_a_clock_before_the_unix_epoch_is_refused_not_read_as_zero() {
        let instant = UNIX_EPOCH - Duration::from_secs(1);

        let refusal = unix_seconds_of(instant).unwrap_err();

        assert!(matches!(refusal, CommandLineFailure::ClockBeforeUnixEpoch));
        assert!(refusal.to_string().starts_with("the system clock reads"));
    }

    #[test]
    fn spec_3_1_a_clock_after_the_unix_epoch_gives_its_milliseconds_and_one_before_it_is_refused() {
        let after = UNIX_EPOCH + Duration::from_millis(MILLISECONDS_AFTER_EPOCH);
        let before = UNIX_EPOCH - Duration::from_millis(1);

        assert_eq!(
            unix_milliseconds_of(after).unwrap(),
            MILLISECONDS_AFTER_EPOCH
        );
        assert!(matches!(
            unix_milliseconds_of(before).unwrap_err(),
            CommandLineFailure::ClockBeforeUnixEpoch
        ));
    }

    const PROPERTY_CASES: u64 = 200;
    const MAXIMUM_STEPS: u64 = 20;
    const MAXIMUM_HELD_BYTES: u64 = 40;
    const MILLISECONDS_RANGE: u64 = 4_000_000_000_000;
    const SIDES_OF_THE_EPOCH: u64 = 2;
    const BYTE_VALUES: u64 = 256;

    fn held_bytes(draws: &mut Draws) -> Vec<u8> {
        (0..draws.below(MAXIMUM_HELD_BYTES))
            .map(|_byte| u8::try_from(draws.below(BYTE_VALUES)).unwrap())
            .collect()
    }

    #[test]
    fn spec_3_1_a_wall_clock_reading_is_the_latest_one_at_or_after_the_epoch_and_each_one_before_it_is_recorded()
     {
        for case in 0..PROPERTY_CASES {
            let last_reading = AtomicU64::new(0);
            let recorded = AtomicU64::new(0);
            let mut latest_valid_reading = 0;
            let mut before_epoch_readings = 0_u64;
            let mut draws = Draws::seeded(case);
            for step in 0..=draws.below(MAXIMUM_STEPS) {
                let milliseconds = draws.below(MILLISECONDS_RANGE);
                let at_or_after_epoch = draws.below(SIDES_OF_THE_EPOCH) == 0;
                let instant = if at_or_after_epoch {
                    UNIX_EPOCH + Duration::from_millis(milliseconds)
                } else {
                    UNIX_EPOCH - Duration::from_millis(milliseconds.saturating_add(1))
                };

                let reading = wall_clock_reading(instant, &last_reading, |_failure| {
                    recorded.fetch_add(1, Ordering::SeqCst);
                });

                if at_or_after_epoch {
                    latest_valid_reading = milliseconds;
                } else {
                    before_epoch_readings = before_epoch_readings.saturating_add(1);
                }
                assert_eq!(reading, latest_valid_reading, "case {case} step {step}");
                assert_eq!(
                    recorded.load(Ordering::SeqCst),
                    before_epoch_readings,
                    "case {case} step {step}"
                );
            }
        }
    }

    #[test]
    fn spec_3_6_a_gate_closing_appends_exactly_one_line_after_whatever_the_record_holds() {
        for case in 0..PROPERTY_CASES {
            let failure = if case.is_multiple_of(2) {
                GateFailure::WrongProof
            } else {
                GateFailure::Timeout
            };
            let held = held_bytes(&mut Draws::seeded(case));
            let mut record = held.clone();

            write_gate_closing(&mut record, &failure);

            let mut expected = held;
            expected.extend(
                format!("a gossip connection was closed at the network secret gate: {failure}\n")
                    .into_bytes(),
            );
            assert_eq!(record, expected, "case {case}");
        }
    }

    #[test]
    fn spec_3_13_a_command_ends_with_the_status_it_reached_or_failure_after_one_line_of_its_message()
     {
        for case in 0..PROPERTY_CASES {
            let outcome: Result<ExitStatus, CommandLineFailure> = match case % 4 {
                0 => Ok(ExitStatus::Success),
                1 => Ok(ExitStatus::Failure),
                2 => Ok(ExitStatus::DoctorFoundFailedCheck),
                _ => Err(CommandLineFailure::Runtime(io::Error::other(format!(
                    "failure of case {case}"
                )))),
            };
            let (expected_status, expected_line) = match &outcome {
                Ok(status) => (*status, String::new()),
                Err(failure) => (ExitStatus::Failure, format!("{failure}\n")),
            };
            let held = held_bytes(&mut Draws::seeded(case));
            let mut record = held.clone();

            let status = exit_status_after(outcome, &mut record);

            let mut expected_record = held;
            expected_record.extend(expected_line.into_bytes());
            assert_eq!(status, expected_status, "case {case}");
            assert_eq!(record, expected_record, "case {case}");
        }
    }

    #[test]
    fn spec_3_13_standard_error_stays_writable_by_another_thread_while_a_command_runs() {
        const OTHER_THREAD_WRITES_WITHIN: Duration = Duration::from_millis(2000);
        const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
        const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
        const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
        const SCRATCH_PREFIX: &str = "estafette-stderr-writable";
        let root = std::env::temp_dir().join(format!("{SCRATCH_PREFIX}-{}", std::process::id()));
        let _scratch_removed_when_the_test_ends = ScratchRoot(root.clone());
        let (written, other_thread_wrote) = std::sync::mpsc::channel();
        let first_lookup = AtomicBool::new(true);
        let wrote_in_time = AtomicBool::new(false);
        let environment = |name: &str| {
            if first_lookup.swap(false, Ordering::SeqCst) {
                let written = written.clone();
                std::thread::spawn(move || {
                    writeln!(io::stderr(), "another thread writes while the command runs").unwrap();
                    written.send(()).unwrap();
                });
                wrote_in_time.store(
                    other_thread_wrote
                        .recv_timeout(OTHER_THREAD_WRITES_WITHIN)
                        .is_ok(),
                    Ordering::SeqCst,
                );
            }
            match name {
                CONFIG_HOME_VARIABLE => Some(root.join("c").into_os_string()),
                DATA_HOME_VARIABLE => Some(root.join("d").into_os_string()),
                RUNTIME_DIRECTORY_VARIABLE => Some(root.join("r").into_os_string()),
                _ => None,
            }
        };
        let Command::Observer(observer) =
            EstafetteArguments::try_parse_from(["estafette", "show-peers", "lab"])
                .unwrap()
                .command
        else {
            panic!("show-peers parses as an observer command");
        };

        let status = run_command(Command::Observer(observer), environment);

        assert_eq!(status, ExitStatus::Failure);
        assert!(wrote_in_time.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn spec_3_13_a_signal_that_cannot_be_heard_is_a_failure_naming_it_not_an_interrupt() {
        const UNLISTENABLE_SIGNAL_NUMBER: i32 = -1;

        let refusal = interrupt_requested(SignalKind::from_raw(UNLISTENABLE_SIGNAL_NUMBER))
            .err()
            .unwrap();

        assert!(matches!(
            refusal,
            CommandLineFailure::SignalNotHeard {
                signal: "SIGINT",
                ..
            }
        ));
        assert!(refusal.to_string().starts_with("cannot listen for SIGINT"));
    }

    #[test]
    fn spec_3_13_a_malformed_invitation_or_host_line_is_a_usage_error_that_exits_2() {
        const MALFORMED: &str = "not-an-invitation";
        const USAGE_ERROR_EXIT_CODE: i32 = 2;
        for arguments in [
            &["estafette", "network", "join", MALFORMED][..],
            &["estafette", "network", "add-host", "lab", MALFORMED][..],
        ] {
            let refusal =
                EstafetteArguments::try_parse_from(arguments.iter().copied()).unwrap_err();

            assert_eq!(refusal.exit_code(), USAGE_ERROR_EXIT_CODE, "{arguments:?}");
        }
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

    #[cfg(feature = "relay")]
    #[test]
    fn spec_3_14_run_infrastructure_without_a_certificate_prints_the_host_line_and_exits_1() {
        const SCRATCH_PREFIX: &str = "estafette-infra-main";
        const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
        const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
        const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
        const SECRET_SEED_BYTE: u8 = 3;
        let root = std::env::temp_dir().join(format!("{SCRATCH_PREFIX}-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&root));
        let base = root.clone();
        let environment = move |name: &str| match name {
            CONFIG_HOME_VARIABLE => Some(base.join("c").into_os_string()),
            DATA_HOME_VARIABLE => Some(base.join("d").into_os_string()),
            RUNTIME_DIRECTORY_VARIABLE => Some(base.join("r").into_os_string()),
            _ => None,
        };
        let created = EstafetteArguments::try_parse_from(["estafette", "network", "create", "lab"])
            .unwrap()
            .command;
        let Command::Network { command } = created else {
            panic!("network create parses as a network command");
        };
        let directories =
            Directories::establish(&environment, &OperatingSystemFilesystem, None).unwrap();
        network::run(
            &command,
            &directories,
            &OperatingSystemFilesystem,
            |bytes| *bytes = [SECRET_SEED_BYTE; KEY_LEN],
            SECONDS_AFTER_EPOCH,
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();
        let Command::Infrastructure(infrastructure) = EstafetteArguments::try_parse_from([
            "estafette",
            "run-infrastructure",
            "lab",
            "127.0.0.1",
            "relay.example",
        ])
        .unwrap()
        .command
        else {
            panic!("run-infrastructure parses as the infrastructure command");
        };
        let (mut printed, mut recorded) = (Vec::new(), Vec::new());

        let status =
            run_infrastructure_command(infrastructure, &environment, &mut printed, &mut recorded);

        assert_eq!(u8::from(status), 1);
        let printed = String::from_utf8(printed).unwrap();
        assert_eq!(printed.lines().count(), 1);
        assert!(printed.starts_with("estafette-host-1:"));
        assert!(
            String::from_utf8(recorded)
                .unwrap()
                .contains("holds no certificate")
        );
        drop(std::fs::remove_dir_all(&root));
    }
}
