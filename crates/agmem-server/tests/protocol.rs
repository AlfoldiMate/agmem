//! The MCP surface, driven by a real rmcp client in this process.
//!
//! Client and server sit on the two ends of a `tokio::io::duplex` pipe, which
//! is the SDK's own test pattern (design §7.3): the full JSON-RPC framing,
//! the initialize handshake and the tool dispatch all run, without a child
//! process or a socket.
//!
//! The snapshots are the point. Nothing in this stack breaks loudly — an rmcp
//! or schemars upgrade that changes how a tool schema is generated produces a
//! server that still starts, still lists tools, and quietly describes them
//! differently to every agent. The snapshot is what turns that into a failing
//! test.

use std::sync::Arc;

use agmem_embed::NoopEmbedder;
use agmem_server::config::Cli;
use agmem_server::service::AgmemService;
use clap::Parser as _;
use rmcp::RoleClient;
use rmcp::ServiceExt as _;
use rmcp::service::RunningService;

/// A client already through the initialize handshake, and the server task it
/// is talking to.
///
/// `().serve(..)` is a complete MCP client: rmcp implements `ClientHandler`
/// for the unit type, and `serve` does not return until initialize has been
/// answered — so anything the client says afterwards is post-handshake.
async fn connected() -> (
    RunningService<RoleClient, ()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    // `mem://` never touches the data dir, but resolving one is part of
    // startup, so the test points it somewhere disposable rather than at the
    // developer's real platform directory.
    let data = tempfile::tempdir().expect("tempdir");
    let config = Cli::try_parse_from([
        "agmem",
        "--db",
        "mem://",
        "--embedder",
        "none",
        "--data",
        &data.path().display().to_string(),
    ])
    .expect("parse")
    .resolve()
    .expect("resolve");
    let db = agmem_store::db::connect(&config.db_url)
        .await
        .expect("connect mem://");
    agmem_store::migrate::ensure(&db).await.expect("migrate");
    let service = AgmemService::new(db, Arc::new(NoopEmbedder), Arc::new(config));

    let (server_end, client_end) = tokio::io::duplex(4096);
    let server = tokio::spawn(async move {
        service.serve(server_end).await?.waiting().await?;
        anyhow::Ok(())
    });
    let client = ().serve(client_end).await.expect("initialize");
    (client, server)
}

#[tokio::test]
async fn initialize_announces_agmem_and_its_tool_capability() {
    let (client, server) = connected().await;
    let info = client.peer_info().expect("negotiated server info").clone();

    let implementation = info.server_info.as_ref().expect("server implementation");
    assert_eq!(
        implementation.name, "agmem",
        "rmcp's own name leaks through here if `with_server_info` is ever dropped"
    );
    assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
    assert!(
        info.capabilities.tools.is_some(),
        "a hand-written get_info must re-declare the tool capability"
    );
    let instructions = info.instructions.as_deref().expect("instructions");
    assert!(
        instructions.contains("supersedes"),
        "the session-level instructions must name the correction path: {instructions}"
    );

    insta::assert_json_snapshot!("initialize", &info.capabilities);
    client.cancel().await.expect("shut down");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn list_tools_matches_the_recorded_surface() {
    let (client, server) = connected().await;
    let tools = client.list_tools(None).await.expect("list_tools");

    // Empty until the tool issues land; every one of them re-records this.
    insta::assert_json_snapshot!("list_tools", &tools.tools);
    assert!(
        tools.next_cursor.is_none(),
        "the whole surface fits one page"
    );

    client.cancel().await.expect("shut down");
    server.await.expect("join").expect("serve");
}

#[tokio::test]
async fn an_unknown_tool_is_refused_rather_than_ignored() {
    let (client, server) = connected().await;
    let error = client
        .call_tool(rmcp::model::CallToolRequestParams::new("no_such_tool"))
        .await
        .expect_err("an unrouted name must fail");

    // rmcp answers with a JSON-RPC error rather than an empty success, and
    // does not echo the name back — so the code is what a client can act on.
    assert!(
        matches!(&error, rmcp::service::ServiceError::McpError(data)
            if data.code == rmcp::model::ErrorCode::INVALID_PARAMS),
        "an unrouted name must come back as a protocol error: {error}"
    );
    client.cancel().await.expect("shut down");
    server.await.expect("join").expect("serve");
}
