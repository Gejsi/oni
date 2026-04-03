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
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Plan(#[from] PlanError),
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
    #[error("mode is not implemented yet: {mode}")]
    UnsupportedMode { mode: String },
    #[error("cannot preview delete operations without a destination manifest")]
    MissingDestinationManifest,
    #[error("source file does not have a file name: {path}")]
    MissingSourceName { path: PathBuf },
    #[error("source and destination paths are required")]
    MissingOperands,
    #[error("internal stdio helper mode is not implemented yet")]
    ServeNotImplemented,
}
