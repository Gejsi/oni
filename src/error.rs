//! Typed error definitions for the current `v2` codebase.
//!
//! The goal is to keep domain failures specific near their source and only
//! collapse them into `OniError` at the binary boundary.

use std::io;
use std::path::{PathBuf, StripPrefixError};

use thiserror::Error;

use crate::manifest::ManifestRoot;
use crate::transport::protocol::{ProtocolVersion, VersionRange};

#[derive(Debug, Error)]
pub enum OniError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Path(#[from] EndpointError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Chunker(#[from] ChunkerError),
    #[error(transparent)]
    Session(#[from] SessionError),
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("path does not exist: {path}")]
    MissingPath { path: PathBuf },
    #[error("failed to walk filesystem under: {root}")]
    Walk {
        root: PathBuf,
        #[source]
        source: walkdir::Error,
    },
    #[error("failed to read metadata for: {path}")]
    Metadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to compute relative path for: {path}")]
    RelativePath {
        path: PathBuf,
        #[source]
        source: StripPrefixError,
    },
    #[error("path does not have a file name: {path}")]
    MissingFileName { path: PathBuf },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanError {
    #[error(
        "source root {source_root} cannot be planned against destination root {destination_root}"
    )]
    RootKindMismatch {
        source_root: ManifestRoot,
        destination_root: ManifestRoot,
    },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EndpointError {
    #[error("remote path is missing a host: {spec}")]
    MissingRemoteHost { spec: String },
    #[error("remote path is missing a path after the host: {spec}")]
    MissingRemotePath { spec: String },
    #[error("remote path is missing a user before '@': {spec}")]
    MissingRemoteUser { spec: String },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("invalid protocol version range: {min}..={max}")]
    InvalidVersionRange {
        min: ProtocolVersion,
        max: ProtocolVersion,
    },
    #[error("invalid protocol limit {field}: {value}")]
    InvalidLimit { field: &'static str, value: u32 },
    #[error("invalid protocol magic: {found:?}")]
    InvalidMagic { found: [u8; 4] },
    #[error("no shared protocol version: local supports {local}, remote supports {remote}")]
    NoSharedVersion {
        local: VersionRange,
        remote: VersionRange,
    },
    #[error("truncated protocol message {message} while reading {field}")]
    TruncatedMessage {
        message: &'static str,
        field: &'static str,
    },
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("failed to {operation} on stdio transport")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("stdio frame is too large: {len} bytes exceeds the configured maximum {max}")]
    FrameTooLarge { len: usize, max: usize },
    #[error("unexpected EOF while trying to {operation} on stdio transport")]
    UnexpectedEof { operation: &'static str },
}

#[derive(Debug, Error)]
pub enum ChunkerError {
    #[error("invalid FastCDC {field}: {value} is outside the supported range {min}..={max}")]
    InvalidBound {
        field: &'static str,
        value: u32,
        min: u32,
        max: u32,
    },
    #[error("invalid FastCDC chunk-size ordering: min={min_size}, avg={avg_size}, max={max_size}")]
    InvalidOrdering {
        min_size: u32,
        avg_size: u32,
        max_size: u32,
    },
    #[error("failed to {operation}")]
    FastCdcIo {
        operation: &'static str,
        #[source]
        source: fastcdc::v2020::Error,
    },
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("failed to {operation}: {path}")]
    ApplyIo {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
