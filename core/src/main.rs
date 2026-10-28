use std::fmt;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use estafette::cli::cards::{self, CardCommand};
use estafette::cli::network::{self, NetworkCommand, NetworkCommandError};
use estafette::dirs::{Directories, DirectoriesError, OperatingSystemFilesystem};
use estafette::errors::ExitStatus;
use iroh::SecretKey;

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
}

#[derive(Debug)]
enum CommandLineFailure {
    Directories(DirectoriesError),
    ClockBeforeUnixEpoch,
    Network(NetworkCommandError),
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
