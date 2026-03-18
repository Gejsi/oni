use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelativePath(PathBuf);

impl RelativePath {
    pub fn new(path: PathBuf) -> Option<Self> {
        if path.is_absolute() || path.components().any(|component| matches!(component, std::path::Component::ParentDir)) {
            return None;
        }

        Some(Self(path))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    pub len: u64,
    pub modified_unix_secs: Option<u64>,
    pub permissions: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: RelativePath,
    pub metadata: FileMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Manifest {
    pub entries: Vec<FileEntry>,
}
