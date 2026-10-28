use std::fmt;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use estafette::cli::cards::{self, CardCommand};
use estafette::cli::network::{self, NetworkCommand, NetworkCommandError};
use estafette::cli::observer::{self, ObserverCommand, ObserverCommandError, ObserverEffects};
use estafette::dirs::{Directories, DirectoriesError, OperatingSystemFilesystem};
use estafette::errors::ExitStatus;
use estafette::inbox::socket::{InboxSocket, OperatingSystemProcessTable};
use estafette::net::endpoint::NodeEndpoint;
use estafette::node::relay_answers;
use estafette::sidecar::ThreadSleep;
use estafette::wire::Nonce;
use iroh::SecretKey;
use tokio::time::Instant;

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
}

#[derive(Debug)]
enum CommandLineFailure {
    Directories(DirectoriesError),
    ClockBeforeUnixEpoch,
    Network(NetworkCommandError),
    Runtime(io::Error),
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
            Self::Runtime(source) => write!(formatter, "cannot start the async runtime: {source}"),
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
    };
    ExitCode::from(u8::from(status))
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
    unix_seconds_of(SystemTime::now())?;
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
            observer_effects(Arc::clone(&directories)),
            result,
        ))
        .map_err(CommandLineFailure::Observer)
}

fn observer_effects(directories: Arc<Directories>) -> ObserverEffects {
    ObserverEffects {
        clock: Arc::new(unix_milliseconds_now),
        monotonic: Arc::new(Instant::now),
        fill_seed: Arc::new(|seed| *seed = SecretKey::generate().to_bytes()),
        fill_probe_nonce: Arc::new(|nonce: &mut Nonce| {
            for (slot, byte) in nonce.iter_mut().zip(SecretKey::generate().to_bytes()) {
                *slot = byte;
            }
        }),
        report: Arc::new(|record| drop(writeln!(io::stderr(), "{record}"))),
        probe_relay: Arc::new(|relay| Box::pin(relay_answers(relay))),
        bind_endpoint: Box::new(|key, gate_key, relays| {
            Box::pin(NodeEndpoint::bind(
                key,
                gate_key,
                relays,
                |nonce| *nonce = SecretKey::generate().to_bytes(),
                |failure| drop(writeln!(io::stderr(), "{failure}")),
            ))
        }),
        start_inbox: Box::new(move || {
            InboxSocket::start(
                &directories,
                std::process::id(),
                &OperatingSystemProcessTable,
            )
            .map(Arc::new)
        }),
        interrupted: Box::pin(async {
            drop(tokio::signal::ctrl_c().await);
        }),
    }
}

fn unix_milliseconds_now() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn unix_seconds_of(instant: SystemTime) -> Result<u64, CommandLineFailure> {
    instant
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_before_epoch| CommandLineFailure::ClockBeforeUnixEpoch)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const SECONDS_AFTER_EPOCH: u64 = 1_700_000_000;

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
}
