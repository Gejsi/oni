use std::path::Path;

use crate::engine::planner::SyncPlan;
use crate::error::ApplyError;

pub trait Applier: Send + Sync {
    fn apply(&self, destination_root: &Path, plan: &SyncPlan) -> Result<(), ApplyError>;
}

#[derive(Debug, Default)]
pub struct StubApplier;

impl Applier for StubApplier {
    fn apply(&self, _destination_root: &Path, _plan: &SyncPlan) -> Result<(), ApplyError> {
        Err(ApplyError::NotImplemented {
            feature: "plan application",
        })
    }
}
