use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OniError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
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
