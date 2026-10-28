use std::io::{self, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use estafette::cli::cards::{self, CardCommand};
use estafette::cli::network::{self, NetworkCommand, NetworkCommandError};
use estafette::dirs::{Directories, OperatingSystemFilesystem};
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
) -> Result<(), NetworkCommandError> {
    let directories = Directories::establish(
        |name| std::env::var_os(name),
        &OperatingSystemFilesystem,
        None,
    )?;
    network::run(
        command,
        &directories,
        &OperatingSystemFilesystem,
        |bytes| *bytes = SecretKey::generate().to_bytes(),
        now_unix_seconds(),
        result,
        record,
    )
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
