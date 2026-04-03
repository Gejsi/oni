use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// Tests create real trees on disk
// The label keeps leftover paths readable when a case fails
pub fn temp_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    std::env::temp_dir().join(format!("oni-{label}-{unique}"))
}
