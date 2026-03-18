use std::path::{Path, PathBuf};

use crate::error::ApplyError;

pub fn validate_destination(root: &Path, relative_path: &Path) -> Result<PathBuf, ApplyError> {
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ApplyError::PathEscape {
            path: relative_path.to_path_buf(),
        });
    }

    Ok(root.join(relative_path))
}
