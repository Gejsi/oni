use crate::error::PlanError;
use crate::manifest::{Manifest, ManifestRoot};

#[derive(Debug, PartialEq, Eq, Default)]
pub struct PlanOptions {
    pub delete_extraneous: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Operation {
    Create {
        source_index: usize,
    },
    Update {
        source_index: usize,
        destination_index: usize,
    },
    Delete {
        destination_index: usize,
    },
    Skip {
        source_index: usize,
        destination_index: usize,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub operations: Vec<Operation>,
}

impl Plan {
    pub fn build(
        source: &Manifest,
        destination: Option<&Manifest>,
        options: PlanOptions,
    ) -> Result<Self, PlanError> {
        // No destination means every source entry must be created
        let Some(destination) = destination else {
            return Ok(Self {
                operations: source
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(source_index, _)| Operation::Create { source_index })
                    .collect(),
            });
        };

        // Reject `file -> directory` and `directory -> file`
        if matches!(
            (&source.root, &destination.root),
            (ManifestRoot::File, ManifestRoot::Directory)
                | (ManifestRoot::Directory, ManifestRoot::File)
        ) {
            return Err(PlanError::RootKindMismatch {
                source_root: source.root.clone(),
                destination_root: destination.root.clone(),
            });
        }

        let mut operations = Vec::with_capacity(source.entries.len() + destination.entries.len());
        let mut source_index = 0;
        let mut destination_index = 0;

        while source_index < source.entries.len() && destination_index < destination.entries.len() {
            let source_entry = &source.entries[source_index];
            let destination_entry = &destination.entries[destination_index];

            // Both manifests are sorted by relative path
            // This lets the planner do one linear merge-style walk
            match source_entry.path.cmp(&destination_entry.path) {
                std::cmp::Ordering::Less => {
                    operations.push(Operation::Create { source_index });
                    source_index += 1;
                }
                std::cmp::Ordering::Greater => {
                    if options.delete_extraneous {
                        operations.push(Operation::Delete { destination_index });
                    }
                    destination_index += 1;
                }
                std::cmp::Ordering::Equal => {
                    if source_entry.metadata == destination_entry.metadata
                        && source_entry.kind == destination_entry.kind
                    {
                        operations.push(Operation::Skip {
                            source_index,
                            destination_index,
                        });
                    } else {
                        operations.push(Operation::Update {
                            source_index,
                            destination_index,
                        });
                    }
                    source_index += 1;
                    destination_index += 1;
                }
            }
        }

        while source_index < source.entries.len() {
            operations.push(Operation::Create { source_index });
            source_index += 1;
        }

        if options.delete_extraneous {
            while destination_index < destination.entries.len() {
                operations.push(Operation::Delete { destination_index });
                destination_index += 1;
            }
        }

        Ok(Self { operations })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Operation, Plan, PlanOptions};
    use crate::error::PlanError;
    use crate::manifest::{EntryKind, EntryMetadata, Manifest, ManifestEntry, ManifestRoot};

    #[test]
    fn plans_create_for_missing_destination() {
        let source = directory_manifest(&[("notes/today.txt", 12), ("photos/cover.jpg", 32)]);

        let plan = Plan::build(&source, None, PlanOptions::default()).unwrap();

        assert_eq!(
            plan.operations,
            vec![
                Operation::Create { source_index: 0 },
                Operation::Create { source_index: 1 },
            ]
        );
    }

    #[test]
    fn plans_skip_for_matching_file_roots() {
        let source = file_manifest("todo.txt", 3, 10);
        let destination = file_manifest("todo.txt", 3, 10);

        let plan = Plan::build(&source, Some(&destination), PlanOptions::default()).unwrap();

        assert_eq!(
            plan.operations,
            vec![Operation::Skip {
                source_index: 0,
                destination_index: 0,
            }]
        );
    }

    #[test]
    fn plans_create_for_missing_file_root_destination() {
        let source = file_manifest("todo.txt", 3, 10);

        let plan = Plan::build(&source, None, PlanOptions::default()).unwrap();

        assert_eq!(plan.operations, vec![Operation::Create { source_index: 0 }]);
    }

    #[test]
    fn plans_update_for_changed_file_roots() {
        let source = file_manifest("todo.txt", 4, 11);
        let destination = file_manifest("todo.txt", 3, 10);

        let plan = Plan::build(&source, Some(&destination), PlanOptions::default()).unwrap();

        assert_eq!(
            plan.operations,
            vec![Operation::Update {
                source_index: 0,
                destination_index: 0,
            }]
        );
    }

    #[test]
    fn plans_skip_for_matching_directories() {
        let source = directory_manifest(&[("a.txt", 1), ("b.txt", 2), ("c.txt", 3)]);
        let destination = directory_manifest(&[("a.txt", 1), ("b.txt", 2), ("c.txt", 3)]);

        let plan = Plan::build(&source, Some(&destination), PlanOptions::default()).unwrap();

        assert_eq!(
            plan.operations,
            vec![
                Operation::Skip {
                    source_index: 0,
                    destination_index: 0,
                },
                Operation::Skip {
                    source_index: 1,
                    destination_index: 1,
                },
                Operation::Skip {
                    source_index: 2,
                    destination_index: 2,
                },
            ]
        );
    }

    #[test]
    fn plans_update_when_only_metadata_changes() {
        let source = directory_manifest_with_mtime(&[("a.txt", 1, 11)]);
        let destination = directory_manifest_with_mtime(&[("a.txt", 1, 10)]);

        let plan = Plan::build(&source, Some(&destination), PlanOptions::default()).unwrap();

        assert_eq!(
            plan.operations,
            vec![Operation::Update {
                source_index: 0,
                destination_index: 0,
            }]
        );
    }

    #[test]
    fn plans_mixed_directory_changes() {
        let source = directory_manifest(&[("a.txt", 1), ("c.txt", 3), ("d.txt", 4)]);
        let destination = directory_manifest(&[("a.txt", 1), ("b.txt", 2), ("d.txt", 4)]);

        let plan = Plan::build(
            &source,
            Some(&destination),
            PlanOptions {
                delete_extraneous: true,
            },
        )
        .unwrap();

        assert_eq!(
            plan.operations,
            vec![
                Operation::Skip {
                    source_index: 0,
                    destination_index: 0,
                },
                Operation::Delete {
                    destination_index: 1,
                },
                Operation::Create { source_index: 1 },
                Operation::Skip {
                    source_index: 2,
                    destination_index: 2,
                },
            ]
        );
    }

    #[test]
    fn plans_source_only_tail_entries() {
        let source = directory_manifest(&[("a.txt", 1), ("b.txt", 2), ("c.txt", 3)]);
        let destination = directory_manifest(&[("a.txt", 1)]);

        let plan = Plan::build(&source, Some(&destination), PlanOptions::default()).unwrap();

        assert_eq!(
            plan.operations,
            vec![
                Operation::Skip {
                    source_index: 0,
                    destination_index: 0,
                },
                Operation::Create { source_index: 1 },
                Operation::Create { source_index: 2 },
            ]
        );
    }

    #[test]
    fn ignores_extraneous_destination_entries_when_delete_is_disabled() {
        let source = directory_manifest(&[("a.txt", 1)]);
        let destination = directory_manifest(&[("a.txt", 1), ("b.txt", 2)]);

        let plan = Plan::build(&source, Some(&destination), PlanOptions::default()).unwrap();

        assert_eq!(
            plan.operations,
            vec![Operation::Skip {
                source_index: 0,
                destination_index: 0,
            }]
        );
    }

    #[test]
    fn plans_destination_only_tail_entries_when_delete_is_enabled() {
        let source = directory_manifest(&[("a.txt", 1)]);
        let destination = directory_manifest(&[("a.txt", 1), ("b.txt", 2), ("c.txt", 3)]);

        let plan = Plan::build(
            &source,
            Some(&destination),
            PlanOptions {
                delete_extraneous: true,
            },
        )
        .unwrap();

        assert_eq!(
            plan.operations,
            vec![
                Operation::Skip {
                    source_index: 0,
                    destination_index: 0,
                },
                Operation::Delete {
                    destination_index: 1,
                },
                Operation::Delete {
                    destination_index: 2,
                },
            ]
        );
    }

    #[test]
    fn rejects_file_to_directory_plans() {
        let source = file_manifest("todo.txt", 3, 10);
        let destination = directory_manifest(&[("todo.txt", 3)]);

        let error = Plan::build(&source, Some(&destination), PlanOptions::default()).unwrap_err();

        assert_eq!(
            error,
            PlanError::RootKindMismatch {
                source_root: ManifestRoot::File,
                destination_root: ManifestRoot::Directory,
            }
        );
    }

    #[test]
    fn rejects_directory_to_file_plans() {
        let source = directory_manifest(&[("todo.txt", 3)]);
        let destination = file_manifest("todo.txt", 3, 10);

        let error = Plan::build(&source, Some(&destination), PlanOptions::default()).unwrap_err();

        assert_eq!(
            error,
            PlanError::RootKindMismatch {
                source_root: ManifestRoot::Directory,
                destination_root: ManifestRoot::File,
            }
        );
    }

    #[test]
    fn allows_empty_source_directories() {
        let source = Manifest {
            root: ManifestRoot::Directory,
            entries: Vec::new(),
        };

        let plan = Plan::build(&source, None, PlanOptions::default()).unwrap();

        assert!(plan.operations.is_empty());
    }

    fn file_manifest(path: &str, len: u64, modified_unix_secs: u64) -> Manifest {
        Manifest {
            root: ManifestRoot::File,
            entries: vec![ManifestEntry {
                path: PathBuf::from(path),
                kind: EntryKind::File,
                metadata: EntryMetadata {
                    len,
                    modified_unix_secs: Some(modified_unix_secs),
                },
            }],
        }
    }

    fn directory_manifest(entries: &[(&str, u64)]) -> Manifest {
        directory_manifest_with_mtime(
            &entries
                .iter()
                .map(|(path, len)| (*path, *len, *len))
                .collect::<Vec<_>>(),
        )
    }

    fn directory_manifest_with_mtime(entries: &[(&str, u64, u64)]) -> Manifest {
        Manifest {
            root: ManifestRoot::Directory,
            entries: entries
                .iter()
                .map(|(path, len, modified_unix_secs)| ManifestEntry {
                    path: PathBuf::from(path),
                    kind: EntryKind::File,
                    metadata: EntryMetadata {
                        len: *len,
                        modified_unix_secs: Some(*modified_unix_secs),
                    },
                })
                .collect(),
        }
    }
}
