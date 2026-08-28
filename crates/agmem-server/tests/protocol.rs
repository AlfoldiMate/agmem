//! The MCP surface, driven by a real rmcp client in this process.
//!
//! Client and server sit on the two ends of a `tokio::io::duplex` pipe, which
//! is the SDK's own test pattern (design §7.3): the full JSON-RPC framing,
//! the initialize handshake and the tool dispatch all run, without a child
//! process or a socket. What a tool *did* is then checked through the store's
//! own read API, so these tests span the whole path a real call takes.
//!
//! The snapshots are the point. Nothing in this stack breaks loudly — an rmcp
//! or schemars upgrade that changes how a tool schema is generated produces a
//! server that still starts, still lists tools, and quietly describes them
//! differently to every agent. The snapshot is what turns that into a failing
//! test.

use std::sync::Arc;

use agmem_core::{MemoryRecord, Source, SpaceName};
use agmem_embed::{EmbedError, Embedder, NoopEmbedder};
use agmem_server::config::Cli;
use agmem_server::service::AgmemService;
use agmem_store::db::Db;
use agmem_store::migrate;
use agmem_store::repo::{self, Liveness, Lookup, SpaceStats};
use clap::Parser as _;
use rmcp::RoleClient;
use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, CallToolResult, ErrorCode};
use rmcp::service::{RunningService, ServiceError};
use serde_json::{Value, json};

/// A server, the client talking to it, and the store behind both.
struct Harness {
    client: RunningService<RoleClient, ()>,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
    db: Db,
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
    async fn start(embedder: Arc<dyn Embedder>) -> Self {
        let data = tempfile::tempdir().expect("tempdir");
        let config = Cli::try_parse_from([
            "agmem",
            "--db",
            "mem://",
            // Only `embedder::build` reads this; the service is handed the
            // backend a test wants directly.
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
        migrate::ensure(&db).await.expect("migrate");
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
    async fn call(
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

    /// One successful `remember`, as the structured result an agent reads.
    async fn remember(&self, arguments: Value) -> Value {
        let result = self.call("remember", arguments).await.expect("remember");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("remember answers with structured content, not just text")
    }

    /// One successful `recall`, as the structured result an agent reads.
    async fn recall(&self, arguments: Value) -> Value {
        let result = self.call("recall", arguments).await.expect("recall");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("recall answers with structured content, not just text")
    }

    /// One successful `inspect`, as the structured result an agent reads.
    async fn inspect(&self, reference: &str) -> Value {
        let result = self
            .call("inspect", json!({ "ref": reference }))
            .await
            .expect("inspect");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("inspect answers with structured content, not just text")
    }

    /// Every memory the default space holds, closed ones included.
    async fn memories(&self) -> Vec<MemoryRecord> {
        let mut lookup = Lookup::new(vec![space()]);
        lookup.liveness = Liveness::Any;
        repo::direct_lookup(&self.db, &lookup)
            .await
            .expect("lookup")
    }

    /// What the default space holds, in alphabetical order.
    async fn contents(&self) -> Vec<String> {
        let mut contents: Vec<String> = self
            .memories()
            .await
            .into_iter()
            .map(|memory| memory.content)
            .collect();
        contents.sort();
        contents
    }

    async fn stats(&self) -> SpaceStats {
        repo::stats(&self.db, &space()).await.expect("stats")
    }

    async fn shutdown(self) {
        self.client.cancel().await.expect("shut down");
        self.server.await.expect("join").expect("serve");
    }
}

/// The space the harness's server was started with.
fn space() -> SpaceName {
    "default".parse().expect("valid slug")
}

/// Vectors from a fixed vocabulary: one axis per keyword the text contains.
///
/// Two texts naming the same keywords come out identical, which is exactly
/// what the near-duplicate gate is for — the same claim in different words.
/// Real embeddings would say something similar and much less legibly.
#[derive(Debug, Clone, Copy)]
struct KeywordEmbedder;

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

/// The ids in one array of a `remember` diff.
fn ids(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .expect("an array of ids")
        .iter()
        .map(|id| id.as_str().expect("an id"))
        .collect()
}

/// Every hit of a `recall`, in the order it ranked them.
fn hits(found: &Value) -> &Vec<Value> {
    found["hits"].as_array().expect("an array of hits")
}

/// What each hit says, in rank order.
fn hit_contents(found: &Value) -> Vec<&str> {
    hits(found)
        .iter()
        .map(|hit| hit["content"].as_str().expect("content"))
        .collect()
}

#[tokio::test]
async fn initialize_announces_agmem_and_its_tool_capability() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let info = agmem
        .client
        .peer_info()
        .expect("negotiated server info")
        .clone();

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
    agmem.shutdown().await;
}

#[tokio::test]
async fn list_tools_matches_the_recorded_surface() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let tools = agmem.client.list_tools(None).await.expect("list_tools");

    // Every tool issue re-records this: it is the contract each agent reads.
    insta::assert_json_snapshot!("list_tools", &tools.tools);
    assert!(
        tools.next_cursor.is_none(),
        "the whole surface fits one page"
    );

    agmem.shutdown().await;
}

#[tokio::test]
async fn an_unknown_tool_is_refused_rather_than_ignored() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let error = agmem
        .client
        .call_tool(rmcp::model::CallToolRequestParams::new("no_such_tool"))
        .await
        .expect_err("an unrouted name must fail");

    // rmcp answers with a JSON-RPC error rather than an empty success, and
    // does not echo the name back — so the code is what a client can act on.
    assert!(
        matches!(&error, ServiceError::McpError(data) if data.code == ErrorCode::INVALID_PARAMS),
        "an unrouted name must come back as a protocol error: {error}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn remember_stores_a_batch_and_provenances_it_to_the_episode() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let diff = agmem
        .remember(json!({
            "memories": [
                {
                    "content": "The user prefers Rust over Python for CLI tools",
                    "entities": ["user"],
                    "tags": ["identity"]
                },
                {
                    "content": "Cargo builds fail when the disk cache is cold",
                    "kind": "lesson"
                }
            ],
            "episode": {
                "content": "I'd rather write CLIs in Rust than Python.\n\nAlso cargo died on a cold cache again.",
                "session": "s-1"
            }
        }))
        .await;

    assert_eq!(ids(&diff["created"]).len(), 2, "{diff}");
    assert!(ids(&diff["superseded"]).is_empty(), "{diff}");
    assert!(
        diff["duplicates"].as_array().expect("array").is_empty(),
        "{diff}"
    );
    let episode = diff["episode"].as_str().expect("the episode id comes back");

    let memories = agmem.memories().await;
    assert!(
        memories.iter().all(|memory| matches!(
            &memory.source,
            Source::Episode { episode: from } if from.as_str() == episode
        )),
        "every fact written in the same call is provenanced to the episode: {:?}",
        memories.iter().map(|m| &m.source).collect::<Vec<_>>()
    );

    let lesson = memories
        .iter()
        .find(|memory| memory.content.starts_with("Cargo"))
        .expect("the lesson");
    assert_eq!(lesson.kind.as_str(), "lesson");
    assert_eq!(
        lesson.decay_class.as_str(),
        "slow",
        "an unset decay class comes from the kind"
    );
    let fact = memories
        .iter()
        .find(|memory| memory.content.starts_with("The user"))
        .expect("the fact");
    assert_eq!(
        (fact.kind.as_str(), fact.decay_class.as_str()),
        ("fact", "normal")
    );
    assert_eq!(fact.tags, ["identity"]);
    assert_eq!(fact.entities, ["user"]);

    let stats = agmem.stats().await;
    assert_eq!(
        (stats.live, stats.episodes, stats.chunks),
        (2, 1, 1),
        "the episode is stored verbatim and chunked for retrieval"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_restatement_comes_back_as_the_id_that_already_holds_it() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let first = agmem
        .remember(json!({ "memories": [{ "content": "The user prefers Rust" }] }))
        .await;
    let stored = ids(&first["created"])[0].to_owned();

    let again = agmem
        .remember(json!({
            "memories": [
                // The same claim in other words — only the vector says so.
                { "content": "Rust is what the user reaches for" },
                // A claim about something else entirely.
                { "content": "The kitchen tap drips at night" }
            ]
        }))
        .await;

    let duplicates = again["duplicates"].as_array().expect("array");
    assert_eq!(duplicates.len(), 1, "{again}");
    assert_eq!(
        duplicates[0]["id"].as_str(),
        Some(stored.as_str()),
        "a duplicate names the memory that already holds the claim"
    );
    assert_eq!(
        duplicates[0]["of"].as_u64(),
        Some(0),
        "and which entry of the request it came from"
    );
    assert!(
        duplicates[0]["similarity"].as_f64().expect("similarity") >= 0.95,
        "with how close a match it was: {}",
        duplicates[0]["similarity"]
    );
    assert_eq!(ids(&again["created"]).len(), 1, "{again}");
    assert_eq!(
        agmem.contents().await,
        ["The kitchen tap drips at night", "The user prefers Rust"],
        "the restatement was reported, not stored, and the original is untouched"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn without_an_embedder_the_exact_gate_still_holds() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let first = agmem
        .remember(json!({ "memories": [{ "content": "The user prefers Rust" }] }))
        .await;
    let stored = ids(&first["created"])[0].to_owned();

    // Normalization folds case and whitespace, so this is the same claim; the
    // near-dup gate has no vectors to work with and stays out of it.
    let again = agmem
        .remember(json!({ "memories": [{ "content": "the   USER\nprefers  rust" }] }))
        .await;

    assert!(ids(&again["created"]).is_empty(), "{again}");
    let duplicates = again["duplicates"].as_array().expect("array");
    assert_eq!(duplicates.len(), 1, "{again}");
    assert_eq!(duplicates[0]["id"].as_str(), Some(stored.as_str()));
    assert_eq!(
        duplicates[0]["similarity"].as_f64(),
        Some(1.0),
        "the same text is a similarity of 1 by construction"
    );
    assert_eq!(
        agmem.contents().await,
        ["The user prefers Rust"],
        "BM25-only mode still writes, and still refuses to write twice"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_correction_closes_the_memory_it_supersedes() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let first = agmem
        .remember(json!({ "memories": [{ "content": "The user prefers Rust for CLI tools" }] }))
        .await;
    let old = ids(&first["created"])[0].to_owned();

    // Close enough to the original that the near-dup gate would block it —
    // which is the point: an explicit correction is a decision already made.
    let corrected = agmem
        .remember(json!({
            "memories": [{
                "content": "The user now prefers Rust for everything",
                "supersedes": old,
                "valid_from": "2026-08-28T09:00:00Z"
            }]
        }))
        .await;

    assert!(
        corrected["duplicates"]
            .as_array()
            .expect("array")
            .is_empty(),
        "a memory carrying `supersedes` skips the near-dup gate: {corrected}"
    );
    assert_eq!(ids(&corrected["superseded"]), [old.as_str()]);
    let new = ids(&corrected["created"])[0].to_owned();

    let memories = agmem.memories().await;
    let closed = memories
        .iter()
        .find(|memory| memory.id.as_str() == old)
        .expect("the closed memory is still readable");
    assert_eq!(
        (
            closed.invalid_reason.map(|reason| reason.as_str()),
            closed.superseded_by.as_ref().map(|id| id.as_str()),
            closed.invalid_at.map(|at| at.to_string())
        ),
        (
            Some("superseded"),
            Some(new.as_str()),
            Some("2026-08-28T09:00:00Z".to_owned())
        ),
        "it is dated at the moment the correction took over, and points forward"
    );
    assert_eq!(
        repo::direct_lookup(&agmem.db, &Lookup::new(vec![space()]))
            .await
            .expect("lookup")
            .into_iter()
            .map(|memory| memory.content)
            .collect::<Vec<_>>(),
        ["The user now prefers Rust for everything"],
        "and only one claim is live"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_request_that_cannot_be_stored_names_what_is_wrong() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let cases = [
        (json!({ "memories": [] }), "nothing to remember"),
        (
            json!({ "memories": [{ "content": "  \n " }] }),
            "memories[0].content",
        ),
        (
            json!({ "memories": [{ "content": "a claim", "valid_from": "last tuesday" }] }),
            "memories[0].valid_from",
        ),
        (
            json!({ "memories": [{ "content": "a claim", "supersedes": "nonsense" }] }),
            "memories[0].supersedes",
        ),
        (
            json!({ "memories": [{ "content": "a claim", "supersedes": "01M145SMNET1XRYA713EWAQTD3" }] }),
            "does not exist in space",
        ),
        (
            json!({ "space": "Not A Slug", "memories": [{ "content": "a claim" }] }),
            "invalid space name",
        ),
    ];

    for (arguments, expected) in cases {
        let error = agmem
            .call("remember", arguments.clone())
            .await
            .expect_err("the call must be refused");
        assert!(
            matches!(&error, ServiceError::McpError(data)
                if data.code == ErrorCode::INVALID_PARAMS && data.message.contains(expected)),
            "{arguments} should be refused naming {expected:?}, got: {error}"
        );
    }

    assert!(
        agmem.memories().await.is_empty(),
        "a refused call writes nothing at all"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn recall_fuses_claims_with_the_text_they_came_from_and_says_why() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let stored = agmem
        .remember(json!({
            "memories": [
                { "content": "The user prefers Rust over Python for CLI tools" },
                { "content": "The kitchen tap drips at night" }
            ],
            "episode": { "content": "I'd rather write CLIs in Rust than Python." }
        }))
        .await;
    let episode = stored["episode"].as_str().expect("the episode id");

    let found = agmem
        .recall(json!({ "query": "which language does the user reach for in Rust projects" }))
        .await;

    assert_eq!(
        found["spaces"],
        json!(["default", "user"]),
        "an unset space searches this project and the person behind it"
    );
    assert_eq!(
        hits(&found).len(),
        3,
        "both claims and the episode's one chunk compete in a single order: {found}"
    );
    assert_eq!(
        hit_contents(&found).last(),
        Some(&"The kitchen tap drips at night"),
        "what the query is not about ranks last: {found}"
    );

    let best = &hits(&found)[0];
    assert_eq!(
        best["signals"]["rrf_normalized"].as_f64(),
        Some(1.0),
        "the strongest retrieval hit normalises to 1"
    );
    for hit in hits(&found) {
        let signals = &hit["signals"];
        for signal in ["rrf", "rrf_normalized", "retention", "importance"] {
            assert!(
                signals[signal].is_f64(),
                "every hit shows why it surfaced, {signal} included: {hit}"
            );
        }
        assert!(hit["score"].is_f64(), "{hit}");
        assert_eq!(
            hit["source"].as_str(),
            Some(format!("episode:{episode}").as_str()),
            "a claim points back at the verbatim text it was distilled from, \
             and a chunk at the episode it slices: {hit}"
        );
    }

    let verbatim = hits(&found)
        .iter()
        .find(|hit| hit["kind"] == "episode")
        .expect("the verbatim chunk is in the pool");
    assert_eq!(
        verbatim["content"].as_str(),
        Some("I'd rather write CLIs in Rust than Python."),
        "unedited, and marked as ground truth rather than a claim"
    );
    assert!(
        verbatim["valid_from"].is_null(),
        "verbatim text has no validity window to report: {verbatim}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn recall_unions_the_current_space_with_the_user_space() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({ "memories": [{ "content": "This project pins surrealdb to 3.x" }] }))
        .await;
    agmem
        .remember(json!({
            "space": "user",
            "memories": [{ "content": "The user works in Europe/Budapest" }]
        }))
        .await;

    // No query at all: the filters-only path, so the order is decay and
    // importance rather than anything a search engine decided.
    let both = agmem.recall(json!({})).await;
    assert_eq!(both["spaces"], json!(["default", "user"]));
    let mut contents = hit_contents(&both);
    contents.sort_unstable();
    assert_eq!(
        contents,
        [
            "The user works in Europe/Budapest",
            "This project pins surrealdb to 3.x"
        ],
        "memory that follows the person is recalled alongside the project's"
    );

    for (space, expected) in [
        ("current", "This project pins surrealdb to 3.x"),
        ("user", "The user works in Europe/Budapest"),
    ] {
        let scoped = agmem.recall(json!({ "space": space })).await;
        assert_eq!(hit_contents(&scoped), [expected], "space {space}: {scoped}");
    }

    let all = agmem.recall(json!({ "space": "all" })).await;
    assert_eq!(
        all["spaces"],
        json!(["default", "user"]),
        "`all` expands through the registry every write registers"
    );
    assert_eq!(hits(&all).len(), 2);
    agmem.shutdown().await;
}

#[tokio::test]
async fn as_of_returns_the_claim_that_was_live_then() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let first = agmem
        .remember(json!({
            "memories": [{
                "content": "The user deploys from a laptop",
                "valid_from": "2026-01-01T00:00:00Z"
            }]
        }))
        .await;
    let old = ids(&first["created"])[0].to_owned();
    let second = agmem
        .remember(json!({
            "memories": [{
                "content": "The user deploys from CI",
                "supersedes": old,
                "valid_from": "2026-06-01T00:00:00Z"
            }]
        }))
        .await;
    let new = ids(&second["created"])[0].to_owned();

    assert_eq!(
        hit_contents(&agmem.recall(json!({})).await),
        ["The user deploys from CI"],
        "a recall answers with what is true now"
    );

    let then = agmem
        .recall(json!({ "as_of": "2026-03-01T00:00:00Z" }))
        .await;
    assert_eq!(
        hit_contents(&then),
        ["The user deploys from a laptop"],
        "and with what was true then, not what replaced it: {then}"
    );
    let superseded = &hits(&then)[0];
    assert_eq!(
        (
            superseded["id"].as_str(),
            superseded["invalid_at"].as_str(),
            superseded["invalid_reason"].as_str(),
            superseded["superseded_by"].as_str(),
        ),
        (
            Some(old.as_str()),
            Some("2026-06-01T00:00:00Z"),
            Some("superseded"),
            Some(new.as_str()),
        ),
        "dated, and pointing at what took over: {superseded}"
    );

    let everything = agmem.recall(json!({ "include_invalidated": true })).await;
    assert_eq!(
        hits(&everything).len(),
        2,
        "asking for history gets both: {everything}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_filters_only_recall_ranks_on_decay_alone() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    agmem
        .remember(json!({
            "memories": [
                { "content": "Never force-push to main", "kind": "instruction" },
                { "content": "The build breaks on a cold cargo cache", "kind": "lesson" },
                { "content": "The user prefers Rust", "tags": ["identity"] }
            ]
        }))
        .await;

    let rules = agmem.recall(json!({ "kinds": ["instruction"] })).await;
    assert_eq!(hit_contents(&rules), ["Never force-push to main"]);
    let only = &hits(&rules)[0];
    assert_eq!(
        (
            only["signals"]["rrf"].as_f64(),
            only["signals"]["rrf_normalized"].as_f64(),
            only["signals"]["importance"].as_f64()
        ),
        (Some(0.0), Some(0.0), Some(1.0)),
        "nothing was retrieved — the filter selected it and the pinned class \
         ranked it: {only}"
    );
    assert_eq!(only["kind"].as_str(), Some("instruction"));
    assert_eq!(only["source"].as_str(), Some("agent"));

    assert_eq!(
        hit_contents(&agmem.recall(json!({ "tags": ["identity"] })).await),
        ["The user prefers Rust"]
    );
    assert_eq!(
        hits(&agmem.recall(json!({ "entities": ["nobody"] })).await).len(),
        0,
        "a filter nothing matches is an empty answer, not an error"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_returned_memory_is_reinforced_and_a_k_past_the_ceiling_is_refused() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({ "memories": [{ "content": "The user prefers Rust" }] }))
        .await;

    let found = agmem.recall(json!({ "k": 1 })).await;
    assert_eq!(hits(&found).len(), 1);
    let memory = agmem.memories().await.remove(0);
    assert_eq!(
        (memory.strength, memory.access_count),
        (2.0, 1),
        "being recalled is what keeps a memory alive"
    );

    for (arguments, expected) in [
        (json!({ "k": 0 }), "k must be between 1 and 50"),
        (json!({ "k": 200 }), "k must be between 1 and 50"),
        (json!({ "space": "Not A Slug" }), "space:"),
        (json!({ "as_of": "last tuesday" }), "as_of:"),
    ] {
        let error = agmem
            .call("recall", arguments.clone())
            .await
            .expect_err("the call must be refused");
        assert!(
            matches!(&error, ServiceError::McpError(data)
                if data.code == ErrorCode::INVALID_PARAMS && data.message.contains(expected)),
            "{arguments} should be refused naming {expected:?}, got: {error}"
        );
    }

    assert_eq!(
        agmem.memories().await.remove(0).access_count,
        1,
        "a refused recall reinforces nothing"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn inspect_walks_a_chain_of_two_corrections_oldest_first() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let mut links = Vec::new();
    for (content, valid_from) in [
        ("The user deploys from a laptop", "2026-01-01T00:00:00Z"),
        ("The user deploys from CI", "2026-03-01T00:00:00Z"),
        ("The user deploys from a robot", "2026-06-01T00:00:00Z"),
    ] {
        let mut memory = json!({ "content": content, "valid_from": valid_from });
        if let Some(previous) = links.last() {
            memory["supersedes"] = json!(previous);
        }
        let diff = agmem.remember(json!({ "memories": [memory] })).await;
        links.push(ids(&diff["created"])[0].to_owned());
    }

    // From the newest link, sent the way `recall` hands ids out: bare.
    let found = agmem.inspect(&links[2]).await;
    assert_eq!(
        found["ref"].as_str(),
        Some(format!("memory:{}", links[2]).as_str())
    );
    let answer = &found["found"];
    assert_eq!(answer["kind"].as_str(), Some("memory"));
    assert_eq!(answer["memory"]["id"].as_str(), Some(links[2].as_str()));
    assert_eq!(
        answer["chain"]
            .as_array()
            .expect("chain")
            .iter()
            .map(|link| (
                link["content"].as_str().expect("content"),
                link["invalid_reason"].as_str()
            ))
            .collect::<Vec<_>>(),
        [
            ("The user deploys from a laptop", Some("superseded")),
            ("The user deploys from CI", Some("superseded")),
            ("The user deploys from a robot", None),
        ],
        "oldest belief first, each dated, only the last one live: {answer}"
    );

    // The same chain from the middle link — a walk, not a lookup.
    let from_middle = agmem.inspect(&format!("memory:{}", links[1])).await;
    assert_eq!(
        from_middle["found"]["chain"], answer["chain"],
        "any link of a chain answers with the whole chain"
    );
    assert_eq!(
        from_middle["found"]["memory"]["id"].as_str(),
        Some(links[1].as_str()),
        "but `memory` is the one that was asked about"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_claim_links_back_to_the_text_it_was_distilled_from() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let verbatim = "I'd rather write CLIs in Rust than Python.\n\nAlso cargo died on a cold cache.";
    let diff = agmem
        .remember(json!({
            "memories": [
                { "content": "The user prefers Rust over Python for CLI tools" },
                { "content": "The build breaks on a cold cargo cache", "kind": "lesson" }
            ],
            "episode": { "content": verbatim, "session": "s-1" }
        }))
        .await;
    let claim = ids(&diff["created"])[0].to_owned();
    let episode = diff["episode"].as_str().expect("episode id").to_owned();

    let found = agmem.inspect(&claim).await;
    let quotable = &found["found"]["episode"];
    assert_eq!(
        quotable["content"].as_str(),
        Some(verbatim),
        "the claim carries its ground truth, unedited: {found}"
    );
    assert_eq!(quotable["session"].as_str(), Some("s-1"));
    assert_eq!(
        found["found"]["memory"]["source"].as_str(),
        Some(format!("episode:{episode}").as_str()),
        "and `source` is the reference that got us here"
    );

    let text = agmem.inspect(&format!("episode:{episode}")).await;
    let answer = &text["found"];
    assert_eq!(answer["kind"].as_str(), Some("episode"));
    assert_eq!(
        answer["chunks"]
            .as_array()
            .expect("chunks")
            .iter()
            .map(|chunk| chunk["position"].as_u64().expect("position"))
            .collect::<Vec<_>>(),
        [0],
        "both paragraphs fit one slice: {answer}"
    );
    let mut derived: Vec<&str> = answer["derived"]
        .as_array()
        .expect("derived")
        .iter()
        .map(|memory| memory["content"].as_str().expect("content"))
        .collect();
    derived.sort_unstable();
    assert_eq!(
        derived,
        [
            "The build breaks on a cold cargo cache",
            "The user prefers Rust over Python for CLI tools"
        ],
        "every claim the same call distilled from it"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn inspect_reports_a_subject_and_what_each_space_holds() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let first = agmem
        .remember(json!({
            "memories": [{
                "content": "The user deploys from a laptop",
                "entities": ["user"],
                "valid_from": "2026-01-01T00:00:00Z"
            }]
        }))
        .await;
    agmem
        .remember(json!({
            "memories": [{
                "content": "The user deploys from CI",
                "entities": ["user"],
                "supersedes": ids(&first["created"])[0],
                "valid_from": "2026-06-01T00:00:00Z"
            }]
        }))
        .await;
    agmem
        .remember(json!({
            "space": "user",
            "memories": [{
                "content": "The user answers in English",
                "kind": "instruction",
                "entities": ["user"]
            }]
        }))
        .await;

    let subject = agmem.inspect("entity:user").await;
    assert_eq!(subject["found"]["entity"].as_str(), Some("user"));
    let mut said: Vec<&str> = subject["found"]["memories"]
        .as_array()
        .expect("memories")
        .iter()
        .map(|memory| memory["content"].as_str().expect("content"))
        .collect();
    said.sort_unstable();
    assert_eq!(
        said,
        [
            "The user answers in English",
            "The user deploys from CI",
            "The user deploys from a laptop"
        ],
        "everything ever said about the subject, across both spaces, corrected \
         claims included: {subject}"
    );

    let health = agmem.inspect("stats").await;
    assert_eq!(
        health["spaces"],
        json!(["default", "user"]),
        "`stats` is a question about the store, so it defaults to every space"
    );
    let counts = health["found"]["counts"].as_array().expect("counts");
    let project = &counts[0];
    assert_eq!(
        (
            project["space"].as_str(),
            project["memories"].as_u64(),
            project["live"].as_u64(),
            project["invalidated"].as_u64()
        ),
        (Some("default"), Some(2), Some(1), Some(1)),
        "a corrected claim is counted, not lost: {project}"
    );
    assert_eq!(
        counts[1]["live_by_kind"],
        json!([{ "kind": "instruction", "count": 1 }]),
        "and the user space holds the standing rule: {}",
        counts[1]
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn an_unanswerable_ref_says_what_the_grammar_is() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let stored = agmem
        .remember(json!({
            "space": "user",
            "memories": [{ "content": "The user answers in English" }]
        }))
        .await;
    let elsewhere = ids(&stored["created"])[0].to_owned();

    for (reference, space, expected) in [
        ("who knows", None, "ref must be"),
        ("memory:nonsense", None, "ref must be"),
        ("entity:", None, "ref must be"),
        ("01M145SMNET1XRYA713EWAQTD3", None, "no memory"),
        ("episode:01M145SMNET1XRYA713EWAQTD3", None, "no episode"),
        // The id is real, but not in the space this call looked in.
        (elsewhere.as_str(), Some("current"), "no memory"),
    ] {
        let mut arguments = json!({ "ref": reference });
        if let Some(space) = space {
            arguments["space"] = json!(space);
        }
        let error = agmem
            .call("inspect", arguments.clone())
            .await
            .expect_err("the call must be refused");
        assert!(
            matches!(&error, ServiceError::McpError(data)
                if data.code == ErrorCode::INVALID_PARAMS && data.message.contains(expected)),
            "{arguments} should be refused naming {expected:?}, got: {error}"
        );
    }

    assert_eq!(
        agmem.inspect(&elsewhere).await["found"]["memory"]["space"].as_str(),
        Some("user"),
        "and the default pair of spaces finds it"
    );
    agmem.shutdown().await;
}
