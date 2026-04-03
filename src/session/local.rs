use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::SessionError;
use crate::fs as fs_ops;
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
    let prepared = prepare(source, destination, options, options.checksum)?;

    build_changes(
        source,
        destination,
        &prepared.source_manifest,
        prepared.destination_manifest.as_ref(),
        &prepared.plan,
    )
}

pub(super) fn apply(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    options: &Options,
) -> Result<Vec<Change>, SessionError> {
    // Real execution must be correct even when the user did not ask dry-run to
    // pay for `--checksum`, so the local apply path always verifies matched
    // same-size files before deciding they are metadata-only or skipped.
    let prepared = prepare(source, destination, options, true)?;

    execute_plan(
        source,
        destination,
        &prepared.source_manifest,
        prepared.destination_manifest.as_ref(),
        &prepared.plan,
    )?;

    build_changes(
        source,
        destination,
        &prepared.source_manifest,
        prepared.destination_manifest.as_ref(),
        &prepared.plan,
    )
}

struct PreparedPlan {
    source_manifest: Manifest,
    destination_manifest: Option<Manifest>,
    plan: Plan,
}

fn prepare(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    options: &Options,
    verify_contents: bool,
) -> Result<PreparedPlan, SessionError> {
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
        verify_contents,
    )?;

    Ok(PreparedPlan {
        source_manifest,
        destination_manifest,
        plan,
    })
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

fn execute_plan(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    plan: &Plan,
) -> Result<(), SessionError> {
    for operation in &plan.operations {
        match *operation {
            Operation::Create { source_index } => {
                let (source_path, destination_path) =
                    create_operation_paths(source, destination, source_manifest, source_index)?;
                fs_ops::replace_file(&source_path, &destination_path)?;
            }
            Operation::UpdateData {
                source_index,
                destination_index,
            } => {
                let (source_path, destination_path) = matched_operation_paths(
                    source,
                    destination,
                    source_manifest,
                    destination_manifest.ok_or(SessionError::MissingDestinationManifest)?,
                    source_index,
                    destination_index,
                )?;
                fs_ops::replace_file(&source_path, &destination_path)?;
            }
            Operation::UpdateMetadata {
                source_index,
                destination_index,
            } => {
                let (source_path, destination_path) = matched_operation_paths(
                    source,
                    destination,
                    source_manifest,
                    destination_manifest.ok_or(SessionError::MissingDestinationManifest)?,
                    source_index,
                    destination_index,
                )?;
                fs_ops::sync_metadata(&source_path, &destination_path)?;
            }
            Operation::Delete { destination_index } => {
                let destination_path = delete_operation_path(
                    source,
                    destination,
                    destination_manifest.ok_or(SessionError::MissingDestinationManifest)?,
                    destination_index,
                )?;
                fs_ops::remove_file(&destination_path)?;
            }
            Operation::Skip {
                source_index: _,
                destination_index: _,
            } => {}
        }
    }

    Ok(())
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

fn create_operation_paths(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    source_index: usize,
) -> Result<(PathBuf, PathBuf), SessionError> {
    match source_manifest.root {
        ManifestRoot::Directory => {
            let relative_path = &source_manifest.entries[source_index].path;
            Ok((
                source.path.join(relative_path),
                destination.path.join(relative_path),
            ))
        }
        ManifestRoot::File => Ok((
            source.path.clone(),
            resolve_file_target(source, destination)?,
        )),
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

fn delete_operation_path(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    destination_manifest: &Manifest,
    destination_index: usize,
) -> Result<PathBuf, SessionError> {
    match destination_manifest.root {
        ManifestRoot::Directory => Ok(destination
            .path
            .join(&destination_manifest.entries[destination_index].path)),
        ManifestRoot::File => resolve_file_target(source, destination),
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
    use std::fs::File;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, UNIX_EPOCH};

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

    #[test]
    fn applies_single_file_create_and_preserves_contents() {
        let root = temp_path("session-apply-create");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("alpha.txt");
        let destination = root.join("beta.txt");
        fs::write(&source, b"oni").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options::default(),
        )
        .unwrap();

        let applied = request.apply().unwrap();

        assert_eq!(applied.operations.len(), 1);
        assert_eq!(applied.operations[0].kind.to_string(), "create");
        assert_eq!(applied.operations[0].path, destination);
        assert_eq!(fs::read(&destination).unwrap(), b"oni");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applies_equal_size_changed_files_even_without_checksum_flag() {
        let root = temp_path("session-apply-same-size");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("alpha.txt");
        let destination = root.join("beta.txt");
        fs::write(&source, b"aaa").unwrap();
        fs::write(&destination, b"bbb").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options::default(),
        )
        .unwrap();

        let applied = request.apply().unwrap();

        assert_eq!(applied.operations.len(), 1);
        assert_eq!(applied.operations[0].kind.to_string(), "update-data");
        assert_eq!(fs::read(&destination).unwrap(), b"aaa");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applies_metadata_only_updates_without_rewriting_file_contents() {
        let root = temp_path("session-apply-metadata");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("alpha.txt");
        let destination = root.join("beta.txt");
        fs::write(&source, b"oni").unwrap();
        fs::write(&destination, b"oni").unwrap();

        let source_time = UNIX_EPOCH + Duration::from_secs(20_000);
        let destination_time = UNIX_EPOCH + Duration::from_secs(10_000);

        File::open(&source)
            .unwrap()
            .set_modified(source_time)
            .unwrap();
        File::open(&destination)
            .unwrap()
            .set_modified(destination_time)
            .unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options::default(),
        )
        .unwrap();

        let applied = request.apply().unwrap();

        assert_eq!(applied.operations.len(), 1);
        assert_eq!(applied.operations[0].kind.to_string(), "update-metadata");
        assert_eq!(fs::read(&destination).unwrap(), b"oni");

        let destination_modified = fs::metadata(&destination).unwrap().modified().unwrap();
        let delta = destination_modified
            .duration_since(source_time)
            .unwrap_or_else(|error| error.duration());
        assert!(
            delta <= Duration::from_secs(1),
            "destination mtime drifted by {:?}",
            delta
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applies_directory_deletes_when_requested() {
        let source = temp_path("session-apply-delete-source");
        let destination = temp_path("session-apply-delete-destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();

        fs::write(source.join("keep.txt"), b"keep").unwrap();
        fs::write(destination.join("keep.txt"), b"keep").unwrap();
        fs::write(destination.join("remove.txt"), b"remove").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options {
                delete_extraneous: true,
                ..Options::default()
            },
        )
        .unwrap();

        let applied = request.apply().unwrap();

        assert_eq!(applied.summary().delete, 1);
        assert!(!destination.join("remove.txt").exists());
        assert!(destination.join("keep.txt").exists());

        fs::remove_dir_all(source).unwrap();
        fs::remove_dir_all(destination).unwrap();
    }
}
