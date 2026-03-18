use crate::engine::manifest::{Manifest, RelativePath};
use crate::error::PlanError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOperation {
    CreateOrUpdate(RelativePath),
    Delete(RelativePath),
    Skip(RelativePath),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncPlan {
    pub operations: Vec<FileOperation>,
}

pub trait Planner: Send + Sync {
    fn build_plan(&self, source: &Manifest, destination: &Manifest) -> Result<SyncPlan, PlanError>;
}

#[derive(Debug, Default)]
pub struct StubPlanner;

impl Planner for StubPlanner {
    fn build_plan(
        &self,
        _source: &Manifest,
        _destination: &Manifest,
    ) -> Result<SyncPlan, PlanError> {
        Err(PlanError::NotImplemented {
            feature: "sync planner",
        })
    }
}
