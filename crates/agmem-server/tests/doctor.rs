//! `--doctor` end-to-end: fresh setup passes, report goes to stderr only.
//!
//! Every case runs `--embedder none`: loading the real model is a download,
//! and CI points `FASTEMBED_CACHE_DIR` somewhere unwritable precisely so an
//! accidental one fails loudly. The ONNX path has its own ignored test.

use std::process::Command;

#[test]
fn doctor_passes_on_fresh_setup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--doctor", "--db", "mem://", "--embedder", "none", "--data"])
        .arg(dir.path())
        .output()
        .expect("run agmem --doctor");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(out.stdout.is_empty(), "doctor must not touch stdout");
    assert!(stderr.contains("all checks passed"), "got: {stderr}");
    assert!(stderr.contains("none (test-only, no vectors)"), "got: {stderr}");
}

#[test]
fn doctor_names_the_tools_this_run_reworded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--doctor", "--db", "mem://", "--embedder", "none", "--data"])
        .arg(dir.path())
        .env("AGMEM_TOOL_DESC_RECALL", "Ask the store before you answer.")
        .output()
        .expect("run agmem --doctor");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(
        stderr.contains("tool descriptions    overridden: recall"),
        "an override is invisible from the outside unless the report says so: {stderr}"
    );
}

#[test]
fn an_override_naming_no_tool_stops_the_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--doctor", "--db", "mem://", "--embedder", "none", "--data"])
        .arg(dir.path())
        .env("AGMEM_TOOL_DESC_RECAL", "…")
        .output()
        .expect("run agmem --doctor");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "a typo must not be ignored");
    assert!(
        stderr.contains("AGMEM_TOOL_DESC_RECAL") && stderr.contains("recall"),
        "the refusal names the variable and the real tools: {stderr}"
    );
}

#[test]
fn doctor_fails_cleanly_on_bad_db_url() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args([
            "--doctor",
            "--db",
            "bogus://nowhere",
            "--embedder",
            "none",
            "--data",
        ])
        .arg(dir.path())
        .output()
        .expect("run agmem --doctor");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "must exit non-zero");
    assert!(out.stdout.is_empty(), "doctor must not touch stdout");
    assert!(stderr.contains("FAIL"), "got: {stderr}");
}
