//! Two sessions, one embedded store (issue #37).
//!
//! The daemon runs in this process and the sessions attach over the real Unix
//! socket with the real handshake, so what is under test is the whole
//! arrangement: one owner of a single-writer store, several MCP services on
//! top of it, and the per-connection configuration that keeps two projects
//! apart while they share it.

#![cfg(unix)]

use std::path::Path;
use std::time::Duration;

use agmem_server::config::{Cli, Config, ToolDescriptions};
use agmem_server::daemon::{self, Handshake};
use clap::Parser as _;
use rmcp::model::{CallToolRequestParams, ProtocolVersion};
use rmcp::service::RunningService;
use rmcp::{RoleClient, ServiceExt as _};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

/// One session's configuration against a shared data dir.
///
/// `--idle-timeout 0` because the test decides when the daemon stops; an
/// idle timer would be a race against the assertions.
fn config(data: &Path, space: &str) -> Config {
    Cli::try_parse_from([
        "agmem",
        "--data",
        &data.display().to_string(),
        "--space",
        space,
        "--embedder",
        "none",
        "--idle-timeout",
        "0",
    ])
    .expect("parse")
    .resolve()
    .expect("resolve")
}

/// A daemon owning a temp data dir, and the sessions that attach to it.
struct Shared {
    data: tempfile::TempDir,
    daemon: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Shared {
    /// A daemon that has opened the store and is accepting.
    async fn start() -> Self {
        let data = tempfile::tempdir().expect("tempdir");
        let daemon = tokio::spawn(daemon::serve::run(config(data.path(), "owner")));

        let socket = daemon::socket_path(data.path()).expect("socket path");
        for _ in 0..600 {
            if UnixStream::connect(&socket).await.is_ok() {
                return Self { data, daemon };
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the daemon never bound {}", socket.display());
    }

    async fn connect(&self) -> UnixStream {
        UnixStream::connect(daemon::socket_path(self.data.path()).expect("socket path"))
            .await
            .expect("connect to the daemon")
    }

    /// The line a session sends before MCP starts.
    fn handshake(&self, space: &str) -> Vec<u8> {
        self.handshake_with(space, ToolDescriptions::default())
    }

    /// The same, from a session that reworded some of its tools.
    fn handshake_with(&self, space: &str, tool_desc: ToolDescriptions) -> Vec<u8> {
        let mut cfg = config(self.data.path(), space);
        cfg.tool_desc = tool_desc;
        let mut line = serde_json::to_vec(&Handshake::new(&cfg)).expect("serialize the handshake");
        line.push(b'\n');
        line
    }

    /// A session attached as a real MCP client, past initialize.
    async fn attach(&self, space: &str) -> RunningService<RoleClient, ()> {
        self.attach_with(space, ToolDescriptions::default()).await
    }

    /// The same, asking the daemon to serve this project's own wording.
    async fn attach_with(
        &self,
        space: &str,
        tool_desc: ToolDescriptions,
    ) -> RunningService<RoleClient, ()> {
        let (read, mut write) = self.connect().await.into_split();
        write
            .write_all(&self.handshake_with(space, tool_desc))
            .await
            .expect("send the handshake");
        ().serve((read, write)).await.expect("initialize")
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.daemon.abort();
    }
}

/// One `tools/call` through an attached session.
async fn call(
    session: &RunningService<RoleClient, ()>,
    name: &'static str,
    arguments: Value,
) -> Value {
    let arguments = arguments.as_object().expect("an object").clone();
    let result = session
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    assert_ne!(result.is_error, Some(true), "{result:?}");
    result
        .structured_content
        .expect("a tool answers with structured content")
}

#[tokio::test]
async fn two_sessions_share_one_store() {
    let shared = Shared::start().await;
    let first = shared.attach("project-a").await;
    let second = shared.attach("project-b").await;

    let stored = call(
        &first,
        "remember",
        json!({ "memories": [{ "content": "The daemon lets two sessions hold one store" }] }),
    )
    .await;
    assert_eq!(
        stored["created"].as_array().expect("created").len(),
        1,
        "{stored}"
    );

    let found = call(&second, "recall", json!({ "space": "all" })).await;
    let contents: Vec<&str> = found["hits"]
        .as_array()
        .expect("hits")
        .iter()
        .filter_map(|hit| hit["content"].as_str())
        .collect();
    assert_eq!(
        contents,
        ["The daemon lets two sessions hold one store"],
        "a write through one session is visible to a recall through the other: {found}"
    );

    let spaces: Vec<&str> = found["spaces"]
        .as_array()
        .expect("spaces")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        spaces.contains(&"project-a") && spaces.contains(&"project-b"),
        "each attached session registers its own project, the way startup does \
         for a lone one: {found}"
    );
}

#[tokio::test]
async fn a_handshake_and_a_request_in_one_write_are_both_seen() {
    let shared = Shared::start().await;
    let mut bytes = shared.handshake("one-write");
    bytes.extend_from_slice(
        format!(
            "{}\n",
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": ProtocolVersion::LATEST.as_str(),
                    "capabilities": {},
                    "clientInfo": { "name": "agmem-daemon-test", "version": "0" },
                },
            })
        )
        .as_bytes(),
    );

    let (read, mut write) = shared.connect().await.into_split();
    write
        .write_all(&bytes)
        .await
        .expect("both lines in one write");

    let mut reply = String::new();
    BufReader::new(read)
        .read_line(&mut reply)
        .await
        .expect("a reply");
    let reply: Value = serde_json::from_str(&reply).expect("the reply is JSON");
    assert_eq!(
        reply["result"]["serverInfo"]["name"],
        json!("agmem"),
        "reading the handshake reads ahead, so the request sharing its write \
         must survive the buffer: {reply}"
    );
}

#[tokio::test]
async fn a_session_expecting_another_store_is_refused() {
    let shared = Shared::start().await;
    let mut asked = Handshake::new(&config(shared.data.path(), "elsewhere"));
    asked.db_url = "surrealkv:///somewhere/else".to_owned();
    let mut line = serde_json::to_vec(&asked).expect("serialize");
    line.push(b'\n');

    let (read, mut write) = shared.connect().await.into_split();
    write.write_all(&line).await.expect("send the handshake");

    let mut reply = String::new();
    let bytes = BufReader::new(read)
        .read_line(&mut reply)
        .await
        .expect("read");
    assert_eq!(
        bytes, 0,
        "a daemon that cannot serve what was asked for closes, rather than \
         serving something else: {reply:?}"
    );
}

#[tokio::test]
async fn each_session_reads_its_own_projects_wording() {
    let shared = Shared::start().await;
    let reworded = shared
        .attach_with(
            "opinionated",
            ToolDescriptions::from_iter([("recall", "Ask the store before you answer.")]),
        )
        .await;
    let plain = shared.attach("as-shipped").await;

    let described = |tools: rmcp::model::ListToolsResult, name: &str| {
        tools
            .tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} is routed"))
            .description
            .clone()
            .map(String::from)
    };
    let from_reworded = reworded.list_tools(None).await.expect("list_tools");
    let from_plain = plain.list_tools(None).await.expect("list_tools");

    assert_eq!(
        described(from_reworded, "recall").as_deref(),
        Some("Ask the store before you answer."),
        "the daemon serves the description the session asked for"
    );
    assert_ne!(
        described(from_plain, "recall").as_deref(),
        Some("Ask the store before you answer."),
        "and not to the project next door — the daemon is started by whichever \
         session got there first, so wording that did not travel would be that \
         one's wording for everybody"
    );
}

#[tokio::test]
async fn a_one_shot_context_reads_through_the_live_daemon() {
    let shared = Shared::start().await;
    let session = shared.attach("hook").await;
    call(
        &session,
        "remember",
        json!({ "memories": [{ "content": "The deploy target is Fly.io." }] }),
    )
    .await;

    // What `agmem context` runs after parsing (issue #46): same data dir, so
    // the one-shot finds the daemon above instead of starting one.
    let cfg = config(shared.data.path(), "hook");
    let block = agmem_server::oneshot::fetch(&cfg, agmem_server::config::ContextArgs::default())
        .await
        .expect("the one-shot answers through the daemon");
    assert!(
        block.starts_with("# Memory context (spaces: hook + user)"),
        "{block}"
    );
    assert!(block.contains("The deploy target is Fly.io."), "{block}");

    let tight = agmem_server::oneshot::fetch(
        &cfg,
        agmem_server::config::ContextArgs {
            budget_chars: Some(200),
            ..Default::default()
        },
    )
    .await
    .expect("the budget flag flows through to the tool");
    assert!(tight.chars().count() <= 200, "{tight}");

    // The one-shot detached; the session that was already attached still is.
    let found = call(&session, "recall", json!({})).await;
    assert_eq!(
        found["hits"].as_array().expect("hits").len(),
        1,
        "the daemon keeps serving after a one-shot came and went: {found}"
    );
}

#[tokio::test]
async fn a_probe_that_says_nothing_does_not_wedge_the_daemon() {
    let shared = Shared::start().await;
    // Connect and leave — what `--doctor` does to find out whether a daemon
    // is here at all.
    drop(shared.connect().await);

    let session = shared.attach("after-the-probe").await;
    let found = call(&session, "recall", json!({})).await;
    assert!(
        found["hits"].as_array().expect("hits").is_empty(),
        "the daemon still serves after a probe came and went: {found}"
    );
}
