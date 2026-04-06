//! Local-only session backend for the current `v2` implementation.
//!
//! The file stays intentionally small:
//! - prepare manifests and a plan
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
//!   resolve/scan destination manifest   build metadata-only plan
//!       |                                  |
//!       +--------------+-------------------+
//!                      |
//!                      v
//!        verify equal-size matches when needed
//!                      |
//!                      v
//!              execute file operations
//!                      |
//!                      v
//!               build user-facing changes

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::error::SessionError;
use crate::fs as fs_ops;
use crate::manifest::{Manifest, ManifestRoot};
use crate::path::LocalEndpoint;
use crate::plan::{Operation, Plan, PlanOptions};
use crate::strategy::{cdc, fixed};

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
        options,
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

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
struct VerificationStats {
    compared_files: usize,
    promoted_to_data: usize,
    compared_bytes: u64,
    elapsed: Duration,
}

static LOCAL_PROFILE_ENABLED: OnceLock<bool> = OnceLock::new();

fn profile_enabled() -> bool {
    *LOCAL_PROFILE_ENABLED.get_or_init(|| {
        std::env::var("ONI_PROFILE_LOCAL")
            .map(|value| !value.is_empty() && value != "0")
            .unwrap_or(false)
    })
}

fn profile_event(event: &str, duration: Duration, fields: &[(&str, String)]) {
    if !profile_enabled() {
        return;
    }

    let mut line = format!(
        "profile-local\tevent={event}\tseconds={:.6}",
        duration.as_secs_f64()
    );

    for (key, value) in fields {
        line.push('\t');
        line.push_str(key);
        line.push('=');
        line.push_str(value);
    }

    eprintln!("{line}");
}

fn prepare(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    options: &Options,
    verify_contents: bool,
) -> Result<PreparedPlan, SessionError> {
    // The local backend always follows the same sequence:
    //
    //   metadata scan -> metadata plan -> optional byte verification
    //
    // 1. scan source
    // 2. scan/resolve destination
    // 3. build a cheap metadata plan
    // 4. optionally upgrade equal-size matches after content verification
    let scan_source_start = Instant::now();
    let source_manifest = Manifest::scan(&source.path)?;
    profile_event(
        "prepare.scan-source",
        scan_source_start.elapsed(),
        &[
            ("path", source.path.display().to_string()),
            ("entries", source_manifest.entries.len().to_string()),
            ("root", source_manifest.root.to_string()),
        ],
    );

    let scan_destination_start = Instant::now();
    let destination_manifest = scan_destination_manifest(source, destination, &source_manifest)?;
    profile_event(
        "prepare.scan-destination",
        scan_destination_start.elapsed(),
        &[
            ("path", destination.path.display().to_string()),
            ("exists", destination_manifest.is_some().to_string()),
        ],
    );

    let plan_start = Instant::now();
    let mut plan = Plan::build(
        &source_manifest,
        destination_manifest.as_ref(),
        PlanOptions {
            delete_extraneous: options.delete_extraneous,
        },
    )?;
    profile_event(
        "prepare.plan-build",
        plan_start.elapsed(),
        &[("operations", plan.operations.len().to_string())],
    );

    let verification_stats = verify_matched_operations(
        source,
        destination,
        &source_manifest,
        destination_manifest.as_ref(),
        &mut plan,
        verify_contents,
    )?;
    profile_event(
        "prepare.verify-matched",
        verification_stats.elapsed,
        &[
            (
                "compared-files",
                verification_stats.compared_files.to_string(),
            ),
            (
                "compared-bytes",
                verification_stats.compared_bytes.to_string(),
            ),
            (
                "promoted-to-update-data",
                verification_stats.promoted_to_data.to_string(),
            ),
        ],
    );

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
    options: &Options,
) -> Result<(), SessionError> {
    // The local executor keeps one simple rule: planned file operations run in
    // order, and each individual file change is crash-safe because `fs.rs`
    // always writes through a sibling temp file before rename.
    let execute_start = Instant::now();

    for operation in &plan.operations {
        match *operation {
            Operation::Create { source_index } => {
                let operation_start = Instant::now();
                let (source_path, destination_path) =
                    create_operation_paths(source, destination, source_manifest, source_index)?;
                fs_ops::replace_file(&source_path, &destination_path)?;
                profile_event(
                    "execute.create",
                    operation_start.elapsed(),
                    &[("path", destination_path.display().to_string())],
                );
            }
            Operation::UpdateData {
                source_index,
                destination_index,
            } => {
                let operation_start = Instant::now();
                let (source_path, destination_path) = matched_operation_paths(
                    source,
                    destination,
                    source_manifest,
                    destination_manifest.ok_or(SessionError::MissingDestinationManifest)?,
                    source_index,
                    destination_index,
                )?;
                apply_data_update(&source_path, &destination_path, options)?;
                profile_event(
                    "execute.update-data",
                    operation_start.elapsed(),
                    &[
                        ("path", destination_path.display().to_string()),
                        ("strategy", options.strategy.to_string()),
                    ],
                );
            }
            Operation::UpdateMetadata {
                source_index,
                destination_index,
            } => {
                let operation_start = Instant::now();
                let (source_path, destination_path) = matched_operation_paths(
                    source,
                    destination,
                    source_manifest,
                    destination_manifest.ok_or(SessionError::MissingDestinationManifest)?,
                    source_index,
                    destination_index,
                )?;
                fs_ops::sync_metadata(&source_path, &destination_path)?;
                profile_event(
                    "execute.update-metadata",
                    operation_start.elapsed(),
                    &[("path", destination_path.display().to_string())],
                );
            }
            Operation::Delete { destination_index } => {
                let operation_start = Instant::now();
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
                profile_event(
                    "execute.delete",
                    operation_start.elapsed(),
                    &[("path", destination_path.display().to_string())],
                );
            }
            Operation::Skip {
                source_index: _,
                destination_index: _,
            } => {}
        }
    }

    profile_event(
        "execute.plan-total",
        execute_start.elapsed(),
        &[("operations", plan.operations.len().to_string())],
    );

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
    //        -> Fixed/Cdc only when the user asks explicitly
    //
    // The real heuristic selector belongs here later.
    match options.strategy {
        super::Strategy::Fixed => apply_fixed_delta_update(source_path, destination_path),
        super::Strategy::Auto | super::Strategy::Whole => {
            fs_ops::replace_file(source_path, destination_path)
        }
        super::Strategy::Cdc => apply_cdc_delta_update(source_path, destination_path, options),
    }
}

fn apply_fixed_delta_update(
    source_path: &Path,
    destination_path: &Path,
) -> Result<(), SessionError> {
    // The destination file is both:
    // - the basis used to compute signatures and copy references
    // - the path that will be atomically replaced once the recipe is applied
    //
    // Local fixed-delta flow:
    //
    //   destination --signatures--> basis table
    //   source      --rolling scan-> recipe
    //   recipe + destination basis -> temp file -> atomic rename
    //
    // This means local delta currently pays extra passes that whole-file copy
    // does not pay: signature scan, source delta scan, and basis rereads during
    // recipe apply.
    let source_len = std::fs::metadata(source_path)
        .map_err(|source| SessionError::ApplyIo {
            operation: "read source metadata for fixed delta",
            path: source_path.to_path_buf(),
            source,
        })?
        .len();
    let destination_len = std::fs::metadata(destination_path)
        .map_err(|source| SessionError::ApplyIo {
            operation: "read destination metadata for fixed delta",
            path: destination_path.to_path_buf(),
            source,
        })?
        .len();
    let block_size = fixed::choose_block_size(source_len, destination_len);

    let signatures_start = Instant::now();
    let mut basis_signature_file =
        File::open(destination_path).map_err(|source| SessionError::ApplyIo {
            operation: "open destination basis file for fixed delta",
            path: destination_path.to_path_buf(),
            source,
        })?;
    let signatures = fixed::signatures(&mut basis_signature_file, block_size)?;
    profile_event(
        "fixed.signatures",
        signatures_start.elapsed(),
        &[
            ("path", destination_path.display().to_string()),
            ("block-size", block_size.to_string()),
            ("basis-bytes", destination_len.to_string()),
            ("blocks", signatures.blocks().len().to_string()),
        ],
    );

    let recipe_start = Instant::now();
    let mut source_file = File::open(source_path).map_err(|source| SessionError::ApplyIo {
        operation: "open source file for fixed delta",
        path: source_path.to_path_buf(),
        source,
    })?;
    let recipe = fixed::delta(&mut source_file, &signatures)?;
    profile_event(
        "fixed.recipe",
        recipe_start.elapsed(),
        &[
            ("path", source_path.display().to_string()),
            ("source-bytes", source_len.to_string()),
            ("chunks", recipe.chunks.len().to_string()),
        ],
    );

    let apply_start = Instant::now();
    fs_ops::replace_with_writer(source_path, destination_path, |writer| {
        let mut basis_apply_file =
            File::open(destination_path).map_err(|source| SessionError::ApplyIo {
                operation: "reopen destination basis file for fixed delta apply",
                path: destination_path.to_path_buf(),
                source,
            })?;

        fixed::apply(&recipe, &mut basis_apply_file, writer)?;
        Ok(())
    })?;
    profile_event(
        "fixed.apply",
        apply_start.elapsed(),
        &[
            ("path", destination_path.display().to_string()),
            ("chunks", recipe.chunks.len().to_string()),
        ],
    );

    Ok(())
}

fn apply_cdc_delta_update(
    source_path: &Path,
    destination_path: &Path,
    options: &Options,
) -> Result<(), SessionError> {
    match options.chunker {
        super::Chunker::FastCdc => apply_fastcdc_delta_update(source_path, destination_path),
        super::Chunker::Fixed | super::Chunker::SeqCdc => Err(SessionError::UnsupportedChunker {
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
    //   source      --chunk_read--> chunk bytes --hash--> recipe
    //   recipe + destination basis ----------------------> temp file -> rename
    //
    // The chunker boundary is now Oni-owned and callback-based, but the local
    // CDC path still buffers the final recipe before apply.
    let config = cdc::FastCdcConfig::default();

    let signatures_start = Instant::now();
    let mut basis_signature_file =
        File::open(destination_path).map_err(|source| SessionError::ApplyIo {
            operation: "open destination basis file for FastCDC delta",
            path: destination_path.to_path_buf(),
            source,
        })?;
    let signatures = cdc::signatures_fastcdc(&mut basis_signature_file, config)?;
    profile_event(
        "cdc.signatures",
        signatures_start.elapsed(),
        &[("path", destination_path.display().to_string())],
    );

    let recipe_start = Instant::now();
    let mut source_file = File::open(source_path).map_err(|source| SessionError::ApplyIo {
        operation: "open source file for FastCDC delta",
        path: source_path.to_path_buf(),
        source,
    })?;
    let recipe = cdc::delta_fastcdc(&mut source_file, &signatures, config)?;
    profile_event(
        "cdc.recipe",
        recipe_start.elapsed(),
        &[
            ("path", source_path.display().to_string()),
            ("chunks", recipe.chunks.len().to_string()),
        ],
    );

    let apply_start = Instant::now();
    fs_ops::replace_with_writer(source_path, destination_path, |writer| {
        let mut basis_apply_file =
            File::open(destination_path).map_err(|source| SessionError::ApplyIo {
                operation: "reopen destination basis file for FastCDC delta apply",
                path: destination_path.to_path_buf(),
                source,
            })?;

        cdc::apply(&recipe, &mut basis_apply_file, writer)?;
        Ok(())
    })?;
    profile_event(
        "cdc.apply",
        apply_start.elapsed(),
        &[
            ("path", destination_path.display().to_string()),
            ("chunks", recipe.chunks.len().to_string()),
        ],
    );

    Ok(())
}

fn build_changes(
    source: &LocalEndpoint,
    destination: &LocalEndpoint,
    source_manifest: &Manifest,
    destination_manifest: Option<&Manifest>,
    plan: &Plan,
) -> Result<Vec<Change>, SessionError> {
    // Preview output uses logical destination paths, not internal manifest
    // indices, so the CLI can print one stable path per operation.
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
) -> Result<VerificationStats, SessionError> {
    if !verify_contents {
        return Ok(VerificationStats::default());
    }

    let Some(destination_manifest) = destination_manifest else {
        return Ok(VerificationStats::default());
    };

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
                let compare_start = Instant::now();
                let compared_bytes = source_manifest.entries[source_index].metadata.len;
                let contents_match = files_match(&source_path, &destination_path)?;
                let elapsed = compare_start.elapsed();
                stats.compared_files += 1;
                stats.compared_bytes += compared_bytes;
                stats.elapsed += elapsed;
                profile_event(
                    "verify.compare-file",
                    elapsed,
                    &[
                        ("source", source_path.display().to_string()),
                        ("destination", destination_path.display().to_string()),
                        ("bytes", compared_bytes.to_string()),
                        (
                            "result",
                            if contents_match {
                                "same".to_string()
                            } else {
                                "different".to_string()
                            },
                        ),
                    ],
                );

                if contents_match {
                    None
                } else {
                    stats.promoted_to_data += 1;
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

    Ok(stats)
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
    fn applies_explicit_fixed_strategy_for_local_updates() {
        let root = temp_path("session-apply-fixed-strategy");
        fs::create_dir_all(&root).unwrap();

        let source = root.join("alpha.txt");
        let destination = root.join("beta.txt");
        fs::write(&source, b"zzabcd1234wxyzyy").unwrap();
        fs::write(&destination, b"abcd1234wxyz").unwrap();

        let request = Request::from_args(
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
            Options {
                strategy: Strategy::Fixed,
                ..Options::default()
            },
        )
        .unwrap();

        let applied = request.apply().unwrap();

        assert_eq!(applied.operations.len(), 1);
        assert_eq!(applied.operations[0].kind.to_string(), "update-data");
        assert_eq!(fs::read(&destination).unwrap(), b"zzabcd1234wxyzyy");

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
