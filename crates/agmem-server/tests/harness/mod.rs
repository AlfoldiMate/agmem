//! The in-process client/server harness the MCP-surface tests drive, shared
//! between `protocol.rs` (the surface's own tests) and `eval.rs` (the
//! memory-quality eval). Client and server sit on the two ends of a
//! `tokio::io::duplex` pipe — the SDK's own test pattern (design §7.3) — so
//! the full JSON-RPC framing, initialize handshake and tool dispatch all run
//! without a child process or a socket.
//!
//! Each test target compiles its own copy of this module and uses a different
//! subset of it, so the lint has nothing useful to say here.
#![allow(dead_code)]

pub mod recorded;

use std::sync::Arc;

use agmem_core::{MemoryRecord, SpaceName};
use agmem_embed::{EmbedError, Embedder};
use agmem_server::config::{Cli, ToolDescriptions, ToolGroup};
use agmem_server::service::AgmemService;
use agmem_store::db::Db;
use agmem_store::migrate;
use agmem_store::repo::{self, Liveness, Lookup, SpaceStats};
use clap::Parser as _;
use rmcp::RoleClient;
use rmcp::ServiceExt as _;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorCode, MetaObject, RequestMetaObject,
};
use rmcp::service::{RunningService, ServiceError};
use serde_json::{Value, json};

/// A server, the client talking to it, and the store behind both.
pub struct Harness {
    pub client: RunningService<RoleClient, ()>,
    pub server: tokio::task::JoinHandle<anyhow::Result<()>>,
    pub db: Db,
    /// `mem://` never touches the data dir, but resolving one is part of
    /// startup, so it points somewhere disposable rather than at the
    /// developer's real platform directory. Dropped last.
    _data: tempfile::TempDir,
}

impl Harness {
    /// A client already through the initialize handshake, on an empty store.
    ///
    /// `().serve(..)` is a complete MCP client: rmcp implements `ClientHandler`
    /// for the unit type, and `serve` does not return until initialize has been
    /// answered — so anything the client says afterwards is post-handshake.
    ///
    /// Serves the whole surface (`AGMEM_TOOLS=all`), because the suite
    /// exercises every tool; [`Self::start_core`] is the list an agent reads.
    pub async fn start(embedder: Arc<dyn Embedder>) -> Self {
        Self::start_with(embedder, ToolDescriptions::default()).await
    }

    /// The default list (#150): what a session with nothing configured
    /// serves, which leaves out `consolidate` and `forget`.
    pub async fn start_core(embedder: Arc<dyn Embedder>) -> Self {
        Self::configure(embedder, ToolDescriptions::default(), ToolGroup::Core).await
    }

    /// The whole surface, serving `tool_desc` instead of agmem's own wording.
    pub async fn start_with(embedder: Arc<dyn Embedder>, tool_desc: ToolDescriptions) -> Self {
        Self::configure(embedder, tool_desc, ToolGroup::All).await
    }

    /// A client on an empty store, serving `tools` with `tool_desc` applied.
    ///
    /// The overrides are set on the resolved config rather than left to
    /// `AGMEM_TOOL_DESC_*` and `AGMEM_TOOLS`: `Cli::resolve` reads the real
    /// environment, and a developer who has one of those exported would
    /// otherwise fail the `list_tools` snapshot with a diff that has nothing
    /// to do with their change.
    pub async fn configure(
        embedder: Arc<dyn Embedder>,
        tool_desc: ToolDescriptions,
        tools: ToolGroup,
    ) -> Self {
        let data = tempfile::tempdir().expect("tempdir");
        let mut config = Cli::try_parse_from([
            "agmem",
            "--db",
            "mem://",
            // Only `embedder::build` reads this; the service is handed the
            // backend a test wants directly.
            "--embedder",
            "none",
            // Left unset, the space derives from the checkout's own name
            // (#44); [`space`] and every assertion quoting it expect this one.
            "--space",
            "default",
            "--data",
            &data.path().display().to_string(),
        ])
        .expect("parse")
        .resolve()
        .expect("resolve");
        config.tool_desc = tool_desc;
        config.tools = tools;
        let db = agmem_store::db::connect(&config.db_url)
            .await
            .expect("connect mem://");
        migrate::ensure(&db).await.expect("migrate");
        // Startup records the embedder pair right after migrating (main.rs),
        // and a first backend whose width differs from the schema's baked
        // 384 adopts the indexes there, so the harness mirrors it.
        migrate::ensure_embedder(&db, embedder.model_id(), embedder.dim())
            .await
            .expect("record the embedder");
        // Startup registers the configured space before it serves anything
        // (design §5.1 step 8, `main.rs`). The registry is a listing, so
        // nothing fails without it — `recall`'s `space: "all"` just quietly
        // leaves out the space the server is actually serving.
        repo::ensure_space(&db, &space())
            .await
            .expect("register the space");
        let service = AgmemService::new(db.clone(), embedder, Arc::new(config));

        let (server_end, client_end) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            service.serve(server_end).await?.waiting().await?;
            anyhow::Ok(())
        });
        let client = ().serve(client_end).await.expect("initialize");
        Self {
            client,
            server,
            db,
            _data: data,
        }
    }

    /// One `tools/call`, with whatever came back.
    pub async fn call(
        &self,
        name: &'static str,
        arguments: Value,
    ) -> Result<CallToolResult, ServiceError> {
        let arguments = arguments
            .as_object()
            .expect("arguments are an object")
            .clone();
        self.client
            .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
            .await
    }

    /// One `tools/call` attributed to `session` via `_meta` (issue #75) —
    /// the per-request override `tools::writer` honours before falling back
    /// to the id the connection was minted. What per-seed writer attribution
    /// in the eval fixtures (issue #86) will ride on.
    pub async fn call_as(
        &self,
        session: &str,
        name: &'static str,
        arguments: Value,
    ) -> Result<CallToolResult, ServiceError> {
        let arguments = arguments
            .as_object()
            .expect("arguments are an object")
            .clone();
        let mut meta = rmcp::model::JsonObject::new();
        meta.insert("agmem/session".to_owned(), json!(session));
        let mut params = CallToolRequestParams::new(name).with_arguments(arguments);
        params.meta = Some(RequestMetaObject(MetaObject(meta)));
        self.client.call_tool(params).await
    }

    /// One successful `remember`, as the structured result an agent reads.
    pub async fn remember(&self, arguments: Value) -> Value {
        let result = self.call("remember", arguments).await.expect("remember");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("remember answers with structured content, not just text")
    }

    /// One successful `reflect`, as the structured result an agent reads.
    pub async fn reflect(&self, arguments: Value) -> Value {
        let result = self.call("reflect", arguments).await.expect("reflect");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("reflect answers with structured content, not just text")
    }

    /// One successful `recall`, as the structured result an agent reads.
    pub async fn recall(&self, arguments: Value) -> Value {
        let result = self.call("recall", arguments).await.expect("recall");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("recall answers with structured content, not just text")
    }

    /// One successful `consolidate`, as the structured result an agent reads.
    pub async fn consolidate(&self, arguments: Value) -> Value {
        let result = self
            .call("consolidate", arguments)
            .await
            .expect("consolidate");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("consolidate answers with structured content, not just text")
    }

    /// One successful `context`, as the markdown block an agent reads.
    pub async fn context(&self, arguments: Value) -> String {
        let result = self.call("context", arguments).await.expect("context");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        assert!(
            result.structured_content.is_none(),
            "context is a block for the prompt, not a record to parse"
        );
        match result.content.first().expect("one content block") {
            ContentBlock::Text(text) => text.text.clone(),
            other => panic!("context answers with text, got {other:?}"),
        }
    }

    /// One successful `inspect`, as the structured result an agent reads.
    pub async fn inspect(&self, reference: &str) -> Value {
        let result = self
            .call("inspect", json!({ "ref": reference }))
            .await
            .expect("inspect");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("inspect answers with structured content, not just text")
    }

    /// One successful `forget`, as the structured result an agent reads.
    pub async fn forget(&self, arguments: Value) -> Value {
        let result = self.call("forget", arguments).await.expect("forget");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("forget answers with structured content, not just text")
    }

    /// Every memory the default space holds, closed ones included.
    pub async fn memories(&self) -> Vec<MemoryRecord> {
        let mut lookup = Lookup::new(vec![space()]);
        lookup.liveness = Liveness::Any;
        repo::direct_lookup(&self.db, &lookup)
            .await
            .expect("lookup")
    }

    /// What the default space holds, in alphabetical order.
    pub async fn contents(&self) -> Vec<String> {
        let mut contents: Vec<String> = self
            .memories()
            .await
            .into_iter()
            .map(|memory| memory.content)
            .collect();
        contents.sort();
        contents
    }

    pub async fn stats(&self) -> SpaceStats {
        repo::stats(&self.db, &space()).await.expect("stats")
    }

    pub async fn shutdown(self) {
        self.client.cancel().await.expect("shut down");
        self.server.await.expect("join").expect("serve");
    }
}

/// The space the harness's server was started with.
pub fn space() -> SpaceName {
    "default".parse().expect("valid slug")
}

/// Vectors from a fixed vocabulary: one axis per keyword the text contains.
///
/// Two texts naming the same keywords come out identical, which is exactly
/// what the near-duplicate gate is for — the same claim in different words.
/// Real embeddings would say something similar and much less legibly.
#[derive(Debug, Clone, Copy)]
pub struct KeywordEmbedder;

/// The keywords with an axis of their own; anything else lands on the last.
const VOCABULARY: [&str; 3] = ["rust", "python", "kitchen"];

impl Embedder for KeywordEmbedder {
    fn dim(&self) -> usize {
        migrate::EMBEDDING_DIM
    }

    fn model_id(&self) -> &str {
        "test-keyword"
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(passages.iter().map(|text| vector(text)).collect())
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(vector(query))
    }
}

/// One axis per vocabulary word present, never all-zero — cosine distance to a
/// zero vector is undefined, and HNSW says so at write time.
fn vector(text: &str) -> Vec<f32> {
    let text = text.to_lowercase();
    let mut vector = vec![0.0; migrate::EMBEDDING_DIM];
    for (axis, word) in VOCABULARY.iter().enumerate() {
        if text.contains(word) {
            vector[axis] = 1.0;
        }
    }
    if vector.iter().all(|weight| *weight == 0.0) {
        vector[VOCABULARY.len()] = 1.0;
    }
    vector
}

/// Vectors at a chosen angle on one plane, so a test can put two memories at
/// an exact cosine similarity.
///
/// [`KeywordEmbedder`] cannot: one-hot axes over a three-word vocabulary only
/// ever produce 1.0, 0.707, 0.577 or 0.0, and every band `consolidate` reads
/// sits strictly between 0.75 and 1.0. Nothing built on it can seed a
/// near-duplicate that the write gate does not also block, let alone a
/// contradiction candidate.
#[derive(Debug, Clone, Copy)]
pub struct AngleEmbedder;

/// Where each marker word sits, in degrees on the first two axes.
///
/// 20° apart is cosine 0.94 — a cluster, and still under the 0.95 write gate,
/// 30° is 0.87: a contradiction candidate and not a cluster. 40° is 0.77, and
/// two claims 20° either side of a third form one chained cluster whose ends
/// do not resemble each other, which is what `min_similarity` reports.
const ANGLES: [(&str, f64); 4] = [
    ("black", 0.0),
    ("blackfmt", 20.0),
    ("ruff", 30.0),
    ("blake", 40.0),
];

impl Embedder for AngleEmbedder {
    fn dim(&self) -> usize {
        migrate::EMBEDDING_DIM
    }

    fn model_id(&self) -> &str {
        "test-angle"
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(passages.iter().map(|text| angled(text)).collect())
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(angled(query))
    }
}

/// The vector for the first marker word `text` contains; 90° — orthogonal to
/// every marker — for text carrying none.
fn angled(text: &str) -> Vec<f32> {
    let lowered = text.to_lowercase();
    let degrees = lowered
        .split_whitespace()
        .find_map(|word| {
            ANGLES
                .iter()
                .find(|(marker, _)| word == *marker)
                .map(|(_, degrees)| *degrees)
        })
        .unwrap_or(90.0);
    let radians = degrees.to_radians();
    let mut vector = vec![0.0; migrate::EMBEDDING_DIM];
    vector[0] = radians.cos() as f32;
    vector[1] = radians.sin() as f32;
    vector
}

/// Backdate a row and set the counters `stale_contexts` reads.
///
/// `last_accessed`, `strength` and `access_count` are the engine's to keep, so
/// no sequence of tool calls produces a note that has been in use for months —
/// which is the only state the stale arm has an opinion about.
pub async fn age(db: &Db, id: &str, days: i64, strength: f64, accesses: i64) {
    db.query(
        "UPDATE type::record('memory', $id)
         SET last_accessed = time::now() - duration::from_secs($idle),
             strength = $strength, access_count = $accesses",
    )
    .bind(("id", id.to_owned()))
    .bind(("idle", days * 86_400))
    .bind(("strength", strength))
    .bind(("accesses", accesses))
    .await
    .expect("age the row")
    .check()
    .expect("statements");
}

/// The ids in one array of a `remember` diff.
pub fn ids(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .expect("an array of ids")
        .iter()
        .map(|id| id.as_str().expect("an id"))
        .collect()
}

/// The ids a `forget` reported as matched, in the order it listed them.
pub fn match_ids(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .expect("an array of matches")
        .iter()
        .map(|found| found["id"].as_str().expect("an id"))
        .collect()
}

/// The refusal an argument error is expected to produce.
pub fn refusal(error: &ServiceError, expected: &str) -> bool {
    matches!(error, ServiceError::McpError(data)
        if data.code == ErrorCode::INVALID_PARAMS && data.message.contains(expected))
}

/// Every hit of a `recall`, in the order it ranked them.
pub fn hits(found: &Value) -> &Vec<Value> {
    found["hits"].as_array().expect("an array of hits")
}

/// The section headings of a `context` block, in the order they appear.
pub fn headings(block: &str) -> Vec<&str> {
    block
        .lines()
        .filter(|line| line.starts_with("## "))
        .collect()
}

/// What each hit says, in rank order.
pub fn hit_contents(found: &Value) -> Vec<&str> {
    hits(found)
        .iter()
        .map(|hit| hit["content"].as_str().expect("content"))
        .collect()
}
