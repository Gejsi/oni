use std::fmt;
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::error::ManifestError;

/// The manifest currently records only regular files
///
/// This is the smallest useful unit for a sync inventory. A path may later
/// refer to a directory, symlink, device, or something else, but that policy
/// does not belong in the first version of the scanner
#[derive(Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
}

impl fmt::Display for EntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => f.write_str("file"),
        }
    }
}

/// Metadata captured for each manifest entry
///
/// For a path like `letters/monday.txt`, this stores:
/// - its length in bytes
/// - its modification time, if one is available
///
/// The scanner keeps this small on purpose. Its job is to inventory files, not
/// to preserve every filesystem detail yet
#[derive(Debug, PartialEq, Eq)]
pub struct EntryMetadata {
    pub len: u64,
    pub modified_unix_secs: Option<u64>,
}

/// A single manifest entry, stored relative to the scanned root
///
/// If the scanned root is `/archive`, an entry may be `photos/cover.jpg`
///
/// If the scanned root is `/archive/todo.txt`, the entry is `todo.txt`
#[derive(Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub metadata: EntryMetadata,
}

impl ManifestEntry {
    /// The relative-path rule is:
    /// - if the root itself is a file, keep only the file name
    /// - if the root is a directory, strip the root prefix
    ///
    /// So `/docs/todo.txt` scanned as a file becomes `todo.txt`
    ///
    /// And `/docs/work/todo.txt` scanned under `/docs` becomes `work/todo.txt`
    fn build_file_entry(
        path: &Path,
        root: &Path,
        metadata: &Metadata,
    ) -> Result<Self, ManifestError> {
        let relative_path = if path == root {
            path.file_name()
                .map(PathBuf::from)
                .ok_or_else(|| ManifestError::MissingFileName {
                    path: path.to_path_buf(),
                })?
        } else {
            path.strip_prefix(root)
                .map_err(|source| ManifestError::RelativePath {
                    path: path.to_path_buf(),
                    source,
                })?
                .to_path_buf()
        };

        Ok(Self {
            path: relative_path,
            kind: EntryKind::File,
            metadata: EntryMetadata {
                len: metadata.len(),
                modified_unix_secs: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs()),
            },
        })
    }
}

/// Whether the scan root comes from a file or from a directory
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ManifestRoot {
    File,
    Directory,
}

impl fmt::Display for ManifestRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => f.write_str("file"),
            Self::Directory => f.write_str("directory"),
        }
    }
}

/// A stable inventory of regular files under a scan root
///
/// A directory root such as `/archive` may yield entries like:
/// - `letters/first.txt`
/// - `letters/second.txt`
/// - `pictures/summer.jpg`
///
/// A file root such as `/archive/readme.txt` yields exactly one entry:
/// - `readme.txt`
#[derive(Debug, PartialEq, Eq)]
pub struct Manifest {
    pub root: ManifestRoot,
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    /// Scans `root` into a manifest of regular files
    ///
    /// A directory such as `/data` may produce `notes/day-1.txt` and
    /// `photos/cover.jpg`
    ///
    /// A file such as `/data/todo.txt` produces one entry, `todo.txt`
    pub fn scan(root: &Path) -> Result<Self, ManifestError> {
        if !root.exists() {
            return Err(ManifestError::MissingPath {
                path: root.to_path_buf(),
            });
        }

        if root.is_file() {
            let metadata = fs::metadata(root).map_err(|source| ManifestError::Metadata {
                path: root.to_path_buf(),
                source,
            })?;

            return Ok(Self {
                root: ManifestRoot::File,
                entries: vec![ManifestEntry::build_file_entry(root, root, &metadata)?],
            });
        }

        let mut entries = Vec::new();

        for entry in WalkDir::new(root) {
            let entry = entry.map_err(|source| ManifestError::Walk {
                root: root.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(path).map_err(|source| ManifestError::Metadata {
                path: path.to_path_buf(),
                source,
            })?;

            if !metadata.is_file() {
                continue;
            }

            entries.push(ManifestEntry::build_file_entry(path, root, &metadata)?);
        }

        // The walker does not promise a stable order
        // Sorting keeps the inventory deterministic
        // For instance, `letters/a.txt` will always come before `letters/b.txt`
        entries.sort_by(|left, right| left.path.cmp(&right.path));

        Ok(Self {
            root: ManifestRoot::Directory,
            entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{Manifest, ManifestRoot};

    #[test]
    fn scans_a_single_file() {
        let root = temp_path("single-file");
        fs::create_dir_all(&root).unwrap();
        let file_path = root.join("a.txt");
        fs::write(&file_path, b"oni").unwrap();

        let manifest = Manifest::scan(&file_path).unwrap();

        assert_eq!(manifest.root, ManifestRoot::File);
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(manifest.entries[0].path, PathBuf::from("a.txt"));
        assert_eq!(manifest.entries[0].metadata.len, 3);
        assert_eq!(
            manifest.entries[0].metadata.modified_unix_secs,
            fs::metadata(&file_path)
                .unwrap()
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scans_a_directory_in_sorted_order() {
        let root = temp_path("directory");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("z.txt"), b"z").unwrap();
        fs::write(root.join("nested/a.txt"), b"a").unwrap();

        let manifest = Manifest::scan(&root).unwrap();

        assert_eq!(manifest.root, ManifestRoot::Directory);
        assert_eq!(manifest.entries.len(), 2);
        assert_eq!(manifest.entries[0].path, PathBuf::from("nested/a.txt"));
        assert_eq!(manifest.entries[1].path, PathBuf::from("z.txt"));

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scans_an_empty_directory() {
        let root = temp_path("empty-directory");
        fs::create_dir_all(&root).unwrap();

        let manifest = Manifest::scan(&root).unwrap();

        assert_eq!(manifest.root, ManifestRoot::Directory);
        assert!(manifest.entries.is_empty());

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scans_deeply_nested_paths() {
        let root = temp_path("deep-directory");
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::write(root.join("a/b/c/notes.txt"), b"hello").unwrap();
        fs::write(root.join("a/top.txt"), b"world").unwrap();

        let manifest = Manifest::scan(&root).unwrap();
        let paths: Vec<_> = manifest
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect();

        assert_eq!(
            paths,
            vec![PathBuf::from("a/b/c/notes.txt"), PathBuf::from("a/top.txt")]
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn keeps_names_with_spaces() {
        let root = temp_path("spaces");
        fs::create_dir_all(root.join("daily notes")).unwrap();
        fs::write(root.join("daily notes/today plan.txt"), b"text").unwrap();

        let manifest = Manifest::scan(&root).unwrap();

        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(
            manifest.entries[0].path,
            PathBuf::from("daily notes/today plan.txt")
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn returns_all_paths_in_sorted_order() {
        let root = temp_path("sorted-paths");
        fs::create_dir_all(root.join("letters")).unwrap();
        fs::create_dir_all(root.join("pictures")).unwrap();
        fs::write(root.join("letters/b.txt"), b"b").unwrap();
        fs::write(root.join("letters/a.txt"), b"a").unwrap();
        fs::write(root.join("pictures/cover.jpg"), b"jpg").unwrap();

        let manifest = Manifest::scan(&root).unwrap();
        let paths: Vec<_> = manifest
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect();

        assert_eq!(
            paths,
            vec![
                PathBuf::from("letters/a.txt"),
                PathBuf::from("letters/b.txt"),
                PathBuf::from("pictures/cover.jpg"),
            ]
        );

        fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinks() {
        let root = temp_path("symlink");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("real.txt"), b"real").unwrap();
        symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

        let manifest = Manifest::scan(&root).unwrap();
        let paths: Vec<_> = manifest
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect();

        assert_eq!(paths, vec![PathBuf::from("real.txt")]);

        fs::remove_file(root.join("link.txt")).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn returns_an_error_for_missing_paths() {
        let root = temp_path("missing");

        let error = Manifest::scan(&root).unwrap_err();

        assert!(matches!(
            error,
            crate::error::ManifestError::MissingPath { .. }
        ));
    }

    fn temp_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("oni-{label}-{unique}"))
    }
}
