use std::fmt;
use std::path::PathBuf;

use crate::plan::Operation;

/// Dry-run output independent of the backend that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub mode: super::Mode,
    pub operations: Vec<Change>,
}

impl Preview {
    pub fn summary(&self) -> Summary {
        let mut summary = Summary::default();

        for operation in &self.operations {
            match operation.kind {
                ChangeKind::Create => summary.create += 1,
                ChangeKind::UpdateData => summary.update_data += 1,
                ChangeKind::UpdateMetadata => summary.update_metadata += 1,
                ChangeKind::Delete => summary.delete += 1,
                ChangeKind::Skip => summary.skip += 1,
            }
        }

        summary
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    pub path: PathBuf,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    Create,
    UpdateData,
    UpdateMetadata,
    Delete,
    Skip,
}

impl From<&Operation> for ChangeKind {
    fn from(value: &Operation) -> Self {
        match value {
            Operation::Create { .. } => Self::Create,
            Operation::UpdateData { .. } => Self::UpdateData,
            Operation::UpdateMetadata { .. } => Self::UpdateMetadata,
            Operation::Delete { .. } => Self::Delete,
            Operation::Skip { .. } => Self::Skip,
        }
    }
}

impl fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create => f.write_str("create"),
            Self::UpdateData => f.write_str("update-data"),
            Self::UpdateMetadata => f.write_str("update-metadata"),
            Self::Delete => f.write_str("delete"),
            Self::Skip => f.write_str("skip"),
        }
    }
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct Summary {
    pub create: usize,
    pub update_data: usize,
    pub update_metadata: usize,
    pub delete: usize,
    pub skip: usize,
}

impl Summary {
    pub fn update_total(&self) -> usize {
        self.update_data + self.update_metadata
    }
}
