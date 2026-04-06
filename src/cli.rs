//! CLI surface for the `oni` binary.
//!
//! The default command shape is `oni <SOURCE> <DESTINATION>`. Hidden
//! subcommands exist only for internal helpers and debugging entry points.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::session::{Chunker, Options, Strategy};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "oni, a file synchronization tool",
    subcommand_negates_reqs = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub run: RunArgs,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(hide = true)]
    Manifest {
        /// File or directory to scan
        path: PathBuf,
    },
    #[command(hide = true)]
    Internal(InternalCommand),
}

#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    /// Show the planned operations without modifying the destination
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,

    /// Increase logging detail. Repeat for more verbosity.
    #[arg(short = 'v', long = "verbose", action = ArgAction::Count)]
    pub verbose: u8,

    /// Delete destination entries that do not exist in the source
    #[arg(long = "delete")]
    pub delete_extraneous: bool,

    /// Force content verification instead of metadata-only planning
    #[arg(long = "checksum")]
    pub checksum: bool,

    /// Transfer strategy preference for later execution stages
    #[arg(long = "strategy", value_enum, default_value_t = StrategyArg::Auto)]
    pub strategy: StrategyArg,

    /// Chunker preference for later CDC-based execution stages
    #[arg(long = "chunker", value_enum, default_value_t = ChunkerArg::FastCdc)]
    pub chunker: ChunkerArg,

    /// Print a structured summary after the dry-run preview
    #[arg(long = "stats")]
    pub stats: bool,

    /// Attach a benchmark label to this sync session
    #[arg(long = "benchmark-tag")]
    pub benchmark_tag: Option<String>,

    /// Source path. Remote endpoints use [user@]host:path.
    #[arg(value_name = "SOURCE", required = true)]
    pub source: Option<String>,

    /// Destination path. Remote endpoints use [user@]host:path.
    #[arg(value_name = "DESTINATION", required = true)]
    pub destination: Option<String>,
}

impl RunArgs {
    /// Convert raw CLI flags into the session-level options object used by the
    /// rest of the codebase.
    pub fn options(&self) -> Options {
        Options {
            dry_run: self.dry_run,
            delete_extraneous: self.delete_extraneous,
            checksum: self.checksum,
            verbose: self.verbose,
            strategy: self.strategy.into(),
            chunker: self.chunker.into(),
            stats: self.stats,
            benchmark_tag: self.benchmark_tag.clone(),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum StrategyArg {
    /// Let the session choose once heuristics exist.
    Auto,
    /// Always replace changed files wholesale.
    Whole,
    /// Use the fixed-size rsync-style delta path.
    Fixed,
    /// Use the content-defined chunking delta path.
    Cdc,
}

impl From<StrategyArg> for Strategy {
    fn from(value: StrategyArg) -> Self {
        match value {
            StrategyArg::Auto => Self::Auto,
            StrategyArg::Whole => Self::Whole,
            StrategyArg::Fixed => Self::Fixed,
            StrategyArg::Cdc => Self::Cdc,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum ChunkerArg {
    /// Reserved for future non-FastCDC chunking experiments.
    Fixed,
    #[value(name = "fastcdc")]
    FastCdc,
    #[value(name = "seqcdc")]
    SeqCdc,
}

impl From<ChunkerArg> for Chunker {
    fn from(value: ChunkerArg) -> Self {
        match value {
            ChunkerArg::Fixed => Self::Fixed,
            ChunkerArg::FastCdc => Self::FastCdc,
            ChunkerArg::SeqCdc => Self::SeqCdc,
        }
    }
}

#[derive(Args, Debug)]
pub struct InternalCommand {
    #[command(subcommand)]
    pub command: InternalSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum InternalSubcommand {
    Serve(ServeCommand),
}

#[derive(Args, Debug)]
pub struct ServeCommand {
    /// Serve framed protocol messages over stdin/stdout.
    #[arg(long)]
    pub stdio: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{ChunkerArg, Cli, Command, InternalSubcommand, StrategyArg};

    #[test]
    fn parses_default_copy_shape_with_early_flags() {
        let cli = Cli::try_parse_from([
            "oni",
            "-n",
            "--delete",
            "--strategy",
            "cdc",
            "--chunker",
            "seqcdc",
            "--stats",
            "--benchmark-tag",
            "nightly",
            "src",
            "dst",
        ])
        .unwrap();

        assert!(cli.command.is_none());
        assert!(cli.run.dry_run);
        assert!(cli.run.delete_extraneous);
        assert!(cli.run.stats);
        assert_eq!(cli.run.strategy, StrategyArg::Cdc);
        assert_eq!(cli.run.chunker, ChunkerArg::SeqCdc);
        assert_eq!(cli.run.benchmark_tag.as_deref(), Some("nightly"));
        assert_eq!(cli.run.source.as_deref(), Some("src"));
        assert_eq!(cli.run.destination.as_deref(), Some("dst"));
    }

    #[test]
    fn parses_hidden_internal_stdio_shape() {
        let cli = Cli::try_parse_from(["oni", "internal", "serve", "--stdio"]).unwrap();

        let Some(Command::Internal(command)) = cli.command else {
            panic!("expected internal command");
        };

        match command.command {
            InternalSubcommand::Serve(serve) => assert!(serve.stdio),
        }
    }
}
