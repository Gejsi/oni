use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::error::OniError;
use crate::manifest::Manifest;

#[derive(Parser, Debug)]
#[command(version, about = "oni")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Manifest {
        /// File or directory to scan
        path: PathBuf,
    },
}

pub fn run() -> Result<(), OniError> {
    let cli = Cli::parse();

    match cli.command {
        Command::Manifest { path } => {
            let manifest = Manifest::scan(&path)?;

            for entry in manifest.entries {
                println!(
                    "{}\t{}\t{}",
                    entry.kind.as_str(),
                    entry.metadata.len,
                    entry.path.display()
                );
            }

            Ok(())
        }
    }
}
