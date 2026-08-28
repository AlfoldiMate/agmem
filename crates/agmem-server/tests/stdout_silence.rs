//! stdout is the MCP wire: startup and logging must never write to it.

use std::process::Command;

#[test]
fn startup_and_logging_write_nothing_to_stdout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--data"])
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
