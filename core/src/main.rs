#[cfg(feature = "relay")]
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use estafette::cli::cards::{self, CardCommand, RegistryArgs};
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
#[cfg(feature = "relay")]
use tokio::signal::unix::Signal;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::time::Instant;

const MILLISECONDS_PER_SECOND: u64 = 1000;
/// The bytes one fresh secret key gives the secure random fill.
const RANDOM_BYTES_PER_SECRET_KEY: usize = 32;
/// The longest an awaited shutdown waits for the runtime's blocking work, which after SIGINT or SIGHUP includes a read of standard input that may never return.
const AWAITED_SHUTDOWN_LIMIT: Duration = Duration::from_millis(1000);

const PROGRAM_PURPOSE: &str = "Messaging between agents across machines and harnesses";
#[cfg(feature = "relay")]
const INTERRUPT_SIGNAL: &str = "SIGINT";
#[cfg(feature = "relay")]
const TERMINATE_SIGNAL: &str = "SIGTERM";

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
    #[cfg(feature = "relay")]
    SignalNotHeard {
        signal: &'static str,
        source: io::Error,
    },
    #[cfg(feature = "relay")]
    Infrastructure(InfrastructureCommandError),
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
            #[cfg(feature = "relay")]
            Self::SignalNotHeard { signal, source } => write!(
                formatter,
                "cannot listen for {signal}, so the host could not be stopped cleanly and does not start: {source}"
            ),
            #[cfg(feature = "relay")]
            Self::Infrastructure(source) => write!(formatter, "{source}"),
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
        #[cfg(feature = "relay")]
        Command::Infrastructure(command) => run_infrastructure_command(
            command,
            |name: &str| std::env::var_os(name),
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
    unix_milliseconds_of(SystemTime::now())?;
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
        is_card_held: Arc::new(|sidecar_path: &Path| {
            sidecar::is_held(sidecar_path).is_ok_and(|held| held)
        }),
        report: Arc::new(|record: ServerRecord| drop(writeln!(io::stderr(), "{record}"))),
    }
}

/// Milliseconds since the Unix epoch by this machine's clock, which start refuses to run before; 0 only if the clock is later set back past it.
fn wall_clock_milliseconds() -> u64 {
    unix_milliseconds_of(SystemTime::now()).unwrap_or_default()
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
    match serve_infrastructure(command, environment, result) {
        Ok(()) => ExitStatus::Success,
        Err(failure) => {
            drop(writeln!(record, "{failure}"));
            ExitStatus::Failure
        }
    }
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
    unix_seconds_of(SystemTime::now())?;
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
        fill_from_secure_source: Box::new(|seed| *seed = SecretKey::generate().to_bytes()),
        clock: Arc::new(wall_clock_milliseconds),
        bind_endpoint: Box::new(|key, gate_key, relay| {
            Box::pin(NodeEndpoint::bind(
                key,
                gate_key,
                [relay],
                fill_with_secure_random_bytes,
                |closing: GateFailure| drop(writeln!(io::stderr(), "{closing}")),
            ))
        }),
        start_relay: Box::new(|site| Box::pin(infra::start_embedded_relay(site))),
        stopped,
        report: Arc::new(|record| drop(writeln!(io::stderr(), "{record}"))),
    }
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
#[cfg(feature = "relay")]
fn listen_for(kind: SignalKind, name: &'static str) -> Result<Signal, CommandLineFailure> {
    signal(kind).map_err(|source| CommandLineFailure::SignalNotHeard {
        signal: name,
        source,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use clap::CommandFactory;

    use super::*;

    const SECONDS_AFTER_EPOCH: u64 = 1_700_000_000;
    const MILLISECONDS_AFTER_EPOCH: u64 = 1_700_000_000_123;
    const BLOCKING_READ_THAT_NEVER_RETURNS_IN_TIME: Duration = Duration::from_secs(10);
    const AWAITED_SHUTDOWN_ENDS_WITHIN: Duration = Duration::from_millis(2000);
    const SPEC_PROGRAM_PURPOSE: &str = "Messaging between agents across machines and harnesses";

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
        let root = std::env::temp_dir().join(format!("u29-main-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&root));
        let base = root.clone();
        let environment = move |name: &str| match name {
            "XDG_CONFIG_HOME" => Some(base.join("c").into_os_string()),
            "XDG_DATA_HOME" => Some(base.join("d").into_os_string()),
            "XDG_RUNTIME_DIR" => Some(base.join("r").into_os_string()),
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
            |bytes| *bytes = [3; 32],
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
