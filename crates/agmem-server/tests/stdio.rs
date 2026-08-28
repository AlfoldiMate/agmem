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
    // What the surface *is* belongs to the protocol snapshot; all this has to
    // show is that routing survives a real transport, so it names one tool
    // rather than pinning the list every tool issue would have to re-record.
    let listed = reply(2);
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list must answer with an array: {listed}"))
        .iter()
        .map(|tool| tool["name"].as_str().expect("every tool has a name"))
        .collect();
    assert!(
        names.contains(&"remember"),
        "the binary must route what the service registers: {names:?}"
    );
    assert!(
        stderr.contains("agmem starting"),
        "logging still belongs on stderr: {stderr}"
    );
}

/// Two `agmem` processes on one data dir, the way two Claude Code sessions
/// arrive (issue #37).
///
/// This is the acceptance criterion and the one thing only real processes can
/// show: the first binary starts the shared daemon, the second finds it, and
/// what one writes the other reads. Before the daemon, the second process
/// exited on the lock and that session silently had no memory tools.
#[cfg(unix)]
#[test]
fn two_processes_on_one_data_dir_both_serve() {
    use std::io::{BufRead as _, BufReader};

    let data = tempfile::tempdir().expect("tempdir");

    /// A session's stdin, and its stdout already framed into lines.
    struct Session {
        child: std::process::Child,
        stdin: std::process::ChildStdin,
        stdout: BufReader<std::process::ChildStdout>,
    }

    impl Session {
        /// Send one request and read the one reply it produces.
        fn ask(&mut self, request: serde_json::Value) -> serde_json::Value {
            self.tell(request);
            let mut reply = String::new();
            self.stdout.read_line(&mut reply).expect("a reply");
            serde_json::from_str(&reply).unwrap_or_else(|e| panic!("{reply:?}: {e}"))
        }

        /// Send one notification, which produces nothing to read.
        fn tell(&mut self, request: serde_json::Value) {
            self.stdin
                .write_all(line(request).as_bytes())
                .expect("write request");
        }
    }

    let start = |space: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agmem"))
            .args(["--embedder", "none", "--space", space])
            // Short enough that a failed run does not leave a daemon behind
            // for long, long enough that it cannot expire mid-test.
            .args(["--idle-timeout", "30"])
            .arg("--data")
            .arg(data.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn agmem");
        let mut session = Session {
            stdin: child.stdin.take().expect("stdin"),
            stdout: BufReader::new(child.stdout.take().expect("stdout")),
            child,
        };
        let hello = session.ask(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": ProtocolVersion::LATEST.as_str(),
                "capabilities": {},
                "clientInfo": { "name": "agmem-shared-test", "version": "0" },
            },
        }));
        assert_eq!(
            hello["result"]["serverInfo"]["name"],
            serde_json::json!("agmem"),
            "a session that cannot reach the shared store must not pretend to serve: {hello}"
        );
        session.tell(serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized"
        }));
        session
    };

    let mut first = start("project-a");
    let mut second = start("project-b");

    let stored = first.ask(serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {
            "name": "remember",
            "arguments": { "memories": [{ "content": "Two sessions, one store" }] },
        },
    }));
    assert_eq!(
        stored["result"]["structuredContent"]["created"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "the first session wrote: {stored}"
    );

    let found = second.ask(serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "recall", "arguments": { "space": "all" } },
    }));
    let contents: Vec<&str> = found["result"]["structuredContent"]["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|hit| hit["content"].as_str())
        .collect();
    assert_eq!(
        contents,
        ["Two sessions, one store"],
        "the second process reads what the first wrote: {found}"
    );

    for mut session in [first, second] {
        drop(session.stdin);
        let status = session.child.wait().expect("wait");
        assert!(status.success(), "a session must hang up cleanly: {status}");
    }
}
