//! Binary entry point and CLI for Oni.
//!
//! The public shape stays small:
//! - normal sync: `oni [OPTIONS] <SOURCE> <DESTINATION>`
//! - hidden helper mode: `oni internal serve --stdio`

use clap::{Parser, Subcommand};

use oni::error::OniError;

#[derive(Parser, Debug)]
#[command(version, about = "oni, a file synchronization tool")]
struct Cli {
    /// Delete destination entries that do not exist in the source.
    #[arg(long = "delete")]
    delete_extraneous: bool,

    #[command(subcommand)]
    command: Option<Command>,

    /// Source path. Remote endpoints use [user@]host:path.
    #[arg(value_name = "SOURCE")]
    source: Option<String>,

    /// Destination path. Remote endpoints use [user@]host:path.
    #[arg(value_name = "DESTINATION")]
    destination: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    #[command(name = "internal", hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
    },
}

#[derive(Debug, PartialEq, Eq, Subcommand)]
enum InternalCommand {
    #[command(name = "serve")]
    Serve {
        #[arg(long = "stdio", required = true)]
        stdio: bool,
    },
}

fn main() -> Result<(), OniError> {
    Ok(())
}
