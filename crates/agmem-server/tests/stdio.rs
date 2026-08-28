//! The binary really speaks MCP on stdin/stdout.
//!
//! The in-process protocol tests drive `AgmemService` over a duplex pipe,
//! which proves the service and skips the two things that can only break in
//! the real binary: that `main` reaches the serve loop at all, and that the
//! stdio transport owns stdout uncontested. This test spawns `agmem` as a
//! child process and talks raw JSON-RPC at it, the way a client would.

use std::io::Write as _;
use std::process::{Command, Stdio};

use rmcp::model::ProtocolVersion;

/// One JSON-RPC line, as an MCP client would frame it.
fn line(value: serde_json::Value) -> String {
    format!("{value}\n")
}

#[test]
fn the_binary_answers_initialize_and_tools_list_over_stdio() {
    let data = tempfile::tempdir().expect("tempdir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--db", "mem://", "--embedder", "none", "--data"])
        .arg(data.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn agmem");

    let mut stdin = child.stdin.take().expect("stdin");
    for request in [
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": ProtocolVersion::LATEST.as_str(),
                "capabilities": {},
                "clientInfo": { "name": "agmem-stdio-test", "version": "0" },
            },
        }),
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    ] {
        stdin
            .write_all(line(request).as_bytes())
            .expect("write request");
    }
    // EOF is how a client hangs up; the serve loop must end and the process
    // exit cleanly rather than hanging on a closed pipe.
    drop(stdin);

    let out = child.wait_with_output().expect("wait for agmem");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "agmem exited badly; stderr: {stderr}");

    let replies: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line:?}: {e}")))
        .collect();
    let reply = |id: i64| {
        replies
            .iter()
            .find(|reply| reply["id"] == id)
            .unwrap_or_else(|| panic!("no reply with id {id} in {replies:?}"))
    };

    let initialize = reply(1);
    assert_eq!(initialize["result"]["serverInfo"]["name"], "agmem");
    assert_eq!(
        initialize["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        initialize["result"]["capabilities"]["tools"].is_object(),
        "the tool capability must survive the real transport: {initialize}"
    );
    assert_eq!(
        reply(2)["result"]["tools"],
        serde_json::json!([]),
        "no tools are registered yet; the tool issues re-record this"
    );
    assert!(
        stderr.contains("agmem starting"),
        "logging still belongs on stderr: {stderr}"
    );
}
