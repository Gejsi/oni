//! Session-level orchestration.
//!
//! This module owns user-visible sync options and dispatch. Backend-specific
//! filesystem work stays in `session::local`, while this front door keeps the
//! public request type small and transport-agnostic.

mod local;

use std::fmt;
use std::path::Path;

use crate::error::{PathError, SessionError};
use crate::path::{parse_endpoint, Endpoint};
use crate::plan::PlanOptions;

/// A parsed sync request.
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

    pub fn apply(
        &self,
        mut on_operation: impl FnMut(crate::plan::Operation, &Path),
    ) -> Result<(), SessionError> {
        match (&self.source, &self.destination) {
            (Endpoint::Local(source), Endpoint::Local(destination)) => {
                local::apply(source, destination, &self.options, &mut on_operation)
            }
            _ => Err(self.unsupported_mode()),
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
    pub plan: PlanOptions,
    pub strategy: Strategy,
    pub chunker: Chunker,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            plan: PlanOptions::default(),
            strategy: Strategy::Auto,
            chunker: Chunker::FastCdc,
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
