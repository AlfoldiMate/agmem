//! stdout is the MCP wire: startup and logging must never write to it.
//!
//! Two paths reach a serving process now, so both are checked. The
//! in-process one logs from the same process that owns stdout; the shared one
//! pumps another process's bytes through it, and its own log lines must not
//! join them.

use std::process::Command;

/// Start agmem on a fresh data dir, let stdin close, and hand back its stderr
/// — having already checked the promise this file exists for.
fn started(args: &[&str]) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(args)
        .arg("--data")
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
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn in_process_startup_and_logging_write_nothing_to_stdout() {
    // `--embedder none`, because CI poisons `FASTEMBED_CACHE_DIR` on purpose
    // (design §7, issue #2): a test that loads the real model where the cache
    // does not exist is a test that *downloads* one, and it fails instead —
    // which is what kept this suite red from #11 onwards. The loader itself is
    // covered by the ignored twin below.
    let stderr = started(&["--no-daemon", "--embedder", "none"]);

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

/// The same promise with the real model behind it.
///
/// The embedder is the likeliest thing in the process to write to stdout —
/// it is somebody else's C++ — and `--embedder none` is exactly the path that
/// does not exercise it. Ignored rather than deleted: it needs the model on
/// disk, so it belongs to a developer with a warm cache, not to CI.
///
/// `cargo test -p agmem-server --test stdout_silence -- --ignored`
#[test]
#[ignore = "loads the real embedding model"]
fn the_real_embedder_writes_nothing_to_stdout_either() {
    let stderr = started(&["--no-daemon"]);

    assert!(
        stderr.contains("agmem starting"),
        "expected the startup log line on stderr: {stderr}"
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
