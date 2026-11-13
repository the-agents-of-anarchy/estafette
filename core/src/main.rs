use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use estafette::cli::cards::{self, CardCommand, RegistryArgs};
use estafette::cli::debug::{DebugArgs, Tracing};
use estafette::cli::doctor::{self, DoctorArgs, DoctorError};
#[cfg(feature = "relay")]
use estafette::cli::infra::{
    self, InfrastructureCommand, InfrastructureCommandError, InfrastructureEffects, StopRequested,
};
use estafette::cli::network::{self, NetworkCommand, NetworkCommandError};
use estafette::cli::observer::{
    self, Interruption, ObserverCommand, ObserverCommandError, ObserverEffects, OperatorEffects,
};
use estafette::config::{ConfigError, NetworkConfig};
use estafette::dirs::{Directories, DirectoriesError, OperatingSystemFilesystem, TraceDirectory};
use estafette::errors::ExitStatus;
use estafette::event::{
    CLOCK_BEFORE_UNIX_EPOCH, Event, InboxEvent, ProcessEvent, RunRecord, TransportEvent,
    write_run_record,
};
use estafette::inbox::socket::{InboxSocket, InboxSocketError, OperatingSystemProcessTable};
use estafette::mcp::{self, ServedNetwork, ServerEffects};
use estafette::net::endpoint::{GateFailure, NodeEndpoint};
use estafette::node::{
    Exit, Network, NodeEffects, NodeError, RuntimeShutdown, ShutdownSignal, relay_answers,
};
use estafette::sidecar::{self, ThreadSleep};
use estafette::trace::iroh_lines::{self, IrohLinesRefused};
use estafette::trace::{
    RUN_ID_BYTES, RunId, Trace, TraceBounds, TraceClocks, TraceEffects, TraceRefused, TraceStop,
};
use estafette::wire::Nonce;
use iroh::SecretKey;
use iroh::endpoint::BindError;
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

/// Sends iroh's own lines into a trace's raw file, done once per process by the composition root.
type InstallIrohLines = fn(Arc<Trace>) -> Result<(), IrohLinesRefused>;

/// What the trace of a process that starts an endpoint runs on: its effects, its bounds, and the install that sends iroh's own lines to it.
#[derive(Clone, Copy)]
struct TraceSetup<'effects> {
    effects: &'effects TraceEffects,
    bounds: TraceBounds,
    install_iroh_lines: InstallIrohLines,
}

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
        #[command(flatten)]
        debug: DebugArgs,
    },
    /// Check the configuration file permissions, each card, each sidecar, each known relay and the visibility of a peer, print one line per check, and exit 3 when any check fails
    Doctor {
        #[command(flatten)]
        arguments: DoctorArgs,
        #[command(flatten)]
        debug: DebugArgs,
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
    Observer(ObserverCommandError),
    SignalNotHeard {
        signal: &'static str,
        source: io::Error,
    },
    #[cfg(feature = "relay")]
    Infrastructure(InfrastructureCommandError),
    Doctor(DoctorError),
    Trace(TraceRefused),
    IrohLines(IrohLinesRefused),
}

impl fmt::Display for CommandLineFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Directories(source) => write!(formatter, "{source}"),
            Self::ClockBeforeUnixEpoch => formatter.write_str(CLOCK_BEFORE_UNIX_EPOCH),
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
            Self::Trace(source) => write!(formatter, "{source}"),
            Self::IrohLines(source) => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for CommandLineFailure {}

fn main() -> ExitCode {
    let status = run_command(
        EstafetteArguments::parse().command,
        |name| std::env::var_os(name),
        TraceSetup {
            effects: &TraceEffects::of_the_operating_system(trace_clocks_started_now()),
            bounds: TraceBounds::SPEC,
            install_iroh_lines: iroh_lines::install,
        },
    );
    ExitCode::from(u8::from(status))
}

/// Runs the command on the process's streams, reading the environment through the given lookup and tracing on the given setup, and says how the process ends.
fn run_command(
    command: Command,
    environment: impl Fn(&str) -> Option<OsString>,
    trace_setup: TraceSetup<'_>,
) -> ExitStatus {
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
        Command::Infrastructure(command) => run_infrastructure_command(
            command,
            &environment,
            &mut io::stdout(),
            io::stderr(),
            trace_setup,
        ),
        Command::Network { command } => exit_status_after(
            run_network_command(&command, &environment, &mut io::stdout(), &mut io::stderr())
                .map(|()| ExitStatus::Success),
            standard_error_record().as_ref(),
        ),
        Command::Observer(command) => run_traced(
            command.debug().tracing(),
            &environment,
            command.cards_dir(),
            io::stderr(),
            trace_setup,
            |directories, process| {
                run_observer_command(&command, directories, process, &mut io::stdout())
                    .map(|()| ExitStatus::Success)
            },
        ),
        Command::Mcp { registry, debug } => run_traced(
            debug.tracing(),
            &environment,
            registry.cards_dir.clone(),
            io::stderr(),
            trace_setup,
            |directories, process| serve_mcp(registry, directories, process),
        ),
        Command::Doctor { arguments, debug } => run_traced(
            debug.tracing(),
            &environment,
            arguments.registry.cards_dir.clone(),
            io::stderr(),
            trace_setup,
            |directories, process| {
                run_doctor_command(&arguments, directories, process, &mut io::stdout())
            },
        ),
    }
}

/// The run record of one process that starts an endpoint, and its debug trace unless `--no-debug` was given.
struct ProcessRecord {
    record: RunRecord,
    trace: Option<Arc<Trace>>,
}

/// Runs a command that starts an endpoint: resolves the directories, starts the trace on the given setup unless `--no-debug` and names its files first on standard error, sends iroh's own lines to it through the setup's install, runs the command, records how it ended, then empties the trace's queues within their bound before the process exits, saying on standard error, file by file, when a queue did not empty or its file could not be cut back to its last line.
fn run_traced<StandardError: Write + Send + 'static>(
    tracing: Tracing,
    environment: impl Fn(&str) -> Option<OsString>,
    cards_dir: Option<PathBuf>,
    stderr: StandardError,
    trace_setup: TraceSetup<'_>,
    command: impl FnOnce(Directories, &ProcessRecord) -> Result<ExitStatus, CommandLineFailure>,
) -> ExitStatus {
    let stderr = Arc::new(Mutex::new(stderr));
    let trace = match start_trace(
        tracing,
        &environment,
        trace_setup.effects,
        trace_setup.bounds,
    ) {
        Ok(trace) => trace,
        Err(failure) => {
            return exit_status_after(Err(failure), run_record_on(&stderr, None).as_ref());
        }
    };
    if let Some(trace) = &trace {
        drop(writeln!(
            stderr.lock().unwrap_or_else(PoisonError::into_inner),
            "{}",
            trace.announcement()
        ));
    }
    let process = ProcessRecord {
        record: run_record_on(&stderr, trace.clone()),
        trace,
    };
    let outcome = Directories::establish(environment, &OperatingSystemFilesystem, cards_dir)
        .map_err(CommandLineFailure::Directories)
        .and_then(|directories| {
            if let Some(trace) = &process.trace {
                (trace_setup.install_iroh_lines)(Arc::clone(trace))
                    .map_err(CommandLineFailure::IrohLines)?;
            }
            command(directories, &process)
        });
    let status = exit_status_after(outcome, process.record.as_ref());
    if let Some(trace) = &process.trace {
        for stop in trace.finish() {
            let unfinished = match stop {
                TraceStop::Drained => None,
                TraceStop::NotDrained { file } => Some(ProcessEvent::TraceNotDrained {
                    file,
                    drain: trace_setup.bounds.drain,
                }),
                TraceStop::NotCut { file, source } => {
                    Some(ProcessEvent::TraceNotCut { file, source })
                }
            };
            if let Some(unfinished) = unfinished {
                run_record_on(&stderr, None)(unfinished.into());
            }
        }
    }
    status
}

/// The debug trace of this process under `--debug`, its run id from the secure random source and its files written through the given effects within the given bounds; none under `--no-debug`.
fn start_trace(
    tracing: Tracing,
    environment: &impl Fn(&str) -> Option<OsString>,
    trace_effects: &TraceEffects,
    trace_bounds: TraceBounds,
) -> Result<Option<Arc<Trace>>, CommandLineFailure> {
    match tracing {
        Tracing::NoDebug => Ok(None),
        Tracing::Debug => {
            let directory = TraceDirectory::resolve(environment)
                .map_err(|failure| CommandLineFailure::Directories(failure.into()))?;
            let mut run_id = [0; RUN_ID_BYTES];
            fill_with_secure_random_bytes(&mut run_id);
            Trace::start(
                &directory,
                &OperatingSystemFilesystem,
                RunId::from_random_bytes(run_id),
                trace_effects,
                trace_bounds,
            )
            .map(|trace| Some(Arc::new(trace)))
            .map_err(CommandLineFailure::Trace)
        }
    }
}

/// The trace's clocks started now: milliseconds since this call, and the latest wall-clock reading at or after the Unix epoch.
fn trace_clocks_started_now() -> TraceClocks {
    let started = std::time::Instant::now();
    let last_wall_reading = AtomicU64::new(0);
    TraceClocks {
        monotonic_ms: Arc::new(move || {
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
        }),
        wall_ms: Arc::new(move || {
            wall_clock_reading(SystemTime::now(), &last_wall_reading, &|_event| {})
        }),
    }
}

/// The run record written to the standard error stream and, when there is one, into the trace.
fn run_record_on<StandardError: Write + Send + 'static>(
    stderr: &Arc<Mutex<StandardError>>,
    trace: Option<Arc<Trace>>,
) -> RunRecord {
    let stderr = Arc::clone(stderr);
    Arc::new(move |event: Event| {
        drop(write_run_record(
            &mut *stderr.lock().unwrap_or_else(PoisonError::into_inner),
            &event,
        ));
        if let Some(trace) = &trace {
            trace.record(&event);
        }
    })
}

/// Binds the endpoint, then names it in the trace when there is one.
async fn bound_in_trace(
    binding: impl Future<Output = Result<NodeEndpoint, BindError>>,
    trace: Option<Arc<Trace>>,
) -> Result<NodeEndpoint, BindError> {
    let endpoint = binding.await?;
    if let Some(trace) = trace {
        trace.note_endpoint(endpoint.endpoint().id());
    }
    Ok(endpoint)
}

/// Says how the process ends after a command: the status it reached, or failure after recording the failure's message.
fn exit_status_after(
    outcome: Result<ExitStatus, CommandLineFailure>,
    record: &dyn Fn(Event),
) -> ExitStatus {
    outcome.unwrap_or_else(|failure| {
        record(ProcessEvent::CommandFailed(Box::new(failure)).into());
        ExitStatus::Failure
    })
}

/// Serves the MCP tools of the network until the session ends, building every side effect the server runs on.
fn serve_mcp(
    registry: RegistryArgs,
    directories: Directories,
    process: &ProcessRecord,
) -> Result<ExitStatus, CommandLineFailure> {
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
    let record = Arc::clone(&process.record);
    let trace = process.trace.clone();
    let exit = runtime.block_on(async move {
        let inbox = InboxSocket::start(
            &directories,
            process_id,
            &OperatingSystemProcessTable,
            accept_failure_recorder(Arc::clone(&record)),
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
            server_effects(Arc::new(inbox), record, trace),
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

/// The side effects of the MCP server and its node: the clocks, a secure random source, the endpoint binding, the run record, and the filesystem.
fn server_effects(
    inbox: Arc<InboxSocket>,
    record: RunRecord,
    trace: Option<Arc<Trace>>,
) -> ServerEffects {
    let gate_record = Arc::clone(&record);
    ServerEffects {
        node: NodeEffects {
            clock: Arc::new(recording_wall_clock(Arc::clone(&record))),
            monotonic: Arc::new(tokio::time::Instant::now),
            fill_probe_nonce: Arc::new(|nonce: &mut Nonce| fill_with_secure_random_bytes(nonce)),
            report: Arc::clone(&record),
            inbox,
            probe_relay: Arc::new(|relay| Box::pin(relay_answers(relay))),
        },
        bind_endpoint: Arc::new(move |card_key, gate_key, relays, connections| {
            Box::pin(bound_in_trace(
                NodeEndpoint::bind(
                    card_key,
                    gate_key,
                    relays,
                    fill_with_secure_random_bytes,
                    gate_closing_recorder(Arc::clone(&gate_record)),
                    connections,
                ),
                trace.clone(),
            ))
        }),
        registry: Arc::new(OperatingSystemFilesystem),
        pause_between_lock_tries: Arc::new(std::thread::sleep),
        is_card_held: Arc::new(|sidecar_path: &Path| {
            sidecar::is_held(sidecar_path).is_ok_and(|held| held)
        }),
        report: record,
    }
}

/// The wall clock handed to a node: milliseconds since the Unix epoch, holding its last reading when the machine's clock is set back before the epoch.
fn recording_wall_clock(record: RunRecord) -> impl Fn() -> u64 + Send + Sync + 'static {
    let last_reading = AtomicU64::new(0);
    move || wall_clock_reading(SystemTime::now(), &last_reading, record.as_ref())
}

/// The milliseconds since the Unix epoch at `now`, remembered as the last reading; a `now` before the epoch is recorded and answered by the last reading.
fn wall_clock_reading(now: SystemTime, last_reading: &AtomicU64, record: &dyn Fn(Event)) -> u64 {
    match unix_milliseconds_of(now) {
        Ok(milliseconds) => {
            last_reading.store(milliseconds, Ordering::Relaxed);
            milliseconds
        }
        Err(_before_epoch) => {
            record(ProcessEvent::ClockBeforeUnixEpoch.into());
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

/// Hands the reason a gossip connection was closed at the network secret gate to the run record.
fn gate_closing_recorder(record: RunRecord) -> impl Fn(GateFailure) + Send + Sync + 'static {
    move |failure| record(TransportEvent::GateClosed(failure).into())
}

/// Hands the failure to accept a client of the inbox socket to the run record.
fn accept_failure_recorder(record: RunRecord) -> impl Fn(io::Error) + Send + Sync + 'static {
    move |failure| record(InboxEvent::AcceptFailed(failure).into())
}

/// The run record written to standard error alone.
fn standard_error_record() -> RunRecord {
    run_record_on(&Arc::new(Mutex::new(io::stderr())), None)
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

fn observer_runtime(
    directories: Directories,
    process: &ProcessRecord,
    interruption: Interruption,
) -> Result<ObserverRuntime, CommandLineFailure> {
    let directories = Arc::new(directories);
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
        observer_effects(Arc::clone(&directories), clock, interrupted, process);
    Ok(ObserverRuntime {
        directories,
        runtime,
        effects,
        operator_effects,
    })
}

fn run_observer_command(
    command: &ObserverCommand,
    directories: Directories,
    process: &ProcessRecord,
    result: &mut (impl Write + Send),
) -> Result<(), CommandLineFailure> {
    let ObserverRuntime {
        directories,
        runtime,
        effects,
        operator_effects,
    } = observer_runtime(directories, process, command.interruption())?;
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
    directories: Directories,
    process: &ProcessRecord,
    result: &mut impl Write,
) -> Result<ExitStatus, CommandLineFailure> {
    let ObserverRuntime {
        directories,
        runtime,
        effects,
        operator_effects,
    } = observer_runtime(directories, process, doctor::INTERRUPTION)?;
    runtime
        .block_on(doctor::run(
            arguments,
            &directories,
            &OperatingSystemFilesystem,
            &ThreadSleep,
            effects,
            operator_effects.probe_relay,
            result,
        ))
        .map_err(CommandLineFailure::Doctor)
}

fn observer_effects(
    directories: Arc<Directories>,
    clock: impl Fn() -> u64 + Send + Sync + 'static,
    interrupted: Pin<Box<dyn Future<Output = ()> + Send>>,
    process: &ProcessRecord,
) -> (ObserverEffects, OperatorEffects) {
    let record = Arc::clone(&process.record);
    let trace = process.trace.clone();
    let gate_record = Arc::clone(&record);
    let accept_record = Arc::clone(&record);
    let effects = ObserverEffects {
        clock: Arc::new(clock),
        monotonic: Arc::new(Instant::now),
        fill_seed: Arc::new(|seed: &mut [u8; RANDOM_BYTES_PER_SECRET_KEY]| {
            fill_with_secure_random_bytes(seed);
        }),
        bind_endpoint: Box::new(move |key, gate_key, relays, connections| {
            Box::pin(bound_in_trace(
                NodeEndpoint::bind(
                    key,
                    gate_key,
                    relays,
                    fill_with_secure_random_bytes,
                    gate_closing_recorder(Arc::clone(&gate_record)),
                    connections,
                ),
                trace.clone(),
            ))
        }),
        interrupted,
    };
    let operator_effects = OperatorEffects {
        fill_probe_nonce: Arc::new(|nonce: &mut Nonce| fill_with_secure_random_bytes(nonce)),
        report: record,
        probe_relay: Arc::new(|relay| Box::pin(relay_answers(relay))),
        start_inbox: Box::new(move || {
            InboxSocket::start(
                &directories,
                std::process::id(),
                &OperatingSystemProcessTable,
                accept_failure_recorder(accept_record),
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

/// Runs the infrastructure command on a runtime of its own, its run record on `stderr` and its trace on the given setup, and says how the process ends.
#[cfg(feature = "relay")]
fn run_infrastructure_command(
    command: InfrastructureCommand,
    environment: impl Fn(&str) -> Option<OsString>,
    result: &mut (impl Write + Send),
    stderr: impl Write + Send + 'static,
    trace_setup: TraceSetup<'_>,
) -> ExitStatus {
    run_traced(
        command.debug().tracing(),
        environment,
        None,
        stderr,
        trace_setup,
        |directories, process| {
            serve_infrastructure(command, &directories, process, result)
                .map(|()| ExitStatus::Success)
        },
    )
}

/// Refuses a clock before the Unix epoch and serves the host until a stop signal.
#[cfg(feature = "relay")]
fn serve_infrastructure(
    command: InfrastructureCommand,
    directories: &Directories,
    process: &ProcessRecord,
    result: &mut (impl Write + Send),
) -> Result<(), CommandLineFailure> {
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
            directories,
            &OperatingSystemFilesystem,
            infrastructure_effects(stopped, process),
            result,
        ))
        .map_err(CommandLineFailure::Infrastructure)
}

/// The side effects of the infrastructure command: the secure random source, the wall clock, the gated endpoint, the embedded relay, the stop signals and the run record on stderr.
#[cfg(feature = "relay")]
fn infrastructure_effects(
    stopped: StopRequested,
    process: &ProcessRecord,
) -> InfrastructureEffects {
    let record = Arc::clone(&process.record);
    let trace = process.trace.clone();
    let gate_record = Arc::clone(&record);
    InfrastructureEffects {
        fill_from_secure_source: Box::new(|seed: &mut [u8; RANDOM_BYTES_PER_SECRET_KEY]| {
            fill_with_secure_random_bytes(seed);
        }),
        clock: Arc::new(recording_wall_clock(Arc::clone(&record))),
        bind_endpoint: Box::new(move |key, gate_key, relay, connections| {
            Box::pin(bound_in_trace(
                NodeEndpoint::bind(
                    key,
                    gate_key,
                    [relay],
                    fill_with_secure_random_bytes,
                    gate_closing_recorder(Arc::clone(&gate_record)),
                    connections,
                ),
                trace.clone(),
            ))
        }),
        monotonic: Arc::new(Instant::now),
        start_relay: Box::new(|site| Box::pin(infra::start_embedded_relay(site))),
        stopped,
        report: record,
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
    use std::ffi::OsStr;
    use std::time::{Duration, Instant};

    use std::sync::atomic::AtomicBool;

    use clap::CommandFactory;
    use clap::error::ErrorKind;
    #[cfg(feature = "relay")]
    use estafette::crypto::KEY_LEN;
    use estafette::dirs::{TraceFileKind, trace_file_of};
    use estafette::trace::{CreateTraceFile, SpawnThread, TraceFile};

    use super::*;
    use crate::seeded_draws::Draws;

    const SECONDS_AFTER_EPOCH: u64 = 1_700_000_000;
    const MILLISECONDS_AFTER_EPOCH: u64 = 1_700_000_000_123;
    const BLOCKING_READ_THAT_NEVER_RETURNS_IN_TIME: Duration = Duration::from_secs(10);
    const AWAITED_SHUTDOWN_ENDS_WITHIN: Duration = Duration::from_millis(2000);
    const SPEC_PROGRAM_PURPOSE: &str = "Messaging between agents across machines and harnesses";
    /// Sends no iroh line anywhere: one test process runs many traced commands, and a process holds one global subscriber.
    const IROH_LINES_LEFT_OUT: InstallIrohLines = |_trace| Ok(());

    /// A scratch directory tree that is removed when this value is dropped, whether the test passed or failed.
    struct ScratchRoot(PathBuf);

    impl Drop for ScratchRoot {
        fn drop(&mut self) {
            drop(std::fs::remove_dir_all(&self.0));
        }
    }

    /// A scratch root for the test, removed when the guard drops, and the environment naming its three base directories.
    fn scratch_environment(prefix: &str) -> (ScratchRoot, impl Fn(&str) -> Option<OsString>) {
        const CONFIG_HOME_VARIABLE: &str = "XDG_CONFIG_HOME";
        const DATA_HOME_VARIABLE: &str = "XDG_DATA_HOME";
        const RUNTIME_DIRECTORY_VARIABLE: &str = "XDG_RUNTIME_DIR";
        const CONFIG_HOME_UNDER_ROOT: &str = "c";
        const DATA_HOME_UNDER_ROOT: &str = "d";
        const RUNTIME_DIRECTORY_UNDER_ROOT: &str = "r";
        let root = std::env::temp_dir().join(format!("{prefix}-{}", std::process::id()));
        let base = root.clone();
        let environment = move |name: &str| match name {
            CONFIG_HOME_VARIABLE => Some(base.join(CONFIG_HOME_UNDER_ROOT).into_os_string()),
            DATA_HOME_VARIABLE => Some(base.join(DATA_HOME_UNDER_ROOT).into_os_string()),
            RUNTIME_DIRECTORY_VARIABLE => {
                Some(base.join(RUNTIME_DIRECTORY_UNDER_ROOT).into_os_string())
            }
            _ => None,
        };
        (ScratchRoot(root), environment)
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

    const ENDPOINT_COMMANDS: &[&str] = &[
        "show-peers",
        "tail",
        "test-send",
        "mcp",
        "doctor",
        #[cfg(feature = "relay")]
        "run-infrastructure",
    ];
    const COMMANDS_WITHOUT_AN_ENDPOINT: [&str; 3] = ["create-card", "list-cards", "network"];
    const DEBUG_FLAG: &str = "debug";
    const NO_DEBUG_FLAG: &str = "no-debug";
    const SPEC_TRACE_HELP_STATEMENTS: [&str; 11] = [
        "traces/ under the data directory",
        "<start>-<run>-estafette.events.jsonl",
        "which is the file safe to post",
        "<start>-<run>-estafette.raw.jsonl",
        "which is not safe to post",
        "the default until v1.0.0",
        "rolls over once at 64 MiB into a .1 file that replaces any earlier one",
        "at most 128 MiB per file",
        "at most 1 GiB",
        "written in the last 10 minutes",
        "Write no debug trace for this run: create no trace file and start no trace writer",
    ];

    /// The long flags the command takes, without the leading dashes.
    fn long_flags_of(command: &clap::Command) -> Vec<String> {
        command
            .get_arguments()
            .filter_map(clap::Arg::get_long)
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn observability_decision_1_the_debug_pair_is_on_every_command_that_starts_an_endpoint_and_on_no_other()
     {
        let program = EstafetteArguments::command();

        for name in ENDPOINT_COMMANDS {
            let flags = long_flags_of(program.find_subcommand(name).unwrap());
            assert!(
                flags.iter().any(|flag| flag == DEBUG_FLAG),
                "{name}: {flags:?}"
            );
            assert!(
                flags.iter().any(|flag| flag == NO_DEBUG_FLAG),
                "{name}: {flags:?}"
            );
        }
        for name in COMMANDS_WITHOUT_AN_ENDPOINT {
            let command = program.find_subcommand(name).unwrap();
            for command in std::iter::once(command).chain(command.get_subcommands()) {
                let flags = long_flags_of(command);
                assert!(
                    !flags.iter().any(|flag| flag == DEBUG_FLAG),
                    "{name}: {flags:?}"
                );
                assert!(
                    !flags.iter().any(|flag| flag == NO_DEBUG_FLAG),
                    "{name}: {flags:?}"
                );
            }
        }
    }

    #[test]
    fn observability_decision_1_the_last_of_debug_and_no_debug_wins_and_debug_is_the_default() {
        let cases: [(&[&str], Tracing); 4] = [
            (&[], Tracing::Debug),
            (&["--no-debug"], Tracing::NoDebug),
            (&["--debug", "--no-debug"], Tracing::NoDebug),
            (&["--no-debug", "--debug"], Tracing::Debug),
        ];
        for (flags, expected) in cases {
            let arguments = ["estafette", "mcp", "lab"].iter().chain(flags.iter());
            let Command::Mcp { debug, .. } = EstafetteArguments::try_parse_from(arguments)
                .unwrap()
                .command
            else {
                panic!("mcp parses as the mcp command");
            };

            assert_eq!(debug.tracing(), expected, "{flags:?}");
        }
    }

    #[test]
    fn observability_decision_3_each_endpoint_command_help_states_what_the_trace_writes_where_which_file_is_safe_to_post_and_its_four_bounds()
     {
        let mut program = EstafetteArguments::command();
        program.build();

        for name in ENDPOINT_COMMANDS {
            let help = program
                .find_subcommand_mut(name)
                .unwrap()
                .render_long_help()
                .to_string();
            for statement in SPEC_TRACE_HELP_STATEMENTS {
                assert!(
                    help.contains(statement),
                    "{name} help lacks {statement:?}:\n{help}"
                );
            }
        }
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

                let reading = wall_clock_reading(instant, &last_reading, &|_event| {
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

            let expected_line =
                format!("a gossip connection was closed at the network secret gate: {failure}\n");

            write_run_record(&mut record, &TransportEvent::GateClosed(failure).into()).unwrap();

            let mut expected = held;
            expected.extend(expected_line.into_bytes());
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
            let record = std::cell::RefCell::new(held.clone());

            let status = exit_status_after(outcome, &|event| {
                write_run_record(&mut *record.borrow_mut(), &event).unwrap();
            });

            let record = record.into_inner();
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

        let status = run_command(
            Command::Observer(observer),
            environment,
            TraceSetup {
                effects: &TraceEffects::of_the_operating_system(trace_clocks_started_now()),
                bounds: TraceBounds::SPEC,
                install_iroh_lines: IROH_LINES_LEFT_OUT,
            },
        );

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
    fn spec_3_13_a_command_line_naming_a_malformed_invitation_or_host_line_is_refused_before_any_command_runs_with_status_2()
     {
        const MALFORMED: &str = "not-an-invitation";
        const USAGE_ERROR_EXIT_CODE: i32 = 2;
        for arguments in [
            &["estafette", "network", "join", MALFORMED][..],
            &["estafette", "network", "add-host", "lab", MALFORMED][..],
        ] {
            let refusal =
                EstafetteArguments::try_parse_from(arguments.iter().copied()).unwrap_err();

            assert_eq!(refusal.kind(), ErrorKind::ValueValidation, "{arguments:?}");
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

    /// A trace file whose every write waits until the command's body has ended, appending to the bytes the test reads.
    struct HeldBackTraceFile {
        body_ended: Arc<Mutex<()>>,
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl TraceFile for HeldBackTraceFile {
        fn set_length(&mut self, _length: u64) -> io::Result<()> {
            Ok(())
        }
    }

    impl Write for HeldBackTraceFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            drop(
                self.body_ended
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner),
            );
            self.written
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn observability_decision_12_every_event_a_command_records_is_in_the_trace_when_its_entry_function_returns()
     {
        const EVENTS_RECORDED: usize = 1000;
        const SPEC_CLOCK_EVENT_NAME: &str = "process.clock_before_unix_epoch";
        const SCRATCH_PREFIX: &str = "estafette-trace-drain";
        let (_scratch_removed_when_the_test_ends, environment) =
            scratch_environment(SCRATCH_PREFIX);
        let body_ended = Arc::new(Mutex::new(()));
        let written = Arc::new(Mutex::new(Vec::new()));
        let held_back_files = Arc::clone(&body_ended);
        let files_written = Arc::clone(&written);
        let trace_effects = TraceEffects {
            create_file: Arc::new(move |_path: &Path| -> io::Result<Box<dyn TraceFile>> {
                Ok(Box::new(HeldBackTraceFile {
                    body_ended: Arc::clone(&held_back_files),
                    written: Arc::clone(&files_written),
                }))
            }),
            ..TraceEffects::of_the_operating_system(trace_clocks_started_now())
        };
        let body_running = body_ended.lock().unwrap();

        let status = run_traced(
            Tracing::Debug,
            environment,
            None,
            io::sink(),
            TraceSetup {
                effects: &trace_effects,
                bounds: TraceBounds::SPEC,
                install_iroh_lines: IROH_LINES_LEFT_OUT,
            },
            move |_directories, process| {
                for _ in 0..EVENTS_RECORDED {
                    (process.record)(ProcessEvent::ClockBeforeUnixEpoch.into());
                }
                drop(body_running);
                Ok(ExitStatus::Success)
            },
        );

        let written = String::from_utf8(written.lock().unwrap().clone()).unwrap();
        let recorded = written
            .lines()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["event"]
                    == SPEC_CLOCK_EVENT_NAME
            })
            .count();
        assert_eq!(status, ExitStatus::Success);
        assert_eq!(recorded, EVENTS_RECORDED);
    }

    #[test]
    fn observability_decision_12_a_trace_still_writing_when_its_drain_bound_runs_out_says_so_for_each_file_events_first_and_the_status_stands()
     {
        const SCRATCH_PREFIX: &str = "estafette-trace-undrained";
        let (_scratch_removed_when_the_test_ends, environment) =
            scratch_environment(SCRATCH_PREFIX);
        let run_ended = Arc::new(Mutex::new(()));
        let held_back_files = Arc::clone(&run_ended);
        let trace_effects = TraceEffects {
            create_file: Arc::new(move |_path: &Path| -> io::Result<Box<dyn TraceFile>> {
                Ok(Box::new(HeldBackTraceFile {
                    body_ended: Arc::clone(&held_back_files),
                    written: Arc::default(),
                }))
            }),
            ..TraceEffects::of_the_operating_system(trace_clocks_started_now())
        };
        let standard_error = SharedRecord::default();
        let run_running = run_ended.lock().unwrap();

        let status = run_traced(
            Tracing::Debug,
            environment,
            None,
            standard_error.clone(),
            TraceSetup {
                effects: &trace_effects,
                bounds: TraceBounds {
                    drain: Duration::ZERO,
                    ..TraceBounds::SPEC
                },
                install_iroh_lines: IROH_LINES_LEFT_OUT,
            },
            |_directories, process| {
                record_in_the_events_file_alone(process);
                Ok(ExitStatus::Success)
            },
        );

        drop(run_running);
        assert_eq!(status, ExitStatus::Success);
        assert_eq!(
            lines_after_the_announcement(&standard_error),
            [
                spec_not_drained_line(SPEC_EVENTS_FILE_WORD, Duration::ZERO),
                spec_not_drained_line(SPEC_RAW_FILE_WORD, Duration::ZERO)
            ]
        );
    }

    const STAND_IN_SHRINK_REFUSAL: &str = "the file refuses to shrink";
    const SPEC_EVENTS_FILE_WORD: &str = "events";
    const SPEC_RAW_FILE_WORD: &str = "raw";
    const STOP_REPORTED_WITHIN: Duration = Duration::from_secs(10);

    /// The line standard error gives, by the lead's ruling on decision 12, for a trace file whose writer did not report its stop within `drain`.
    fn spec_not_drained_line(file_word: &str, drain: Duration) -> String {
        format!(
            "the debug trace did not finish writing its {file_word} file within {} ms of the stop: the file's last lines may be missing",
            drain.as_millis()
        )
    }

    /// The line standard error gives, by the lead's ruling on decision 12, for a trace file the stand-in refused to cut back to its last line.
    fn spec_not_cut_line(file_word: &str) -> String {
        format!(
            "the debug trace could not cut its {file_word} file back to its last line at the stop, so the file ends in zero bytes: {STAND_IN_SHRINK_REFUSAL}"
        )
    }

    /// The lines standard error holds after its first, the one naming the trace files.
    fn lines_after_the_announcement(standard_error: &SharedRecord) -> Vec<String> {
        String::from_utf8(standard_error.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect()
    }

    /// Records one event in the events file and nowhere else, so the file is sized ahead of a line and standard error gains nothing.
    fn record_in_the_events_file_alone(process: &ProcessRecord) {
        if let Some(trace) = &process.trace {
            trace.record(&ProcessEvent::ClockBeforeUnixEpoch.into());
        }
    }

    /// Trace effects that create each events file, as the trace directory names it, through `events_file` and every other file through `raw_file`.
    fn trace_effects_creating(
        events_file: CreateTraceFile,
        raw_file: CreateTraceFile,
    ) -> TraceEffects {
        TraceEffects {
            create_file: Arc::new(move |path: &Path| {
                let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
                match trace_file_of(file_name) {
                    Some((_, TraceFileKind::Events)) => events_file(path),
                    Some((_, TraceFileKind::Raw)) | None => raw_file(path),
                }
            }),
            ..TraceEffects::of_the_operating_system(trace_clocks_started_now())
        }
    }

    /// Creates trace files that refuse to be cut back to their last line.
    fn uncuttable_trace_files() -> CreateTraceFile {
        Arc::new(|_path: &Path| -> io::Result<Box<dyn TraceFile>> {
            Ok(Box::new(UncuttableTraceFile { sized: 0 }))
        })
    }

    /// Starts each trace writer thread but the first, the events file's as `Trace::start` starts them, whose work is dropped unrun so it never reports its stop.
    fn spawning_no_events_writer() -> SpawnThread {
        let is_events_writer = AtomicBool::new(true);
        let spawn_thread =
            TraceEffects::of_the_operating_system(trace_clocks_started_now()).spawn_thread;
        Arc::new(move |name: &str, work: Box<dyn FnOnce() + Send>| {
            if is_events_writer.swap(false, Ordering::SeqCst) {
                return Ok(());
            }
            spawn_thread(name, work)
        })
    }

    /// A trace file that refuses every length but one past the size it was last given, discarding what is written to it.
    struct UncuttableTraceFile {
        sized: u64,
    }

    impl TraceFile for UncuttableTraceFile {
        fn set_length(&mut self, length: u64) -> io::Result<()> {
            if length <= self.sized {
                return Err(io::Error::other(STAND_IN_SHRINK_REFUSAL));
            }
            self.sized = length;
            Ok(())
        }
    }

    impl Write for UncuttableTraceFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn observability_decision_12_a_trace_whose_two_files_cannot_be_cut_back_at_the_stop_says_so_for_each_file_events_first_and_the_status_stands()
     {
        const SCRATCH_PREFIX: &str = "estafette-trace-not-cut";
        let (_scratch_removed_when_the_test_ends, environment) =
            scratch_environment(SCRATCH_PREFIX);
        let trace_effects =
            trace_effects_creating(uncuttable_trace_files(), uncuttable_trace_files());
        let standard_error = SharedRecord::default();

        let status = run_traced(
            Tracing::Debug,
            environment,
            None,
            standard_error.clone(),
            TraceSetup {
                effects: &trace_effects,
                bounds: TraceBounds {
                    drain: STOP_REPORTED_WITHIN,
                    ..TraceBounds::SPEC
                },
                install_iroh_lines: IROH_LINES_LEFT_OUT,
            },
            |_directories, process| {
                record_in_the_events_file_alone(process);
                Ok(ExitStatus::Success)
            },
        );

        assert_eq!(status, ExitStatus::Success);
        assert_eq!(
            lines_after_the_announcement(&standard_error),
            [
                spec_not_cut_line(SPEC_EVENTS_FILE_WORD),
                spec_not_cut_line(SPEC_RAW_FILE_WORD)
            ]
        );
    }

    #[test]
    fn observability_decision_12_an_events_file_that_cannot_be_cut_back_beside_a_raw_file_that_drains_is_named_alone_on_standard_error()
     {
        const SCRATCH_PREFIX: &str = "estafette-trace-events-not-cut";
        let (_scratch_removed_when_the_test_ends, environment) =
            scratch_environment(SCRATCH_PREFIX);
        let trace_effects = trace_effects_creating(
            uncuttable_trace_files(),
            TraceEffects::of_the_operating_system(trace_clocks_started_now()).create_file,
        );
        let standard_error = SharedRecord::default();

        let status = run_traced(
            Tracing::Debug,
            environment,
            None,
            standard_error.clone(),
            TraceSetup {
                effects: &trace_effects,
                bounds: TraceBounds {
                    drain: STOP_REPORTED_WITHIN,
                    ..TraceBounds::SPEC
                },
                install_iroh_lines: IROH_LINES_LEFT_OUT,
            },
            |_directories, process| {
                record_in_the_events_file_alone(process);
                Ok(ExitStatus::Success)
            },
        );

        assert_eq!(status, ExitStatus::Success);
        assert_eq!(
            lines_after_the_announcement(&standard_error),
            [spec_not_cut_line(SPEC_EVENTS_FILE_WORD)]
        );
    }

    #[test]
    fn observability_decision_12_an_events_file_whose_writer_never_reports_and_a_raw_file_that_cannot_be_cut_back_are_each_named_events_first()
     {
        const SCRATCH_PREFIX: &str = "estafette-trace-undrained-and-not-cut";
        let (_scratch_removed_when_the_test_ends, environment) =
            scratch_environment(SCRATCH_PREFIX);
        let trace_effects = TraceEffects {
            spawn_thread: spawning_no_events_writer(),
            ..trace_effects_creating(uncuttable_trace_files(), uncuttable_trace_files())
        };
        let standard_error = SharedRecord::default();

        let status = run_traced(
            Tracing::Debug,
            environment,
            None,
            standard_error.clone(),
            TraceSetup {
                effects: &trace_effects,
                bounds: TraceBounds {
                    drain: STOP_REPORTED_WITHIN,
                    ..TraceBounds::SPEC
                },
                install_iroh_lines: IROH_LINES_LEFT_OUT,
            },
            |_directories, _process| Ok(ExitStatus::Success),
        );

        assert_eq!(status, ExitStatus::Success);
        assert_eq!(
            lines_after_the_announcement(&standard_error),
            [
                spec_not_drained_line(SPEC_EVENTS_FILE_WORD, STOP_REPORTED_WITHIN),
                spec_not_cut_line(SPEC_RAW_FILE_WORD)
            ]
        );
    }

    #[test]
    fn observability_decision_12_an_events_file_holding_nothing_past_its_last_line_is_never_said_to_end_in_zero_bytes_while_the_raw_file_sized_past_its_line_is()
     {
        const SCRATCH_PREFIX: &str = "estafette-trace-nothing-past-the-last-line";
        let (_scratch_removed_when_the_test_ends, environment) =
            scratch_environment(SCRATCH_PREFIX);
        let trace_effects =
            trace_effects_creating(uncuttable_trace_files(), uncuttable_trace_files());
        let standard_error = SharedRecord::default();

        let status = run_traced(
            Tracing::Debug,
            environment,
            None,
            standard_error.clone(),
            TraceSetup {
                effects: &trace_effects,
                bounds: TraceBounds {
                    drain: STOP_REPORTED_WITHIN,
                    ..TraceBounds::SPEC
                },
                install_iroh_lines: IROH_LINES_LEFT_OUT,
            },
            |_directories, _process| Ok(ExitStatus::Success),
        );

        assert_eq!(status, ExitStatus::Success);
        assert_eq!(
            lines_after_the_announcement(&standard_error),
            [spec_not_cut_line(SPEC_RAW_FILE_WORD)]
        );
    }

    #[derive(Clone, Default)]
    struct SharedRecord(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedRecord {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .write(bytes)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
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
        let mut printed = Vec::new();
        let recorded = SharedRecord::default();

        let status = run_infrastructure_command(
            infrastructure,
            &environment,
            &mut printed,
            recorded.clone(),
            TraceSetup {
                effects: &TraceEffects::of_the_operating_system(trace_clocks_started_now()),
                bounds: TraceBounds::SPEC,
                install_iroh_lines: IROH_LINES_LEFT_OUT,
            },
        );

        let recorded = recorded.0.lock().unwrap().clone();
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
