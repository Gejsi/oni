//! Binary entry point for Oni.
//!
//! The executable keeps only thin orchestration here: parse CLI arguments,
//! dispatch internal subcommands if requested, and hand sync work to the library.

use clap::Parser;

use oni::cli::{Cli, InternalCommand, InternalSubcommand};
use oni::error::OniError;
use oni::internal;
use oni::session::Request;

fn main() -> Result<(), OniError> {
    let cli = Cli::parse();

    match cli.internal.clone() {
        Some(InternalCommand::Internal { command }) => run_internal(command),
        None => run(cli),
    }
}

fn run(cli: Cli) -> Result<(), OniError> {
    let options = cli.options();
    let (source, destination) = cli.operands()?;
    let request = Request::from_args(source, destination, options)?;
    request.apply(|_, _| {
        // TODO: user-facing reporting was intentionally removed for now.
    })?;

    Ok(())
}

fn run_internal(command: InternalSubcommand) -> Result<(), OniError> {
    match command {
        InternalSubcommand::ServeStdio => internal::stdio::serve(),
    }
}
