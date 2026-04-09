//! Local-only apply backend.
//!
//! This file intentionally owns one flow only:
//! - scan source and destination
//! - stream planner operations
//! - refine same-size matches by comparing bytes
//! - execute the final operation
//! - emit the final operation to the caller
//!
//! There is one execution flow only. Any future non-applying mode should be
//! built as its own clean flow instead of leaking optional branches into the
//! real executor.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::chunker::fastcdc::FastCdcConfig;
use crate::endpoint::LocalEndpoint;
use crate::error::SessionError;
use crate::manifest::{Manifest, ManifestRoot};
use crate::plan::{for_each_operation, Operation};
use crate::staging;
use crate::strategy::cdc;

use super::Options;

pub fn apply(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    options: &Options,
    mut on_operation: impl FnMut(Operation, &Path),
) -> Result<(), SessionError> {
    let source_manifest = Manifest::scan(&source.path)?;
    let destination_manifest = scan_destination_manifest(source, destination, &source_manifest)?;
    let mut execution_error = None;

    for_each_operation(
        &source_manifest,
        destination_manifest.as_ref(),
        options.plan,
        |planned| {
            if execution_error.is_some() {
                return Ok(());
            }

            match apply_operation(
                source,
                destination,
                &source_manifest,
                destination_manifest.as_ref(),
                planned,
                options,
            ) {
                Ok(operation) => {
                    if let Err(error) = with_operation_path(
                        source,
                        destination,
                        &source_manifest,
                        destination_manifest.as_ref(),
                        operation,
                        |path| on_operation(operation, path),
                    ) {
                        execution_error = Some(error);
                    }
                    Ok(())
                }
                Err(error) => {
                    execution_error = Some(error);
                    Ok(())
                }
            }
        },
    )?;

    if let Some(error) = execution_error {
        Err(error)
    } else {
        Ok(())
    }
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

fn apply_operation(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    planned: Operation,
    options: &Options,
) -> Result<Operation, SessionError> {
    let operation = refine_operation(
        source,
        destination,
        source_manifest,
        destination_manifest,
        planned,
    )?;

    match operation {
        Operation::Create { source_index } => {
            let (source_path, destination_path) =
                create_operation_paths(source, destination, source_manifest, source_index)?;
            staging::replace_file(&source_path, &destination_path)?;
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
            apply_data_update(&source_path, &destination_path, options)?;
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
            staging::sync_metadata(&source_path, &destination_path)?;
        }
        Operation::Delete { destination_index } => {
            let destination_path = delete_operation_path(
                source,
                destination,
                destination_manifest.ok_or(SessionError::MissingDestinationManifest)?,
                destination_index,
            )?;
            staging::remove_file(&destination_path)?;
            if source_manifest.root == ManifestRoot::Directory {
                staging::prune_empty_parent_directories(&destination_path, &destination.path)?;
            }
        }
        Operation::Skip { .. } => {}
    }

    Ok(operation)
}

fn refine_operation(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    operation: Operation,
) -> Result<Operation, SessionError> {
    match operation {
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
                destination_manifest.ok_or(SessionError::MissingDestinationManifest)?,
                source_index,
                destination_index,
            )?;

            if files_match(&source_path, &destination_path)? {
                Ok(operation)
            } else {
                Ok(Operation::UpdateData {
                    source_index,
                    destination_index,
                })
            }
        }
        Operation::Create { .. } | Operation::UpdateData { .. } | Operation::Delete { .. } => {
            Ok(operation)
        }
    }
}

fn apply_data_update(
    source_path: &Path,
    destination_path: &Path,
    options: &Options,
) -> Result<(), SessionError> {
    match options.strategy {
        super::Strategy::Auto | super::Strategy::Whole => {
            staging::replace_file(source_path, destination_path)
        }
        super::Strategy::Cdc => apply_cdc_delta_update(source_path, destination_path, options),
    }
}

fn apply_cdc_delta_update(
    source_path: &Path,
    destination_path: &Path,
    options: &Options,
) -> Result<(), SessionError> {
    match options.chunker {
        super::Chunker::FastCdc => apply_fastcdc_delta_update(source_path, destination_path),
        super::Chunker::SeqCdc => Err(SessionError::UnsupportedChunker {
            strategy: options.strategy.to_string(),
            chunker: options.chunker.to_string(),
        }),
    }
}

fn apply_fastcdc_delta_update(
    source_path: &Path,
    destination_path: &Path,
) -> Result<(), SessionError> {
    let config = FastCdcConfig::default();

    let mut basis_signature_file =
        File::open(destination_path).map_err(|source| SessionError::ApplyIo {
            operation: "open destination basis file for FastCDC delta",
            path: destination_path.to_path_buf(),
            source,
        })?;
    let signatures = cdc::signatures_fastcdc(&mut basis_signature_file, config)?;

    let mut source_file = File::open(source_path).map_err(|source| SessionError::ApplyIo {
        operation: "open source file for FastCDC delta",
        path: source_path.to_path_buf(),
        source,
    })?;
    staging::replace_with_writer(source_path, destination_path, |writer| {
        let mut basis_apply_file =
            File::open(destination_path).map_err(|source| SessionError::ApplyIo {
                operation: "reopen destination basis file for FastCDC delta apply",
                path: destination_path.to_path_buf(),
                source,
            })?;

        cdc::apply_fastcdc(
            &mut source_file,
            &signatures,
            config,
            &mut basis_apply_file,
            writer,
        )?;
        Ok(())
    })?;

    Ok(())
}

fn with_operation_path(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    operation: Operation,
    mut on_path: impl FnMut(&Path),
) -> Result<(), SessionError> {
    match source_manifest.root {
        ManifestRoot::Directory => on_path(directory_operation_path(
            source_manifest,
            destination_manifest,
            operation,
        )?),
        ManifestRoot::File => {
            let target_path = resolve_file_target(source, destination)?;
            on_path(&target_path);
        }
    }

    Ok(())
}

fn directory_operation_path<'a>(
    source_manifest: &'a Manifest,
    destination_manifest: Option<&'a Manifest>,
    operation: Operation,
) -> Result<&'a Path, SessionError> {
    match operation {
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
        } => Ok(source_manifest.entries[source_index].path.as_path()),
        Operation::Delete { destination_index } => Ok(destination_manifest
            .ok_or(SessionError::MissingDestinationManifest)?
            .entries[destination_index]
            .path
            .as_path()),
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

fn files_match(source: &Path, destination: &Path) -> Result<bool, SessionError> {
    // This is the correctness guard for same-size matches. If metadata says two
    // files look identical, Oni still compares bytes before deciding that an
    // update can be skipped or reduced to metadata-only work.
    let mut source_file = File::open(source).map_err(|source_error| SessionError::VerifyIo {
        path: source.to_path_buf(),
        source: source_error,
    })?;
    let mut destination_file =
        File::open(destination).map_err(|source_error| SessionError::VerifyIo {
            path: destination.to_path_buf(),
            source: source_error,
        })?;
    let mut source_buffer = [0_u8; 8 * 1024];
    let mut destination_buffer = [0_u8; 8 * 1024];

    loop {
        let source_read = source_file
            .read(&mut source_buffer)
            .map_err(|source_error| SessionError::VerifyIo {
                path: source.to_path_buf(),
                source: source_error,
            })?;
        let destination_read =
            destination_file
                .read(&mut destination_buffer)
                .map_err(|source_error| SessionError::VerifyIo {
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
    use std::time::{Duration, UNIX_EPOCH};

    use crate::endpoint::temp_path;
    use crate::error::SessionError;
    use crate::plan::PlanOptions;
    use crate::session::{Chunker, Options, Request, Strategy};

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
        let mut operations = Vec::new();

        request
            .apply(|operation, path| operations.push((operation.to_string(), path.to_path_buf())))
            .unwrap();

        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].0, "create");
        assert_eq!(operations[0].1, destination);
        assert_eq!(fs::read(&destination).unwrap(), b"oni");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upgrades_same_size_changed_files_to_data_updates() {
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
        let mut operations = Vec::new();

        request
            .apply(|operation, path| operations.push((operation.to_string(), path.to_path_buf())))
            .unwrap();

        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].0, "update-data");
        assert_eq!(fs::read(&destination).unwrap(), b"aaa");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn applies_explicit_fastcdc_strategy_for_local_updates() {
        let root = temp_path("session-apply-fastcdc-strategy");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("alpha.txt");
        let destination = root.join("beta.txt");
        let destination_bytes = (0..320 * 1024)
            .map(|index| ((index * 31 + index / 97) % 251) as u8)
            .collect::<Vec<_>>();
        let mut source_bytes = destination_bytes[..128 * 1024].to_vec();
        source_bytes.extend(std::iter::repeat_n(b'!', 4 * 1024));
        source_bytes.extend_from_slice(&destination_bytes[128 * 1024..]);
        fs::write(&source, &source_bytes).unwrap();
        fs::write(&destination, &destination_bytes).unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options {
                strategy: Strategy::Cdc,
                chunker: Chunker::FastCdc,
                ..Options::default()
            },
        )
        .unwrap();
        let mut operations = Vec::new();

        request
            .apply(|operation, path| operations.push((operation.to_string(), path.to_path_buf())))
            .unwrap();

        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].0, "update-data");
        assert_eq!(fs::read(&destination).unwrap(), source_bytes);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unimplemented_cdc_chunkers_clearly() {
        let root = temp_path("session-apply-unsupported-cdc-chunker");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("alpha.txt");
        let destination = root.join("beta.txt");
        fs::write(&source, b"source").unwrap();
        fs::write(&destination, b"destin").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options {
                strategy: Strategy::Cdc,
                chunker: Chunker::SeqCdc,
                ..Options::default()
            },
        )
        .unwrap();

        let error = request.apply(|_, _| {}).unwrap_err();

        assert!(matches!(
            error,
            SessionError::UnsupportedChunker {
                strategy,
                chunker,
            } if strategy == "cdc" && chunker == "seqcdc"
        ));
        assert_eq!(fs::read(&destination).unwrap(), b"destin");

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
        let mut operations = Vec::new();

        request
            .apply(|operation, path| operations.push((operation.to_string(), path.to_path_buf())))
            .unwrap();

        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].0, "update-metadata");
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
                plan: PlanOptions {
                    delete_extraneous: true,
                },
                ..Options::default()
            },
        )
        .unwrap();
        let mut delete_count = 0;

        request
            .apply(|operation, _| {
                if matches!(operation, crate::plan::Operation::Delete { .. }) {
                    delete_count += 1;
                }
            })
            .unwrap();

        assert_eq!(delete_count, 1);
        assert!(!destination.join("remove.txt").exists());
        assert!(destination.join("keep.txt").exists());

        fs::remove_dir_all(source).unwrap();
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn removes_empty_destination_directories_after_deletes() {
        let source = temp_path("session-apply-prune-source");
        let destination = temp_path("session-apply-prune-destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(destination.join("nested")).unwrap();
        fs::write(destination.join("nested/remove.txt"), b"remove").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options {
                plan: PlanOptions {
                    delete_extraneous: true,
                },
                ..Options::default()
            },
        )
        .unwrap();
        let mut delete_count = 0;

        request
            .apply(|operation, _| {
                if matches!(operation, crate::plan::Operation::Delete { .. }) {
                    delete_count += 1;
                }
            })
            .unwrap();

        assert_eq!(delete_count, 1);
        assert!(destination.exists());
        assert!(!destination.join("nested").exists());

        fs::remove_dir_all(source).unwrap();
        fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn reports_directory_changes_with_relative_paths() {
        let source = temp_path("session-report-directory-source");
        let destination = temp_path("session-report-directory-destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(source.join("nested/new.txt"), b"new").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options::default(),
        )
        .unwrap();
        let mut operations = Vec::new();

        request
            .apply(|operation, path| operations.push((operation.to_string(), path.to_path_buf())))
            .unwrap();

        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].0, "create");
        assert_eq!(operations[0].1, std::path::PathBuf::from("nested/new.txt"));

        fs::remove_dir_all(source).unwrap();
        fs::remove_dir_all(destination).unwrap();
    }
}
