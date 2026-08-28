//! stdout is the MCP wire: startup and logging must never write to it.
//!
//! Two paths reach a serving process now, so both are checked. The
//! in-process one logs from the same process that owns stdout; the shared one
//! pumps another process's bytes through it, and its own log lines must not
//! join them.

use std::process::Command;

#[test]
fn in_process_startup_and_logging_write_nothing_to_stdout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--no-daemon", "--data"])
        .arg(dir.path())
        .env("AGMEM_LOG", "trace")
        .output()
        .expect("run agmem");

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "stdout must stay empty, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("agmem starting"),
        "expected the startup log line on stderr"
    );
    assert!(
        stderr.contains("stdin closed before a session began"),
        "a client that never initializes is a session that never started, not \
         a failure — and the clean exit above must be that path, not luck: {stderr}"
    );
}

/// The same promise on the shared path, where stdout is somebody else's bytes.
#[cfg(unix)]
#[test]
fn a_session_attached_to_the_shared_store_writes_nothing_of_its_own_to_stdout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        // `--embedder none` because this starts a real daemon: loading the
        // model would make the test a download. `--idle-timeout 1` so the
        // daemon it starts is gone a second later.
        .args(["--embedder", "none", "--idle-timeout", "1", "--data"])
        .arg(dir.path())
        .env("AGMEM_LOG", "trace")
        .output()
        .expect("run agmem");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(
        out.stdout.is_empty(),
        "stdout carries the daemon's replies and nothing else, got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        stderr.contains("started the shared store"),
        "the first session on a fresh data dir starts the daemon: {stderr}"
    );
    assert!(
        stderr.contains("the client hung up"),
        "and closing stdin detaches cleanly rather than erroring: {stderr}"
    );
    assert!(
        dir.path().join("daemon.log").exists(),
        "a detached daemon that logs nowhere is one nobody can diagnose"
    );
}
