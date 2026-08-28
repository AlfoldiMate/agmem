//! `--doctor` end-to-end: fresh setup passes, report goes to stderr only.

use std::process::Command;

#[test]
fn doctor_passes_on_fresh_setup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--doctor", "--db", "mem://", "--data"])
        .arg(dir.path())
        .output()
        .expect("run agmem --doctor");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(out.stdout.is_empty(), "doctor must not touch stdout");
    assert!(stderr.contains("all checks passed"), "got: {stderr}");
}

#[test]
fn doctor_fails_cleanly_on_bad_db_url() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--doctor", "--db", "bogus://nowhere", "--data"])
        .arg(dir.path())
        .output()
        .expect("run agmem --doctor");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "must exit non-zero");
    assert!(out.stdout.is_empty(), "doctor must not touch stdout");
    assert!(stderr.contains("FAIL"), "got: {stderr}");
}
