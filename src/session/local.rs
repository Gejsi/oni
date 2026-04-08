//! Local-only session backend for the current `v2` implementation.
//!
//! The file stays intentionally small:
//! - prepare manifests
//! - stream planned operations
//! - execute local filesystem changes
//! - translate operations into user-facing preview output
//!
//! Current local pipeline:
//!
//!   CLI request
//!       |
//!       v
//!   scan source manifest -------------------+
//!       |                                  |
//!       v                                  v
//!   resolve/scan destination manifest   stream metadata-only plan
//!       |                                  |
//!       +--------------+-------------------+
//!                      |
//!                      v
//!        verify equal-size matches when needed
//!                      |
//!                      v
//!          consume each operation immediately
//!            as preview output or real apply

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::SessionError;
use crate::fs as fs_ops;
use crate::manifest::{Manifest, ManifestRoot};
use crate::path::LocalEndpoint;
use crate::plan::{Operation, PlanOptions, for_each_operation};
use crate::strategy::cdc;

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
    let prepared = prepare(source, destination)?;
    let plan_options = PlanOptions {
        delete_extraneous: options.delete_extraneous,
    };
    build_changes(
        source,
        destination,
        &prepared.source_manifest,
        prepared.destination_manifest.as_ref(),
        options.checksum,
        plan_options,
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
    let prepared = prepare(source, destination)?;
    let plan_options = PlanOptions {
        delete_extraneous: options.delete_extraneous,
    };
    let changes = build_changes(
        source,
        destination,
        &prepared.source_manifest,
        prepared.destination_manifest.as_ref(),
        true,
        plan_options,
    )?;

    execute_plan(
        source,
        destination,
        &prepared.source_manifest,
        prepared.destination_manifest.as_ref(),
        plan_options,
        options,
    )?;

    Ok(changes)
}

struct PreparedPlan {
    source_manifest: Manifest,
    destination_manifest: Option<Manifest>,
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
struct VerificationStats {
    compared_files: usize,
    promoted_to_data: usize,
    compared_bytes: u64,
}

fn prepare(source: &LocalEndpoint, destination: &LocalEndpoint) -> Result<PreparedPlan, SessionError> {
    // The local backend always starts from the same metadata snapshot:
    //
    // 1. scan source
    // 2. scan/resolve destination
    //
    // Planning is now streamed later at the point of preview/execution instead
    // of being buffered here as a second owned `Vec<Operation>`.
    let source_manifest = Manifest::scan(&source.path)?;
    let destination_manifest = scan_destination_manifest(source, destination, &source_manifest)?;

    Ok(PreparedPlan {
        source_manifest,
        destination_manifest,
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
    plan_options: PlanOptions,
    options: &Options,
) -> Result<(), SessionError> {
    // The local executor keeps one simple rule: consume planned operations in
    // order, and make each file change crash-safe via `fs.rs`.
    let mut execution_error: Option<SessionError> = None;

    for_each_verified_operation(
        source,
        destination,
        source_manifest,
        destination_manifest,
        true,
        plan_options,
        |operation| {
            if execution_error.is_some() {
                return;
            }

            if let Err(error) = execute_operation(
                source,
                destination,
                source_manifest,
                destination_manifest,
                operation,
                options,
            ) {
                execution_error = Some(error);
            }
        },
    )?;

    if let Some(error) = execution_error {
        Err(error)
    } else {
        Ok(())
    }
}

fn execute_operation(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    operation: Operation,
    options: &Options,
) -> Result<(), SessionError> {
    match operation {
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
            if source_manifest.root == ManifestRoot::Directory {
                fs_ops::prune_empty_parent_directories(&destination_path, &destination.path)?;
            }
        }
        Operation::Skip { .. } => {}
    }

    Ok(())
}

fn apply_data_update(
    source_path: &Path,
    destination_path: &Path,
    options: &Options,
) -> Result<(), SessionError> {
    // Strategy dispatch stays here instead of in `fs.rs` so the filesystem
    // helpers remain dumb and reusable.
    //
    // `Auto` is intentionally conservative today:
    //
    //   Auto -> Whole
    //        -> Cdc only when the user asks explicitly
    //
    // The real heuristic selector belongs here later.
    match options.strategy {
        super::Strategy::Auto | super::Strategy::Whole => {
            fs_ops::replace_file(source_path, destination_path)
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
    // Current local FastCDC flow:
    //
    //   destination --chunk_read--> chunk bytes --hash--> basis signatures
    //   source      --chunk_read--> chunk bytes --match--> reference/literal sink
    //   sink + destination basis ------------------------> temp file -> rename
    //
    // The important simplification here is that the source pass now streams
    // directly into the temp-file writer. Only the basis-signature table stays
    // buffered for the file.
    let config = cdc::FastCdcConfig::default();

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
    fs_ops::replace_with_writer(source_path, destination_path, |writer| {
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

fn build_changes(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    verify_contents: bool,
    plan_options: PlanOptions,
) -> Result<Vec<Change>, SessionError> {
    // Preview/apply output uses logical destination paths, not internal manifest
    // indices, so the CLI can print one stable path per operation.
    let mut changes = Vec::new();

    match source_manifest.root {
        ManifestRoot::Directory => {
            for_each_verified_operation(
                source,
                destination,
                source_manifest,
                destination_manifest,
                verify_contents,
                plan_options,
                |operation| {
                    changes.push(Change {
                        kind: ChangeKind::from(&operation),
                        path: directory_change_path(
                            source_manifest,
                            destination_manifest,
                            &operation,
                        )
                        .expect("directory change path should be derivable"),
                    });
                },
            )?;
        }
        ManifestRoot::File => {
            let target_path = resolve_file_target(source, destination)?;
            for_each_verified_operation(
                source,
                destination,
                source_manifest,
                destination_manifest,
                verify_contents,
                plan_options,
                |operation| {
                    changes.push(Change {
                        kind: ChangeKind::from(&operation),
                        path: target_path.clone(),
                    });
                },
            )?;
        }
    }

    Ok(changes)
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

fn for_each_verified_operation(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    verify_contents: bool,
    plan_options: PlanOptions,
    mut on_operation: impl FnMut(Operation),
) -> Result<VerificationStats, SessionError> {
    let mut callback_error = None;
    let mut stats = VerificationStats::default();

    // Equal-size matches take this refinement path:
    //
    //   planner metadata guess
    //       Skip / UpdateMetadata
    //               |
    //               v
    //        streaming byte compare
    //         |                 |
    //         v                 v
    //      contents same    contents differ
    //         |                 |
    //         v                 v
    //   keep original op   upgrade to UpdateData
    //
    // The planner stays pure and metadata-based. `--checksum` is intentionally
    // a session-level opt-in because it performs real I/O and should not be
    // the planner's default cost.
    for_each_operation(
        source_manifest,
        destination_manifest,
        plan_options,
        |operation| {
            if callback_error.is_some() {
                return Ok(());
            }

            match verify_operation(
                source,
                destination,
                source_manifest,
                destination_manifest,
                operation,
                verify_contents,
                &mut stats,
            ) {
                Ok(operation) => {
                    on_operation(operation);
                    Ok(())
                }
                Err(error) => {
                    callback_error = Some(error);
                    Ok(())
                }
            }
        },
    )
    .map_err(SessionError::from)?;

    if let Some(error) = callback_error {
        return Err(error);
    }

    Ok(stats)
}

fn verify_operation(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    operation: Operation,
    verify_contents: bool,
    stats: &mut VerificationStats,
) -> Result<Operation, SessionError> {
    match operation {
        Operation::Skip {
            source_index,
            destination_index,
        }
        | Operation::UpdateMetadata {
            source_index,
            destination_index,
        } if verify_contents => {
            let (source_path, destination_path) = matched_operation_paths(
                source,
                destination,
                source_manifest,
                destination_manifest.ok_or(SessionError::MissingDestinationManifest)?,
                source_index,
                destination_index,
            )?;
            let compared_bytes = source_manifest.entries[source_index].metadata.len;
            let contents_match = files_match(&source_path, &destination_path)?;
            stats.compared_files += 1;
            stats.compared_bytes += compared_bytes;

            if contents_match {
                Ok(operation)
            } else {
                stats.promoted_to_data += 1;
                Ok(Operation::UpdateData {
                    source_index,
                    destination_index,
                })
            }
        }
        _ => Ok(operation),
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
    // Directory roots compare relative paths inside each root.
    // File roots compare exactly one logical source/destination item.
    match source_manifest.root {
        ManifestRoot::Directory => Ok((
            source.path.join(&source_manifest.entries[source_index].path),
            destination
                .path
                .join(&destination_manifest.entries[destination_index].path),
        )),
        ManifestRoot::File => Ok((source.path.clone(), resolve_file_target(source, destination)?)),
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
    // This is a streaming byte comparison, not a hashing pass. It is used only
    // when the session explicitly needs content verification for equal-size
    // matches.
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

    use crate::error::SessionError;
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

        let applied = request.apply().unwrap();

        assert_eq!(applied.operations.len(), 1);
        assert_eq!(applied.operations[0].kind.to_string(), "update-data");
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

        let error = request.apply().unwrap_err();

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
                delete_extraneous: true,
                ..Options::default()
            },
        )
        .unwrap();

        let applied = request.apply().unwrap();

        assert_eq!(applied.summary().delete, 1);
        assert!(destination.exists());
        assert!(!destination.join("nested").exists());

        fs::remove_dir_all(source).unwrap();
        fs::remove_dir_all(destination).unwrap();
    }
}
