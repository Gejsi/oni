//! Flat CLI surface for the `oni` binary.
//!
//! The command shape stays simple:
//! - normal sync: `oni [OPTIONS] <SOURCE> <DESTINATION>`
//! - internal helper mode: `oni internal <SUBCOMMAND>`

use clap::{Parser, Subcommand, ValueEnum};

use crate::error::SessionError;
use crate::session::{Chunker, Options, Strategy};

#[derive(Parser, Debug)]
#[command(version, about = "oni, a file synchronization tool")]
pub struct Cli {
    /// Delete destination entries that do not exist in the source.
    #[arg(long = "delete", alias = "del")]
    pub delete_extraneous: bool,

    /// Always replace changed files wholesale.
    #[arg(short = 'W', long = "whole-file", conflicts_with = "strategy")]
    pub whole_file: bool,

    /// Force the CDC delta path for changed files.
    #[arg(long = "cdc", conflicts_with = "strategy")]
    pub cdc: bool,

    /// Transfer strategy preference for later execution stages.
    #[arg(long = "strategy", value_enum)]
    pub strategy: Option<StrategyArg>,

    /// Chunker preference for later CDC-based execution stages.
    #[arg(long = "chunker", value_enum, default_value_t = ChunkerArg::FastCdc)]
    pub chunker: ChunkerArg,

    #[command(subcommand)]
    pub internal: Option<InternalCommand>,

    /// Source path. Remote endpoints use [user@]host:path.
    #[arg(value_name = "SOURCE")]
    pub source: Option<String>,

    /// Destination path. Remote endpoints use [user@]host:path.
    #[arg(value_name = "DESTINATION")]
    pub destination: Option<String>,
}

impl Cli {
    /// Convert CLI flags into the engine-level options object.
    pub fn options(&self) -> Options {
        Options {
            plan: crate::plan::PlanOptions {
                delete_extraneous: self.delete_extraneous,
            },
            strategy: self.strategy(),
            chunker: self.chunker.into(),
        }
    }

    pub fn operands(&self) -> Result<(&str, &str), SessionError> {
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

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum InternalCommand {
    #[command(name = "internal", hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalSubcommand,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum InternalSubcommand {
    #[command(name = "serve-stdio")]
    ServeStdio,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum StrategyArg {
    /// Let the session choose once heuristics exist.
    Auto,
    /// Always replace changed files wholesale.
    Whole,
    /// Use the content-defined chunking delta path.
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
pub enum ChunkerArg {
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
#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{ChunkerArg, Cli, InternalCommand, InternalSubcommand, StrategyArg};
    use crate::session::Strategy;

    #[test]
    fn parses_default_copy_shape_with_early_flags() {
        let cli = Cli::try_parse_from([
            "oni",
            "--delete",
            "--strategy",
            "cdc",
            "--chunker",
            "seqcdc",
            "src",
            "dst",
        ])
        .unwrap();

        assert!(cli.delete_extraneous);
        assert!(cli.internal.is_none());
        assert_eq!(cli.strategy, Some(StrategyArg::Cdc));
        assert_eq!(cli.chunker, ChunkerArg::SeqCdc);
        assert_eq!(cli.source.as_deref(), Some("src"));
        assert_eq!(cli.destination.as_deref(), Some("dst"));
    }

    #[test]
    fn parses_internal_stdio_helper_subcommand() {
        let cli = Cli::try_parse_from(["oni", "internal", "serve-stdio"]).unwrap();

        assert_eq!(
            cli.internal,
            Some(InternalCommand::Internal {
                command: InternalSubcommand::ServeStdio,
            })
        );
        assert!(cli.source.is_none());
        assert!(cli.destination.is_none());
    }

    #[test]
    fn whole_file_flag_overrides_default_strategy() {
        let cli = Cli::try_parse_from(["oni", "--whole-file", "src", "dst"]).unwrap();

        assert_eq!(cli.options().strategy, Strategy::Whole);
    }

    #[test]
    fn cdc_flag_overrides_default_strategy() {
        let cli = Cli::try_parse_from(["oni", "--cdc", "src", "dst"]).unwrap();

        assert_eq!(cli.options().strategy, Strategy::Cdc);
    }
}
