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
use agmem_server::daemon::{self, Ack, Handshake};
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
        // The daemon answers the handshake before MCP starts (issue #60);
        // the buffered reader must carry into rmcp, or bytes it read ahead
        // would be lost.
        let mut read = BufReader::new(read);
        let mut ack = String::new();
        read.read_line(&mut ack).await.expect("read the ack");
        let ack: Ack = serde_json::from_str(&ack).expect("the ack is JSON");
        assert!(ack.ok, "the daemon takes the session: {ack:?}");
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

    let mut read = BufReader::new(read);
    let mut ack = String::new();
    read.read_line(&mut ack).await.expect("the ack");
    let ack: Ack = serde_json::from_str(&ack).expect("the ack is JSON");
    assert!(ack.ok, "{ack:?}");

    let mut reply = String::new();
    read.read_line(&mut reply).await.expect("a reply");
    let reply: Value = serde_json::from_str(&reply).expect("the reply is JSON");
    assert_eq!(
        reply["result"]["serverInfo"]["name"],
        json!("agmem"),
        "reading the handshake reads ahead, so the request sharing its write \
         must survive the buffer: {reply}"
    );
}

#[tokio::test]
async fn a_session_expecting_another_store_is_refused_and_told_so() {
    let shared = Shared::start().await;
    let mut asked = Handshake::new(&config(shared.data.path(), "elsewhere"));
    asked.db_url = "surrealkv:///somewhere/else".to_owned();
    let mut line = serde_json::to_vec(&asked).expect("serialize");
    line.push(b'\n');

    let (read, mut write) = shared.connect().await.into_split();
    write.write_all(&line).await.expect("send the handshake");

    let mut read = BufReader::new(read);
    let mut reply = String::new();
    read.read_line(&mut reply).await.expect("read the ack");
    let ack: Ack = serde_json::from_str(&reply).expect("the refusal is an ack, not a bare close");
    assert!(!ack.ok, "{ack:?}");
    assert!(
        ack.error
            .as_deref()
            .unwrap_or_default()
            .contains("--no-daemon"),
        "the refusal names a way out (issue #60): {ack:?}"
    );
    assert!(
        !ack.retiring,
        "a misconfigured session is not a reason to stop serving everyone else: {ack:?}"
    );

    reply.clear();
    let bytes = read.read_line(&mut reply).await.expect("read");
    assert_eq!(
        bytes, 0,
        "after refusing, the daemon closes rather than serving something \
         else: {reply:?}"
    );
}

#[tokio::test]
async fn a_session_from_another_release_retires_the_daemon() {
    let mut shared = Shared::start().await;
    let mut asked = Handshake::new(&config(shared.data.path(), "upgraded"));
    asked.release = "999.0.0".to_owned();
    let mut line = serde_json::to_vec(&asked).expect("serialize");
    line.push(b'\n');

    let (read, mut write) = shared.connect().await.into_split();
    write.write_all(&line).await.expect("send the handshake");

    let mut reply = String::new();
    BufReader::new(read)
        .read_line(&mut reply)
        .await
        .expect("read the ack");
    let ack: Ack = serde_json::from_str(&reply).expect("the ack is JSON");
    assert!(!ack.ok && ack.retiring, "{ack:?}");

    // Retiring is not just a word: the daemon's run loop actually returns,
    // releasing the socket and the store lock so the refused session can
    // start a daemon from its own binary.
    tokio::time::timeout(Duration::from_secs(10), &mut shared.daemon)
        .await
        .expect("the daemon shuts down after refusing another release")
        .expect("cleanly")
        .expect("without error");
    assert!(
        !daemon::socket_path(shared.data.path())
            .expect("socket path")
            .exists(),
        "the retiring daemon unlinks its socket so the fresh one can bind"
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
async fn a_document_put_through_the_daemon_is_read_by_the_sessions() {
    use agmem_server::config::{DocGetArgs, DocPutArgs};

    let shared = Shared::start().await;
    let cfg = config(shared.data.path(), "hook");

    // What `agmem doc put < plan.md` runs after parsing (#135): same data
    // dir, so the one-shot finds the daemon above instead of starting one.
    let content = "# Plan\n\nStep one.\n".repeat(2_000);
    let line = agmem_server::doc::put(
        &cfg,
        DocPutArgs {
            title: "plan-x".to_owned(),
            kind: agmem_core::DocKind::Plan,
            tags: vec!["role:architect".to_owned()],
            mime: "text/markdown".to_owned(),
            space: None,
        },
        content.clone(),
    )
    .await
    .expect("put through the daemon");
    let (id, uri) = line.trim_end().split_once(' ').expect("id and uri");
    assert_eq!(uri, format!("memory://hook/doc/{id}"), "{line}");

    let session = shared.attach("hook").await;
    let found = call(&session, "inspect", json!({ "ref": "doc:current/plan-x" })).await;
    assert_eq!(found["found"]["episode"]["id"], id, "{found}");
    assert_eq!(found["found"]["episode"]["tags"], json!(["role:architect"]));

    let back = agmem_server::doc::get(
        &cfg,
        DocGetArgs {
            reference: "plan-x".to_owned(),
            offset: None,
            limit: None,
            raw: true,
            space: None,
        },
    )
    .await
    .expect("get through the daemon");
    assert_eq!(back, content, "the whole document, through the socket");

    // The daemon above was started by a `core` config, and the session it
    // serves lists the core surface — yet a one-shot reaches `forget`, the
    // gated tool, because it asks for `all` on its own handshake (#150).
    let listed = session.list_tools(None).await.expect("list_tools");
    assert_eq!(
        listed.tools.len(),
        agmem_server::tools::NAMES.len() - agmem_server::tools::GATED.len(),
        "a default session reads the core list"
    );
    let purged = agmem_server::doc::forget(
        &cfg,
        agmem_server::config::DocForgetArgs {
            id: id.to_owned(),
            purge: true,
            cascade: false,
            space: None,
        },
    )
    .await
    .expect("forget through a core daemon");
    assert!(purged.starts_with("purged 1 record(s)"), "{purged}");
}

#[tokio::test]
async fn a_session_asking_for_every_tool_is_served_every_tool() {
    let shared = Shared::start().await;
    let mut cfg = config(shared.data.path(), "wide");
    cfg.tools = agmem_server::config::ToolGroup::All;

    let (read, mut write) = shared.connect().await.into_split();
    let mut line = serde_json::to_vec(&Handshake::new(&cfg)).expect("serialize");
    line.push(b'\n');
    write.write_all(&line).await.expect("send the handshake");
    let mut read = BufReader::new(read);
    let mut ack = String::new();
    read.read_line(&mut ack).await.expect("read the ack");
    let ack: Ack = serde_json::from_str(&ack).expect("the ack is JSON");
    assert!(ack.ok, "{ack:?}");
    let session = ().serve((read, write)).await.expect("initialize");

    let listed = session.list_tools(None).await.expect("list_tools");
    assert_eq!(listed.tools.len(), agmem_server::tools::NAMES.len());
    let found = call(&session, "consolidate", json!({})).await;
    assert_eq!(found["spaces"], json!(["wide"]), "{found}");
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

#[tokio::test]
async fn a_session_from_an_older_release_is_refused_and_the_daemon_stays() {
    let shared = Shared::start().await;
    let mut asked = Handshake::new(&config(shared.data.path(), "downgraded"));
    asked.release = "0.0.1".to_owned();
    let mut line = serde_json::to_vec(&asked).expect("serialize");
    line.push(b'\n');

    let (read, mut write) = shared.connect().await.into_split();
    write.write_all(&line).await.expect("send the handshake");
    let mut reply = String::new();
    BufReader::new(read)
        .read_line(&mut reply)
        .await
        .expect("read the ack");
    let ack: Ack = serde_json::from_str(&reply).expect("the ack is JSON");
    assert!(
        !ack.ok && !ack.retiring,
        "an older attacher is a second install on PATH, not an upgrade (issue #112); \
         retiring for it would let two binaries retire each other in turn: {ack:?}"
    );
    assert!(
        ack.error
            .as_deref()
            .unwrap_or_default()
            .contains("--no-daemon"),
        "the refusal names a way out: {ack:?}"
    );

    // The daemon is still the newest binary on the socket, so it keeps serving.
    let session = shared.attach("still-served").await;
    let found = call(&session, "recall", json!({})).await;
    assert!(
        found["hits"].as_array().expect("hits").is_empty(),
        "the daemon serves on after refusing an older release: {found}"
    );
}

#[tokio::test]
async fn a_retiring_daemon_turns_late_attachers_away_and_drains_before_exiting() {
    let mut shared = Shared::start().await;
    let already_here = shared.attach("already-here").await;
    // Connected before the retirement, handshake not yet sent: the shape of
    // a session that raced the upgrade.
    let (late_read, mut late_write) = shared.connect().await.into_split();

    let mut asked = Handshake::new(&config(shared.data.path(), "upgraded"));
    asked.release = "999.0.0".to_owned();
    let mut line = serde_json::to_vec(&asked).expect("serialize");
    line.push(b'\n');
    let (read, mut write) = shared.connect().await.into_split();
    write.write_all(&line).await.expect("send the handshake");
    let mut reply = String::new();
    BufReader::new(read)
        .read_line(&mut reply)
        .await
        .expect("read the ack");
    let ack: Ack = serde_json::from_str(&reply).expect("the ack is JSON");
    assert!(!ack.ok && ack.retiring, "{ack:?}");
    let retired_at = std::time::Instant::now();

    // The daemon unlinks its socket the moment it stops accepting; that is
    // the observable edge of "retiring" for the late session below.
    let socket = daemon::socket_path(shared.data.path()).expect("socket path");
    for _ in 0..500 {
        if !socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !socket.exists(),
        "a retiring daemon stops advertising itself at once"
    );

    late_write
        .write_all(&shared.handshake("late"))
        .await
        .expect("the late handshake lands on a connection the daemon accepted");
    let mut reply = String::new();
    BufReader::new(late_read)
        .read_line(&mut reply)
        .await
        .expect("read the late ack");
    let ack: Ack = serde_json::from_str(&reply).expect("the late ack is JSON");
    assert!(
        !ack.ok && ack.retiring,
        "a session accepted onto a retiring daemon would come up with memory tools and \
         lose them a moment later; it hears the same 'wait' the upgrader did (issue \
         #112): {ack:?}"
    );

    // The session that was already attached is served through the drain
    // and cut loose at its end, not before.
    let found = call(&already_here, "recall", json!({})).await;
    assert!(
        found["hits"].as_array().expect("hits").is_empty(),
        "{found}"
    );
    tokio::time::timeout(Duration::from_secs(10), &mut shared.daemon)
        .await
        .expect("the daemon exits once the drain is over")
        .expect("cleanly")
        .expect("without error");
    assert!(
        retired_at.elapsed() >= daemon::serve::DRAIN,
        "a session was still attached, so the daemon stayed for the drain window"
    );
    already_here
        .waiting()
        .await
        .expect("the daemon's exit closes the session's transport");
}
