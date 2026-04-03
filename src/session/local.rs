use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::SessionError;
use crate::manifest::{Manifest, ManifestRoot};
use crate::path::LocalEndpoint;
use crate::plan::{Operation, Plan, PlanOptions};

use super::{Change, ChangeKind, Options};

/// Temporary local preview backend.
///
/// Keeping this logic out of `session::mod` makes the current limitation
/// explicit: only local dry-run planning exists today, while the main session
/// type stays transport-agnostic enough for the future SSH helper path.
pub(super) fn preview(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    options: &Options,
) -> Result<Vec<Change>, SessionError> {
    let source_manifest = Manifest::scan(&source.path)?;
    let destination_manifest = scan_destination_manifest(source, destination, &source_manifest)?;
    let mut plan = Plan::build(
        &source_manifest,
        destination_manifest.as_ref(),
        PlanOptions {
            delete_extraneous: options.delete_extraneous,
        },
    )?;

    verify_matched_operations(
        source,
        destination,
        &source_manifest,
        destination_manifest.as_ref(),
        &mut plan,
        options.checksum,
    )?;

    build_changes(
        source,
        destination,
        &source_manifest,
        destination_manifest.as_ref(),
        &plan,
    )
}

fn scan_destination_manifest(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
) -> Result<Option<Manifest>, SessionError> {
    match source_manifest.root {
        ManifestRoot::Directory => {
            if destination.path.exists() {
                Ok(Some(Manifest::scan(&destination.path)?))
            } else {
                Ok(None)
            }
        }
        ManifestRoot::File => {
            // File roots are a single logical item. Resolve the final target
            // first so `oni a.txt b.txt` plans against `b.txt`, not `a.txt`.
            let target_path = resolve_file_target(source, destination)?;
            if target_path.exists() {
                Ok(Some(Manifest::scan(&target_path)?))
            } else {
                Ok(None)
            }
        }
    }
}

fn build_changes(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    plan: &Plan,
) -> Result<Vec<Change>, SessionError> {
    match source_manifest.root {
        ManifestRoot::Directory => plan
            .operations
            .iter()
            .map(|operation| {
                Ok(Change {
                    kind: ChangeKind::from(operation),
                    path: directory_change_path(source_manifest, destination_manifest, operation)?,
                })
            })
            .collect(),
        ManifestRoot::File => {
            let target_path = resolve_file_target(source, destination)?;
            Ok(plan
                .operations
                .iter()
                .map(|operation| Change {
                    kind: ChangeKind::from(operation),
                    path: target_path.clone(),
                })
                .collect())
        }
    }
}

fn directory_change_path(
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    operation: &Operation,
) -> Result<PathBuf, SessionError> {
    match *operation {
        Operation::Create { source_index }
        | Operation::UpdateData {
            source_index,
            destination_index: _,
        }
        | Operation::UpdateMetadata {
            source_index,
            destination_index: _,
        }
        | Operation::Skip {
            source_index,
            destination_index: _,
        } => Ok(source_manifest.entries[source_index].path.clone()),
        Operation::Delete { destination_index } => Ok(destination_manifest
            .ok_or(SessionError::MissingDestinationManifest)?
            .entries[destination_index]
            .path
            .clone()),
    }
}

fn resolve_file_target(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
) -> Result<PathBuf, SessionError> {
    // Match the common rsync/scp rule: a file copied to an existing directory
    // lands inside that directory with the same file name.
    if destination.path.exists() && destination.path.is_dir() {
        let file_name = source
            .path
            .file_name()
            .ok_or_else(|| SessionError::MissingSourceName {
                path: source.path.clone(),
            })?;

        return Ok(destination.path.join(file_name));
    }

    Ok(destination.path.clone())
}

fn verify_matched_operations(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    plan: &mut Plan,
    verify_contents: bool,
) -> Result<(), SessionError> {
    if !verify_contents {
        return Ok(());
    }

    let Some(destination_manifest) = destination_manifest else {
        return Ok(());
    };

    // The planner stays pure and metadata-based.
    // `--checksum` is intentionally a session-level opt-in because it performs
    // real I/O and should not be the planner's default cost.
    for operation in &mut plan.operations {
        let replacement = match *operation {
            Operation::Skip {
                source_index,
                destination_index,
            }
            | Operation::UpdateMetadata {
                source_index,
                destination_index,
            } => {
                let (source_path, destination_path) = matched_operation_paths(
                    source,
                    destination,
                    source_manifest,
                    destination_manifest,
                    source_index,
                    destination_index,
                )?;

                if files_match(&source_path, &destination_path)? {
                    None
                } else {
                    Some(Operation::UpdateData {
                        source_index,
                        destination_index,
                    })
                }
            }
            Operation::Create { .. } | Operation::UpdateData { .. } | Operation::Delete { .. } => {
                None
            }
        };

        if let Some(operation_with_verified_data_change) = replacement {
            *operation = operation_with_verified_data_change;
        }
    }

    Ok(())
}

fn matched_operation_paths(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: &Manifest,
    source_index: usize,
    destination_index: usize,
) -> Result<(PathBuf, PathBuf), SessionError> {
    match source_manifest.root {
        ManifestRoot::Directory => Ok((
            source
                .path
                .join(&source_manifest.entries[source_index].path),
            destination
                .path
                .join(&destination_manifest.entries[destination_index].path),
        )),
        ManifestRoot::File => Ok((
            source.path.clone(),
            resolve_file_target(source, destination)?,
        )),
    }
}

fn files_match(source: &Path, destination: &Path) -> Result<bool, SessionError> {
    let mut source_file = File::open(source).map_err(|source_error| SessionError::ChecksumIo {
        path: source.to_path_buf(),
        source: source_error,
    })?;
    let mut destination_file =
        File::open(destination).map_err(|source_error| SessionError::ChecksumIo {
            path: destination.to_path_buf(),
            source: source_error,
        })?;
    let mut source_buffer = [0_u8; 8 * 1024];
    let mut destination_buffer = [0_u8; 8 * 1024];

    loop {
        let source_read = source_file
            .read(&mut source_buffer)
            .map_err(|source_error| SessionError::ChecksumIo {
                path: source.to_path_buf(),
                source: source_error,
            })?;
        let destination_read =
            destination_file
                .read(&mut destination_buffer)
                .map_err(|source_error| SessionError::ChecksumIo {
                    path: destination.to_path_buf(),
                    source: source_error,
                })?;

        if source_read != destination_read {
            return Ok(false);
        }

        if source_read == 0 {
            return Ok(true);
        }

        if source_buffer[..source_read] != destination_buffer[..destination_read] {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::thread;
    use std::time::Duration;

    use crate::path::temp_path;
    use crate::session::{Chunker, Options, Request, Strategy};

    #[test]
    fn previews_directory_work_for_local_paths() {
        let source = temp_path("session-source-dir");
        let destination = temp_path("session-destination-dir");

        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("nested/new.txt"), b"new").unwrap();
        fs::write(destination.join("old.txt"), b"old").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options {
                dry_run: true,
                delete_extraneous: true,
                checksum: false,
                verbose: 0,
                strategy: Strategy::Auto,
                chunker: Chunker::FastCdc,
                stats: false,
                benchmark_tag: None,
            },
        )
        .unwrap();

        let preview = request.preview().unwrap();

        assert_eq!(preview.operations.len(), 2);
        assert_eq!(preview.operations[0].kind.to_string(), "create");
        assert_eq!(preview.operations[0].path, PathBuf::from("nested/new.txt"));
        assert_eq!(preview.operations[1].kind.to_string(), "delete");
        assert_eq!(preview.operations[1].path, PathBuf::from("old.txt"));

        fs::remove_dir_all(source).unwrap();
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn previews_single_file_work_against_a_missing_destination_file() {
        let root = temp_path("session-file-root");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("alpha.txt");
        let destination = root.join("beta.txt");
        fs::write(&source, b"oni").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options {
                dry_run: true,
                ..Options::default()
            },
        )
        .unwrap();

        let preview = request.preview().unwrap();

        assert_eq!(preview.operations.len(), 1);
        assert_eq!(preview.operations[0].kind.to_string(), "create");
        assert_eq!(preview.operations[0].path, destination);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checksum_promotes_metadata_only_file_updates_to_data_updates() {
        let root = temp_path("session-checksum-data-update");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("alpha.txt");
        let destination = root.join("beta.txt");
        fs::write(&source, b"aaa").unwrap();
        fs::write(&destination, b"bbb").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options {
                dry_run: true,
                checksum: true,
                ..Options::default()
            },
        )
        .unwrap();

        let preview = request.preview().unwrap();

        assert_eq!(preview.operations.len(), 1);
        assert_eq!(preview.operations[0].kind.to_string(), "update-data");
        assert_eq!(preview.operations[0].path, destination);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checksum_preserves_metadata_only_updates_when_contents_match() {
        let root = temp_path("session-checksum-metadata-update");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("alpha.txt");
        let destination = root.join("beta.txt");
        fs::write(&source, b"oni").unwrap();
        thread::sleep(Duration::from_secs(1));
        fs::write(&destination, b"oni").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options {
                dry_run: true,
                checksum: true,
                ..Options::default()
            },
        )
        .unwrap();

        let preview = request.preview().unwrap();

        assert_eq!(preview.operations.len(), 1);
        assert_eq!(preview.operations[0].kind.to_string(), "update-metadata");
        assert_eq!(preview.operations[0].path, destination);

        fs::remove_dir_all(root).unwrap();
    }
}
