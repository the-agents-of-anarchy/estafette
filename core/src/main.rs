use std::io;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use estafette::cli::cards::{self, CardCommand};
use estafette::dirs::OperatingSystemFilesystem;
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
    };
    ExitCode::from(u8::from(status))
}
