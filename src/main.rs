//! Binary entry point and CLI for Oni.
//!
//! The public shape stays small:
//! - normal sync: `oni [OPTIONS] <SOURCE> <DESTINATION>`
//! - hidden helper mode: `oni internal serve --stdio`

use std::io;

use clap::{Parser, Subcommand, ValueEnum};

use oni::error::{OniError, SessionError};
use oni::protocol::{current_implementation, CapabilitySet, Hello, Limits, Message, PeerRole};
use oni::session::{Chunker, Options, Request, Strategy};
use oni::transport::stdio::Connection;

#[derive(Parser, Debug)]
#[command(version, about = "oni, a file synchronization tool")]
struct Cli {
    /// Delete destination entries that do not exist in the source.
    #[arg(long = "delete")]
    delete_extraneous: bool,

    /// Always replace changed files wholesale.
    #[arg(short = 'W', long = "whole-file", conflicts_with = "strategy")]
    whole_file: bool,

    /// Force the CDC delta path for changed files.
    #[arg(long = "cdc", conflicts_with = "strategy")]
    cdc: bool,

    /// Transfer strategy preference for later execution stages.
    #[arg(long = "strategy", value_enum)]
    strategy: Option<StrategyArg>,

    /// Chunker preference for later CDC-based execution stages.
    #[arg(long = "chunker", value_enum, default_value_t = ChunkerArg::FastCdc)]
    chunker: ChunkerArg,

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

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum StrategyArg {
    Auto,
    Whole,
    Cdc,
}

impl From<StrategyArg> for Strategy {
    fn from(value: StrategyArg) -> Self {
        match value {
            StrategyArg::Auto => Self::Auto,
            StrategyArg::Whole => Self::Whole,
            StrategyArg::Cdc => Self::Cdc,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ChunkerArg {
    #[value(name = "fastcdc")]
    FastCdc,
    #[value(name = "seqcdc")]
    SeqCdc,
}

impl From<ChunkerArg> for Chunker {
    fn from(value: ChunkerArg) -> Self {
        match value {
            ChunkerArg::FastCdc => Self::FastCdc,
            ChunkerArg::SeqCdc => Self::SeqCdc,
        }
    }
}

impl Cli {
    fn options(&self) -> Options {
        Options {
            plan: oni::plan::PlanOptions {
                delete_extraneous: self.delete_extraneous,
            },
            strategy: self.strategy(),
            chunker: self.chunker.into(),
        }
    }

    fn operands(&self) -> Result<(&str, &str), SessionError> {
        let source = self
            .source
            .as_deref()
            .ok_or(SessionError::MissingOperands)?;
        let destination = self
            .destination
            .as_deref()
            .ok_or(SessionError::MissingOperands)?;
        Ok((source, destination))
    }

    fn strategy(&self) -> Strategy {
        if self.whole_file {
            return Strategy::Whole;
        }

        if self.cdc {
            return Strategy::Cdc;
        }

        self.strategy.map(Into::into).unwrap_or(Strategy::Auto)
    }
}

fn main() -> Result<(), OniError> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Internal {
            command: InternalCommand::Serve { stdio },
        }) => {
            debug_assert!(stdio);
            let stdin = io::stdin();
            let stdout = io::stdout();
            let mut connection = Connection::new(
                stdin.lock(),
                stdout.lock(),
                Limits::default().max_frame_bytes() as usize,
            );

            let Some(frame) = connection.receive_frame()? else {
                return Ok(());
            };
            let Message::Hello(remote_hello) = Message::decode(&frame)?;

            let local_hello = Hello::for_current(
                PeerRole::Helper,
                current_implementation(),
                CapabilitySet::default(),
                Limits::default(),
            );

            Hello::negotiate(&local_hello, &remote_hello)?;
            connection.send_frame(&Message::Hello(local_hello).encode()?)?;
            Ok(())
        }
        None => {
            let options = cli.options();
            let (source, destination) = cli.operands()?;
            let request = Request::from_args(source, destination, options)?;
            request.apply(|_, _| {
                // TODO: user-facing reporting was intentionally removed for now.
            })?;
            Ok(())
        }
    }
}
