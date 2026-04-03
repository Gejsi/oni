//! Typed error definitions for the current `v2` codebase.
//!
//! The goal is to keep domain failures specific near their source and only
//! collapse them into `OniError` at the binary boundary.

use std::io;
use std::path::{PathBuf, StripPrefixError};

use thiserror::Error;

use crate::manifest::ManifestRoot;
use crate::protocol::{Capability, PeerRole, ProtocolVersion, VersionRange};

#[derive(Debug, Error)]
pub enum OniError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Path(#[from] PathError),
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error(transparent)]
    Strategy(#[from] StrategyError),
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
pub enum PathError {
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
    #[error("incompatible peer roles for one session: local={local}, remote={remote}")]
    IncompatibleRoles { local: PeerRole, remote: PeerRole },
    #[error("no shared protocol version: local supports {local}, remote supports {remote}")]
    NoSharedVersion {
        local: VersionRange,
        remote: VersionRange,
    },
    #[error("missing required negotiated capability: {capability}")]
    MissingRequiredCapability { capability: Capability },
    #[error("unknown protocol message kind: {kind}")]
    UnknownMessageKind { kind: u16 },
    #[error("unknown peer role id in hello: {value}")]
    UnknownPeerRole { value: u8 },
    #[error("unknown capability id in hello: {id}")]
    UnknownCapabilityId { id: u16 },
    #[error("truncated protocol message {message} while reading {field}")]
    TruncatedMessage {
        message: &'static str,
        field: &'static str,
    },
    #[error("invalid UTF-8 in protocol message {message} field {field}")]
    InvalidTextField {
        message: &'static str,
        field: &'static str,
    },
    #[error("protocol message {message} field {field} is too large: {len} bytes")]
    FieldTooLarge {
        message: &'static str,
        field: &'static str,
        len: usize,
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
    #[error("failed to launch helper process: {target}")]
    Launch {
        target: String,
        #[source]
        source: io::Error,
    },
    #[error("helper process did not expose piped {pipe}: {target}")]
    MissingPipe { pipe: &'static str, target: String },
}

#[derive(Debug, Error)]
pub enum StrategyError {
    #[error("failed to {operation} during fixed-size delta processing")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("fixed-size delta recipe references a missing basis block: {block_index}")]
    InvalidBlockReference { block_index: usize },
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
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Strategy(#[from] StrategyError),
    #[error("failed to access file during checksum preview: {path}")]
    ChecksumIo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to {operation}: {path}")]
    ApplyIo {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("transfer strategy is not implemented yet: {strategy}")]
    UnsupportedStrategy { strategy: String },
    #[error("mode is not implemented yet: {mode}")]
    UnsupportedMode { mode: String },
    #[error("cannot preview delete operations without a destination manifest")]
    MissingDestinationManifest,
    #[error("source file does not have a file name: {path}")]
    MissingSourceName { path: PathBuf },
    #[error("source and destination paths are required")]
    MissingOperands,
}
