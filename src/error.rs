use std::path::PathBuf;

use thiserror::Error;

use crate::manifest::ManifestRoot;

#[derive(Debug, Error)]
pub enum OniError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Plan(#[from] PlanError),
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("path does not exist: {path}")]
    MissingPath { path: PathBuf },
    #[error("failed to walk filesystem under: {root}")]
    Walk { root: PathBuf },
    #[error("failed to read metadata for: {path}")]
    Metadata { path: PathBuf },
    #[error("failed to compute relative path for: {path}")]
    RelativePath { path: PathBuf },
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
