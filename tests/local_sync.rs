//! Integration tests for the user-facing local sync CLI.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();

    std::env::temp_dir().join(format!("oni-integration-{label}-{unique}"))
}

#[test]
fn copies_a_single_file_through_the_cli() {
    let root = temp_path("single-file");
    fs::create_dir_all(&root).unwrap();

    let source = root.join("alpha.txt");
    let destination = root.join("beta.txt");
    fs::write(&source, b"oni").unwrap();

    let status = Command::new(env!("CARGO_BIN_EXE_oni"))
        .arg(&source)
        .arg(&destination)
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(fs::read(&destination).unwrap(), b"oni");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn syncs_directory_updates_and_deletes_through_the_cli() {
    let source = temp_path("directory-source");
    let destination = temp_path("directory-destination");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::create_dir_all(destination.join("nested")).unwrap();

    fs::write(source.join("nested/keep.txt"), b"keep").unwrap();
    fs::write(source.join("nested/change.txt"), b"aaa").unwrap();
    fs::write(destination.join("nested/keep.txt"), b"keep").unwrap();
    fs::write(destination.join("nested/change.txt"), b"bbb").unwrap();
    fs::write(destination.join("nested/remove.txt"), b"remove").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_oni"))
        .arg("--delete")
        .arg("--stats")
        .arg(&source)
        .arg(&destination)
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        fs::read(destination.join("nested/change.txt")).unwrap(),
        b"aaa"
    );
    assert_eq!(
        fs::read(destination.join("nested/keep.txt")).unwrap(),
        b"keep"
    );
    assert!(!destination.join("nested/remove.txt").exists());

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("summary\tupdate-data=1"));
    assert!(stdout.contains("summary\tdelete=1"));

    fs::remove_dir_all(source).unwrap();
    fs::remove_dir_all(destination).unwrap();
}
