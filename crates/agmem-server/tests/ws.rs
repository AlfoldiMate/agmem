//! Sharing mode against a real SurrealDB server (issue #33).
//!
//! The in-process tests cover every engine `engine::any` can open embedded;
//! what only a real server can prove is the sharing story: two agmem
//! processes on one `ws://` store at the same time, no lock file, credentials
//! reaching an authenticated server, and a session surviving a server
//! restart. Each test skips — loudly — when `surreal` is not on PATH; CI
//! installs it so the skip only happens on undressed dev machines.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Whether the `surreal` binary exists to test against.
fn surreal_on_path() -> bool {
    let found = Command::new("surreal")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if !found {
        eprintln!("skipping: `surreal` is not on PATH (brew install surrealdb/tap/surreal)");
    }
    found
}

/// A port nothing is listening on right now.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// A SurrealDB server owned by the test; killed on drop.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    /// `surreal start` on `port` with the given auth arguments and backend
    /// (`"memory"`, or a `surrealkv://` path for state that survives a
    /// restart), ready to accept connections when this returns.
    fn start(port: u16, auth: &[&str], backend: &str) -> Self {
        let child = Command::new("surreal")
            .args(["start", "--bind", &format!("127.0.0.1:{port}")])
            .args(auth)
            .arg(backend)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn surreal");
        let deadline = Instant::now() + Duration::from_secs(15);
        while TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(
                Instant::now() < deadline,
                "surreal never opened port {port}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        Self { child, port }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One agmem process spoken to over stdio, the way a real MCP client would.
///
/// A reader thread feeds stdout lines through a channel so every wait can
/// carry a timeout — a hung protocol conversation fails the test instead of
/// hanging it.
struct Agmem {
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    next_id: i64,
    data: tempfile::TempDir,
}

impl Agmem {
    /// Spawn against `db_url` and finish the MCP initialize handshake.
    fn start(db_url: &str, env: &[(&str, &str)]) -> Self {
        let data = tempfile::tempdir().expect("tempdir");
        let mut child = Command::new(env!("CARGO_BIN_EXE_agmem"))
            .args(["--db", db_url, "--embedder", "none", "--data"])
            .arg(data.path())
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn agmem");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut agmem = Self {
            child,
            stdin,
            lines,
            next_id: 0,
            data,
        };
        let hello = agmem.request(
            "initialize",
            json!({
                "protocolVersion": rmcp::model::ProtocolVersion::LATEST.as_str(),
                "capabilities": {},
                "clientInfo": { "name": "agmem-ws-test", "version": "0" },
            }),
        );
        assert!(hello.get("result").is_some(), "initialize failed: {hello}");
        agmem.notify("notifications/initialized");
        agmem
    }

    /// Send one request and wait for the response that answers it.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{line}").expect("write request");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("agmem answered within 30s");
            let line = self.lines.recv_timeout(remaining).expect("agmem replied");
            let reply: Value = serde_json::from_str(&line).expect("stdout is JSON-RPC");
            if reply.get("id") == Some(&json!(id)) {
                return reply;
            }
        }
    }

    fn notify(&mut self, method: &str) {
        let line = json!({ "jsonrpc": "2.0", "method": method });
        writeln!(self.stdin, "{line}").expect("write notification");
    }

    /// `tools/call`, asserting the tool itself did not report an error.
    fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let reply = self.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        let result = reply
            .get("result")
            .unwrap_or_else(|| panic!("{tool} failed: {reply}"));
        assert_ne!(
            result.get("isError"),
            Some(&json!(true)),
            "{tool} returned an error: {result}"
        );
        result.clone()
    }
}

impl Drop for Agmem {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn two_sessions_share_one_server_without_a_lock_file() {
    if !surreal_on_path() {
        return;
    }
    let server = Server::start(free_port(), &["--unauthenticated"], "memory");

    // Sequential starts (concurrent first migrations are not the point), but
    // both stay connected for the whole conversation.
    let mut first = Agmem::start(&server.url(), &[]);
    let mut second = Agmem::start(&server.url(), &[]);

    first.call(
        "remember",
        json!({ "memories": [{ "content": "The staging cluster runs kubernetes one thirty." }] }),
    );
    let found = second.call("recall", json!({ "query": "staging cluster kubernetes" }));
    assert!(
        found.to_string().contains("one thirty"),
        "the second session cannot see the first session's memory: {found}"
    );

    // And the other direction, while both are still attached.
    second.call(
        "remember",
        json!({ "memories": [{ "content": "The retention sweep runs on Sunday nights." }] }),
    );
    let found = first.call("recall", json!({ "query": "retention sweep Sunday" }));
    assert!(found.to_string().contains("Sunday nights"), "{found}");

    // The server is the single-writer boundary: nobody locked a data dir.
    for agmem in [&first, &second] {
        assert!(
            !agmem.data.path().join("agmem.lock").exists(),
            "a remote engine must not take the embedded store lock"
        );
    }
}

#[test]
fn credentials_reach_an_authenticated_server() {
    if !surreal_on_path() {
        return;
    }
    let server = Server::start(
        free_port(),
        &["--user", "root", "--pass", "s3cret"],
        "memory",
    );

    // Without credentials the server refuses agmem's first queries and the
    // process exits non-zero instead of serving a store it cannot read.
    let refused = Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--db", &server.url(), "--embedder", "none", "--data"])
        .arg(tempfile::tempdir().expect("tempdir").path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn agmem")
        .wait_with_output()
        .expect("wait");
    assert!(
        !refused.status.success(),
        "agmem served an authenticated store it never signed in to"
    );

    // With the pair from the environment, the whole loop works.
    let creds = [("AGMEM_DB_USER", "root"), ("AGMEM_DB_PASS", "s3cret")];
    let mut agmem = Agmem::start(&server.url(), &creds);
    agmem.call(
        "remember",
        json!({ "memories": [{ "content": "The audit log lives in the compliance bucket." }] }),
    );
    let found = agmem.call("recall", json!({ "query": "audit log compliance" }));
    assert!(found.to_string().contains("compliance bucket"), "{found}");
}

#[test]
fn a_session_survives_a_server_restart() {
    if !surreal_on_path() {
        return;
    }
    let store = tempfile::tempdir().expect("tempdir");
    let backend = format!("surrealkv://{}", store.path().join("shared.db").display());
    let port = free_port();
    let server = Server::start(port, &["--unauthenticated"], &backend);

    let mut agmem = Agmem::start(&server.url(), &[]);
    agmem.call(
        "remember",
        json!({ "memories": [{ "content": "The ws reconnect probe stored this before the restart." }] }),
    );

    drop(server);
    let _server = Server::start(port, &["--unauthenticated"], &backend);

    // The SDK reconnects on its own; give it a moment rather than demanding
    // the first attempt succeed.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let reply = agmem.request(
            "tools/call",
            json!({ "name": "recall", "arguments": { "query": "ws reconnect probe restart" } }),
        );
        let text = reply.to_string();
        let healthy = reply
            .get("result")
            .is_some_and(|result| result.get("isError") != Some(&json!(true)));
        if healthy && text.contains("before the restart") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "recall never recovered after the server restart: {reply}"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}
