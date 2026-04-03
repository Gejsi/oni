use std::io;
use std::path::{PathBuf, StripPrefixError};

use thiserror::Error;

use crate::manifest::ManifestRoot;

#[derive(Debug, Error)]
pub enum OniError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Path(#[from] PathError),
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

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error("execution is not implemented yet; use --dry-run to inspect the plan")]
    ApplyNotImplemented,
    #[error("failed to access file during checksum preview: {path}")]
    ChecksumIo {
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
