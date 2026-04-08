//! Session-level orchestration.
//!
//! This module owns user-visible sync options and dispatch. Backend-specific
//! filesystem work stays in `session::local`, while this front door keeps the
//! public request type small and transport-agnostic.

mod local;
mod preview;

use std::fmt;

use crate::error::{PathError, SessionError};
use crate::path::{parse_endpoint, Endpoint};

pub use preview::{Change, ChangeKind, Preview, Summary};

/// A parsed sync request.
///
/// This type stays intentionally small: it owns parsed endpoints and user
/// options, then dispatches into the currently available backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub source: Endpoint,
    pub destination: Endpoint,
    pub options: Options,
}

impl Request {
    pub fn from_args(source: &str, destination: &str, options: Options) -> Result<Self, PathError> {
        Ok(Self {
            source: parse_endpoint(source)?,
            destination: parse_endpoint(destination)?,
            options,
        })
    }

    pub fn mode(&self) -> Mode {
        match (&self.source, &self.destination) {
            (Endpoint::Local(_), Endpoint::Local(_)) => Mode::LocalToLocal,
            (Endpoint::Local(_), Endpoint::Remote(_)) => Mode::LocalToRemote,
            (Endpoint::Remote(_), Endpoint::Local(_)) => Mode::RemoteToLocal,
            (Endpoint::Remote(_), Endpoint::Remote(_)) => Mode::RemoteToRemote,
        }
    }

    pub fn preview(&self) -> Result<Preview, SessionError> {
        if !self.options.dry_run {
            return self.apply();
        }

        let operations = match (&self.source, &self.destination) {
            // Keep the local-only preview path isolated so its filesystem
            // assumptions do not spread into the future SSH-backed session.
            (Endpoint::Local(source), Endpoint::Local(destination)) => {
                local::preview(source, destination, &self.options)?
            }
            _ => return Err(self.unsupported_mode()),
        };

        Ok(self.finish(operations))
    }

    pub fn apply(&self) -> Result<Preview, SessionError> {
        let operations = match (&self.source, &self.destination) {
            (Endpoint::Local(source), Endpoint::Local(destination)) => {
                local::apply(source, destination, &self.options)?
            }
            _ => return Err(self.unsupported_mode()),
        };

        Ok(self.finish(operations))
    }

    fn finish(&self, operations: Vec<Change>) -> Preview {
        Preview {
            mode: self.mode(),
            operations,
        }
    }

    fn unsupported_mode(&self) -> SessionError {
        SessionError::UnsupportedMode {
            mode: self.mode().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub dry_run: bool,
    pub delete_extraneous: bool,
    pub checksum: bool,
    pub verbose: u8,
    pub strategy: Strategy,
    pub chunker: Chunker,
    pub stats: bool,
    pub benchmark_tag: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dry_run: false,
            delete_extraneous: false,
            checksum: false,
            verbose: 0,
            strategy: Strategy::Auto,
            chunker: Chunker::FastCdc,
            stats: false,
            benchmark_tag: None,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Mode {
    LocalToLocal,
    LocalToRemote,
    RemoteToLocal,
    RemoteToRemote,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalToLocal => f.write_str("local -> local"),
            Self::LocalToRemote => f.write_str("local -> remote"),
            Self::RemoteToLocal => f.write_str("remote -> local"),
            Self::RemoteToRemote => f.write_str("remote -> remote"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Strategy {
    Auto,
    Whole,
    Cdc,
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Whole => f.write_str("whole"),
            Self::Cdc => f.write_str("cdc"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Chunker {
    FastCdc,
    SeqCdc,
}

impl fmt::Display for Chunker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FastCdc => f.write_str("fastcdc"),
            Self::SeqCdc => f.write_str("seqcdc"),
        }
    }
}
