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
use agmem_server::config::{Cli, ToolDescriptions};
use agmem_server::service::AgmemService;
use agmem_store::db::Db;
use agmem_store::migrate;
use agmem_store::repo::{self, Liveness, Lookup, SpaceStats};
use clap::Parser as _;
use rmcp::RoleClient;
use rmcp::ServiceExt as _;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, ErrorCode, GetPromptRequestParams, Role,
};
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
        Self::start_with(embedder, ToolDescriptions::default()).await
    }

    /// The same, serving `tool_desc` instead of agmem's own wording.
    ///
    /// The overrides are set on the resolved config rather than left to
    /// `AGMEM_TOOL_DESC_*`: `Cli::resolve` reads the real environment, and a
    /// developer who has one of those exported would otherwise fail the
    /// `list_tools` snapshot with a diff that has nothing to do with their
    /// change.
    async fn start_with(embedder: Arc<dyn Embedder>, tool_desc: ToolDescriptions) -> Self {
        let data = tempfile::tempdir().expect("tempdir");
        let mut config = Cli::try_parse_from([
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
        config.tool_desc = tool_desc;
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

    /// One successful `reflect`, as the structured result an agent reads.
    async fn reflect(&self, arguments: Value) -> Value {
        let result = self.call("reflect", arguments).await.expect("reflect");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("reflect answers with structured content, not just text")
    }

    /// One successful `recall`, as the structured result an agent reads.
    async fn recall(&self, arguments: Value) -> Value {
        let result = self.call("recall", arguments).await.expect("recall");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("recall answers with structured content, not just text")
    }

    /// One successful `consolidate`, as the structured result an agent reads.
    async fn consolidate(&self, arguments: Value) -> Value {
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
    async fn context(&self, arguments: Value) -> String {
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

    /// One successful `forget`, as the structured result an agent reads.
    async fn forget(&self, arguments: Value) -> Value {
        let result = self.call("forget", arguments).await.expect("forget");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        result
            .structured_content
            .expect("forget answers with structured content, not just text")
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

/// Vectors at a chosen angle on one plane, so a test can put two memories at
/// an exact cosine similarity.
///
/// [`KeywordEmbedder`] cannot: one-hot axes over a three-word vocabulary only
/// ever produce 1.0, 0.707, 0.577 or 0.0, and every band `consolidate` reads
/// sits strictly between 0.75 and 1.0. Nothing built on it can seed a
/// near-duplicate that the write gate does not also block, let alone a
/// contradiction candidate.
#[derive(Debug, Clone, Copy)]
struct AngleEmbedder;

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
async fn age(db: &Db, id: &str, days: i64, strength: f64, accesses: i64) {
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
fn ids(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .expect("an array of ids")
        .iter()
        .map(|id| id.as_str().expect("an id"))
        .collect()
}

/// The ids a `forget` reported as matched, in the order it listed them.
fn match_ids(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .expect("an array of matches")
        .iter()
        .map(|found| found["id"].as_str().expect("an id"))
        .collect()
}

/// The refusal an argument error is expected to produce.
fn refusal(error: &ServiceError, expected: &str) -> bool {
    matches!(error, ServiceError::McpError(data)
        if data.code == ErrorCode::INVALID_PARAMS && data.message.contains(expected))
}

/// Every hit of a `recall`, in the order it ranked them.
fn hits(found: &Value) -> &Vec<Value> {
    found["hits"].as_array().expect("an array of hits")
}

/// The section headings of a `context` block, in the order they appear.
fn headings(block: &str) -> Vec<&str> {
    block
        .lines()
        .filter(|line| line.starts_with("## "))
        .collect()
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
    assert!(
        info.capabilities.prompts.is_some(),
        "and the prompt capability — a client that is not told about prompts \
         never asks, and the rituals simply do not exist for it"
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
async fn list_prompts_matches_the_recorded_rituals() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let prompts = agmem.client.list_prompts(None).await.expect("list_prompts");

    // Same reasoning as the tool snapshot: this text is a contract with every
    // agent, and nothing about changing it fails loudly on its own.
    insta::assert_json_snapshot!("list_prompts", &prompts.prompts);

    agmem.shutdown().await;
}

#[tokio::test]
async fn a_ritual_renders_one_instruction_for_the_model() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;

    for (name, must_name) in [
        ("recall_first", "`context`"),
        ("checkpoint", "`supersedes`"),
    ] {
        let result = agmem
            .client
            .get_prompt(GetPromptRequestParams::new(name))
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));

        let [message] = &result.messages[..] else {
            panic!("{name} is one turn in the conversation, got {result:?}");
        };
        assert_eq!(
            message.role,
            Role::User,
            "a ritual is what the person asking says next, not something the \
             model is made to have already said"
        );
        let ContentBlock::Text(text) = &message.content else {
            panic!("{name} is text, got {:?}", message.content);
        };
        assert!(
            text.text.contains(must_name),
            "{name} must name the tool it is a ritual for: {}",
            text.text
        );
        assert!(
            result.description.is_some(),
            "{name} answers with what it is, for a client that shows it"
        );
    }

    agmem.shutdown().await;
}

#[tokio::test]
async fn a_ritual_takes_the_focus_it_was_given_and_runs_without_one() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;

    let arguments = json!({ "focus": "the auth refactor" })
        .as_object()
        .expect("an object")
        .clone();
    let aimed = agmem
        .client
        .get_prompt(GetPromptRequestParams::new("recall_first").with_arguments(arguments))
        .await
        .expect("recall_first with a focus");
    assert!(
        text_of(&aimed).contains("the auth refactor"),
        "a focus the caller typed has to reach the instruction: {aimed:?}"
    );

    // The argument is optional in the schema, and optional has to mean the
    // call works with the key absent — not merely with it null.
    let plain = agmem
        .client
        .get_prompt(GetPromptRequestParams::new("recall_first"))
        .await
        .expect("recall_first with no arguments at all");
    assert!(!text_of(&plain).contains("query:"), "{plain:?}");

    agmem.shutdown().await;
}

#[tokio::test]
async fn an_unknown_ritual_is_refused_rather_than_ignored() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let error = agmem
        .client
        .get_prompt(GetPromptRequestParams::new("no_such_ritual"))
        .await
        .expect_err("an unrouted name must fail");

    let ServiceError::McpError(error) = &error else {
        panic!("expected a protocol error, got {error:?}");
    };
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS, "{error:?}");

    agmem.shutdown().await;
}

/// The one text block a ritual answers with.
fn text_of(result: &rmcp::model::GetPromptResult) -> &str {
    match &result.messages.first().expect("one message").content {
        ContentBlock::Text(text) => &text.text,
        other => panic!("a ritual is text, got {other:?}"),
    }
}

#[tokio::test]
async fn an_override_is_what_the_agent_reads() {
    let built_in = {
        let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
        let tools = agmem.client.list_tools(None).await.expect("list_tools");
        agmem.shutdown().await;
        tools.tools
    };

    let agmem = Harness::start_with(
        Arc::new(NoopEmbedder),
        ToolDescriptions::from_iter([("recall", "Ask the store before you answer.")]),
    )
    .await;
    let tools = agmem.client.list_tools(None).await.expect("list_tools");

    let described = |tools: &[rmcp::model::Tool], name: &str| {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name} is routed"))
            .description
            .clone()
            .map(String::from)
    };

    assert_eq!(
        described(&tools.tools, "recall").as_deref(),
        Some("Ask the store before you answer."),
        "the override reaches list_tools whole — this is the deployment's \
         steering lever, so a splice or a fallback would be a silent failure"
    );
    for name in ["remember", "context", "forget", "inspect"] {
        assert_eq!(
            described(&tools.tools, name),
            described(&built_in, name),
            "{name} was not overridden and must still read as it was written"
        );
    }
    assert_eq!(
        tools.tools.len(),
        built_in.len(),
        "an override rewords a tool; it never adds or removes one"
    );

    // Overriding the description does not touch the schema or the annotations
    // the same route carries — the two halves of the contract are independent.
    let overridden = tools
        .tools
        .iter()
        .find(|tool| tool.name == "recall")
        .expect("recall");
    let original = built_in
        .iter()
        .find(|tool| tool.name == "recall")
        .expect("recall");
    assert_eq!(overridden.input_schema, original.input_schema);
    assert_eq!(overridden.annotations, original.annotations);

    agmem.shutdown().await;
}

#[tokio::test]
async fn an_overridden_tool_still_runs() {
    let agmem = Harness::start_with(
        Arc::new(NoopEmbedder),
        ToolDescriptions::from_iter([("remember", "Write it down.")]),
    )
    .await;

    let written = agmem
        .remember(json!({ "memories": [{ "content": "The override changes the words, not the route." }] }))
        .await;
    assert_eq!(
        written["created"].as_array().expect("created").len(),
        1,
        "the route behind a reworded description is the same route: {written}"
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
    assert_eq!(
        duplicates[0]["content"].as_str(),
        Some("The user prefers Rust"),
        "and what the stored claim says — a correction lands here too, and \
         without its text there is no telling the two apart (issue #38)"
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
async fn a_write_comes_back_with_the_live_claims_it_may_contradict() {
    // Issue #38. Measured at #23 and again once retrieval worked: the agent
    // makes exactly one tool call, never `recall`, so it never holds the id of
    // the claim it is contradicting and both stay live. The write's own answer
    // is the only place that id can arrive.
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let first = agmem
        .remember(json!({ "memories": [{ "content": "Rust and Python are the languages here" }] }))
        .await;
    let old = ids(&first["created"])[0].to_owned();
    assert!(
        first["related"].as_array().expect("array").is_empty(),
        "an empty store has nothing to contradict: {first}"
    );

    // Two vocabulary axes against three: cosine 0.82, under the 0.95 that
    // would block it as a restatement and over the floor for being mentioned.
    let second = agmem
        .remember(json!({
            "memories": [
                { "content": "Rust and Python and kitchen tooling were all replaced" },
                { "content": "The tap drips at night" }
            ]
        }))
        .await;

    assert_eq!(ids(&second["created"]).len(), 2, "{second}");
    assert!(
        second["duplicates"].as_array().expect("array").is_empty(),
        "a candidate is not a duplicate — it was written: {second}"
    );
    let related = second["related"].as_array().expect("array");
    assert_eq!(related.len(), 1, "{second}");
    assert_eq!(
        related[0]["id"].as_str(),
        Some(old.as_str()),
        "the candidate is the id `supersedes` would take"
    );
    assert_eq!(
        related[0]["of"].as_u64(),
        Some(0),
        "and which entry of the request it is a neighbour of"
    );
    assert_eq!(
        related[0]["content"].as_str(),
        Some("Rust and Python are the languages here"),
        "with what it says, so the contradiction can be judged without a lookup"
    );
    let similarity = related[0]["similarity"].as_f64().expect("similarity");
    assert!(
        (0.75..0.95).contains(&similarity),
        "a candidate sits in the correction band, not at either end: {similarity}"
    );

    // Nothing was decided: both claims are live, which is the whole point of
    // handing the id back rather than acting on it.
    assert_eq!(
        agmem.contents().await,
        [
            "Rust and Python and kitchen tooling were all replaced",
            "Rust and Python are the languages here",
            "The tap drips at night",
        ],
        "the server never supersedes on a similarity"
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
async fn every_id_a_recall_hands_out_is_inspectable() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let stored = agmem
        .remember(json!({
            "memories": [{ "content": "The user prefers Rust over Python for CLI tools" }],
            "episode": { "content": "I'd rather write CLIs in Rust than Python." }
        }))
        .await;
    let episode = stored["episode"]
        .as_str()
        .expect("the episode id")
        .to_owned();

    let found = agmem
        .recall(json!({ "query": "which language does the user reach for in Rust projects" }))
        .await;
    assert_eq!(hits(&found).len(), 2, "a claim and its slice: {found}");

    for hit in hits(&found) {
        let id = hit["id"].as_str().expect("every hit carries an id");
        let inspected = agmem.inspect(id).await;

        if hit["kind"] == "episode" {
            // A verbatim hit hands out a *chunk* id, which is not a memory in
            // any space. Feeding it straight back used to fail, blaming
            // `space` for an id that was never a memory (issue #36).
            assert_eq!(
                inspected["ref"].as_str(),
                Some(format!("episode:{episode}").as_str()),
                "a slice answers as the episode it belongs to, echoed canonically: {inspected}"
            );
            let slices = inspected["found"]["chunks"]
                .as_array()
                .expect("the episode lists its slices");
            assert!(
                slices.iter().any(|slice| slice["id"].as_str() == Some(id)),
                "and the slice that matched is one of them: {inspected}"
            );
        } else {
            assert_eq!(
                inspected["ref"].as_str(),
                Some(format!("memory:{id}").as_str()),
                "{inspected}"
            );
            assert_eq!(inspected["found"]["memory"]["id"].as_str(), Some(id));
        }
    }

    let by_episode = agmem.inspect(&episode).await;
    assert_eq!(
        by_episode["ref"].as_str(),
        Some(format!("episode:{episode}").as_str()),
        "an episode id needs no prefix either: {by_episode}"
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
async fn a_full_page_says_how_much_of_the_store_it_is_not() {
    // The measurement behind this: asked what memory holds about a subject, an
    // agent makes one `recall` with the largest `k` it is allowed and reads the
    // answer as the whole store. That is true right up until the store outgrows
    // `k`, and a page of hits carries nothing that says which of the two
    // happened.
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({ "memories": [
            { "content": "The user prefers Rust" },
            { "content": "Deploys run from bin/ship.sh" },
            { "content": "The suite runs with cargo nextest" },
            { "content": "CI denies clippy warnings" },
            { "content": "Secrets come from the environment" }
        ] }))
        .await;

    // No query, so this is the filters-only path: rank is strength then id
    // rather than a BM25 order that a five-row corpus makes arbitrary.
    let paged = agmem.recall(json!({ "k": 2 })).await;
    assert_eq!(hits(&paged).len(), 2);
    let cut = &paged["truncated"];
    assert_eq!(cut["matching_claims"].as_u64(), Some(5), "{paged}");
    assert_eq!(cut["returned_claims"].as_u64(), Some(2), "{paged}");
    assert_eq!(cut["k"].as_u64(), Some(2), "{paged}");
    assert!(
        cut["note"]
            .as_str()
            .expect("a note")
            .contains("consolidate"),
        "a page names the read that is not one: {paged}"
    );

    let whole = agmem.recall(json!({ "k": 10 })).await;
    assert_eq!(hits(&whole).len(), 5);
    assert!(
        whole["truncated"].is_null(),
        "an answer nothing was cut from carries no cut: {whole}"
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
async fn context_lays_out_the_sections_in_a_fixed_order_and_reinforces_nothing() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let written = agmem
        .remember(json!({
            "memories": [
                { "content": "Never force-push to main", "kind": "instruction" },
                { "content": "The user prefers Rust", "tags": ["identity"] },
                { "content": "The build breaks on a cold cargo cache", "kind": "lesson" },
                { "content": "The API gateway is deployed from the infra repo" }
            ]
        }))
        .await;
    let instruction = ids(&written["created"])[0].to_owned();

    let block = agmem.context(json!({})).await;
    assert!(
        block.starts_with("# Memory context (spaces: default + user)"),
        "{block}"
    );
    assert_eq!(
        headings(&block),
        ["## Instructions", "## Profile", "## Relevant", "## Lessons"],
        "{block}"
    );
    for claim in [
        "Never force-push to main",
        "The user prefers Rust",
        "The build breaks on a cold cargo cache",
        "The API gateway is deployed from the infra repo",
    ] {
        assert!(block.contains(claim), "{claim:?} is missing from {block}");
    }
    assert!(
        block.contains(&format!("`{instruction}`")),
        "every line carries the id that leads back to it: {block}"
    );
    assert_eq!(
        block.matches("The user prefers Rust").count(),
        1,
        "an identity fact belongs to the profile, not to both sections: {block}"
    );

    assert!(
        agmem
            .memories()
            .await
            .iter()
            .all(|memory| memory.access_count == 0),
        "context is called on a schedule, so being in the block is not use"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_query_aims_the_relevant_section_and_verbatim_text_stays_out() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    agmem
        .remember(json!({
            "memories": [
                { "content": "The user prefers Rust over Python for command-line tools" },
                { "content": "The kitchen renovation starts in March" }
            ],
            "episode": {
                "content": "We went back and forth over rust and python and settled on rust."
            }
        }))
        .await;

    let block = agmem
        .context(json!({ "query": "which language for command-line tools?" }))
        .await;
    assert!(
        block.contains("The user prefers Rust over Python for command-line tools"),
        "{block}"
    );
    assert!(
        !block.contains("We went back and forth"),
        "the block is a briefing; the verbatim text is what recall is for: {block}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_small_budget_keeps_the_first_section_and_says_it_trimmed() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({
            "memories": [
                { "content": "Never force-push to main", "kind": "instruction" },
                { "content": "The build breaks on a cold cargo cache", "kind": "lesson" },
                { "content": "The API gateway is deployed from the infra repo" }
            ]
        }))
        .await;

    let budget = 200;
    let block = agmem.context(json!({ "budget_chars": budget })).await;
    assert!(
        block.chars().count() <= budget,
        "{} characters against a budget of {budget}: {block}",
        block.chars().count()
    );
    assert!(
        block.contains("Never force-push to main"),
        "instructions come first and survive the budget: {block}"
    );
    assert!(
        block.contains("_Trimmed to fit"),
        "a block that lost entries has to say so: {block}"
    );
    assert!(
        !block.contains("cold cargo cache"),
        "whole entries go, never half of one: {block}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn an_empty_space_says_so_and_an_unusable_budget_is_refused() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;

    let block = agmem.context(json!({})).await;
    assert!(
        block.ends_with("_Nothing stored for these spaces yet._"),
        "an empty store is an answer, not a blank: {block}"
    );
    assert!(headings(&block).is_empty(), "{block}");

    for (arguments, expected) in [
        (
            json!({ "budget_chars": 10 }),
            "budget_chars must be at least 200",
        ),
        (json!({ "space": "Not A Slug" }), "space:"),
    ] {
        let error = agmem
            .call("context", arguments.clone())
            .await
            .expect_err("the call must be refused");
        assert!(
            matches!(&error, ServiceError::McpError(data)
                if data.code == ErrorCode::INVALID_PARAMS && data.message.contains(expected)),
            "{arguments} should be refused naming {expected:?}, got: {error}"
        );
    }
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

#[tokio::test]
async fn a_forgotten_claim_stops_answering_but_stays_readable() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let stored = agmem
        .remember(json!({
            "memories": [
                { "content": "the user prefers Rust over Python for CLI tools" },
                { "content": "the kitchen renovation finished in March" }
            ]
        }))
        .await;
    let kitchen = ids(&stored["created"])[1].to_owned();

    let preview = agmem
        .forget(json!({ "ids": [kitchen.clone()], "dry_run": true }))
        .await;
    assert_eq!(match_ids(&preview["matched"]), vec![kitchen.as_str()]);
    assert_eq!(preview["matched"][0]["kind"].as_str(), Some("memory"));
    assert!(
        ids(&preview["invalidated"]).is_empty(),
        "a dry run reports and changes nothing"
    );
    assert_eq!(agmem.contents().await.len(), 2);

    let done = agmem
        .forget(json!({ "ids": [format!("memory:{kitchen}")] }))
        .await;
    assert_eq!(ids(&done["invalidated"]), vec![kitchen.as_str()]);
    assert!(ids(&done["purged"]).is_empty());

    let found = agmem.recall(json!({ "query": "kitchen renovation" })).await;
    assert!(
        !hit_contents(&found)
            .iter()
            .any(|hit| hit.contains("kitchen")),
        "a forgotten claim stops answering recall: {found}"
    );
    let block = agmem.context(json!({})).await;
    assert!(
        !block.contains("kitchen"),
        "and stops being briefed at session start:\n{block}"
    );

    let seen = agmem.inspect(&kitchen).await;
    assert_eq!(
        seen["found"]["memory"]["invalid_reason"].as_str(),
        Some("forgotten"),
        "but stays readable, dated, and labelled — a wrong forget is recoverable"
    );
    assert!(
        hit_contents(&agmem.recall(json!({ "query": "Rust" })).await)
            .iter()
            .any(|hit| hit.contains("Rust")),
        "and nothing else moved"
    );
    assert_eq!(agmem.memories().await.len(), 2, "soft is not deletion");
    agmem.shutdown().await;
}

#[tokio::test]
async fn forgetting_by_query_needs_the_same_call_with_dry_run_first() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({
            "memories": [
                { "content": "the kitchen renovation finished in March" },
                { "content": "the user prefers Rust over Python" }
            ]
        }))
        .await;

    let straight_in = agmem
        .call("forget", json!({ "query": "kitchen" }))
        .await
        .expect_err("a query with no confirmation must be refused");
    assert!(
        refusal(&straight_in, "dry_run"),
        "and must say what is missing, got: {straight_in}"
    );

    let preview = agmem
        .forget(json!({ "query": "kitchen", "dry_run": true }))
        .await;
    assert_eq!(
        match_ids(&preview["matched"]).len(),
        1,
        "a query selects on the words it contains, not on what resembles them: {preview}"
    );
    let matched = match_ids(&preview["matched"])[0].to_owned();

    let wider = agmem
        .call("forget", json!({ "query": "kitchen", "purge": true }))
        .await
        .expect_err("previewing a close does not authorise a delete");
    assert!(refusal(&wider, "dry_run"), "got: {wider}");

    let done = agmem.forget(json!({ "query": "kitchen" })).await;
    assert_eq!(ids(&done["invalidated"]), vec![matched.as_str()]);

    let again = agmem
        .call("forget", json!({ "query": "kitchen" }))
        .await
        .expect_err("a confirmation authorises one call, not a standing licence");
    assert!(refusal(&again, "dry_run"), "got: {again}");

    assert_eq!(
        agmem.stats().await.live,
        1,
        "the claim that did not contain the word is untouched"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_purge_takes_the_whole_correction_chain_and_leaves_no_row() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let first = agmem
        .remember(json!({ "memories": [{ "content": "the office is on the third floor" }] }))
        .await;
    let old = ids(&first["created"])[0].to_owned();
    let second = agmem
        .remember(json!({
            "memories": [{
                "content": "the office is on the fourth floor",
                "supersedes": format!("memory:{old}")
            }]
        }))
        .await;
    let new = ids(&second["created"])[0].to_owned();

    let preview = agmem
        .forget(json!({ "ids": [new.clone()], "purge": true, "dry_run": true }))
        .await;
    let mut shown = match_ids(&preview["matched"]);
    shown.sort_unstable();
    let mut both = vec![old.as_str(), new.as_str()];
    both.sort_unstable();
    assert_eq!(
        shown, both,
        "a purge shows the whole correction chain before it takes it: {preview}"
    );
    assert_eq!(
        agmem.memories().await.len(),
        2,
        "and the preview itself deletes nothing"
    );

    let done = agmem
        .forget(json!({ "ids": [new.clone()], "purge": true }))
        .await;
    let mut gone = ids(&done["purged"]);
    gone.sort_unstable();
    assert_eq!(gone, both);
    assert!(ids(&done["invalidated"]).is_empty());
    assert!(
        agmem.memories().await.is_empty(),
        "a purge leaves no rows behind"
    );

    let vanished = agmem
        .call("inspect", json!({ "ref": old }))
        .await
        .expect_err("there is nothing left to audit");
    assert!(refusal(&vanished, "no memory"), "got: {vanished}");
    agmem.shutdown().await;
}

#[tokio::test]
async fn verbatim_text_can_only_be_purged_and_its_claims_outlive_it() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let stored = agmem
        .remember(json!({
            "memories": [{ "content": "the user prefers Rust over Python" }],
            "episode": { "content": "I like Rust. Python is fine too." }
        }))
        .await;
    let claim = ids(&stored["created"])[0].to_owned();
    let episode = stored["episode"]
        .as_str()
        .expect("an episode id")
        .to_owned();

    let closed = agmem
        .call("forget", json!({ "ids": [format!("episode:{episode}")] }))
        .await
        .expect_err("verbatim text has no validity window to close");
    assert!(refusal(&closed, "purge: true"), "got: {closed}");

    let slices = agmem.inspect(&format!("episode:{episode}")).await;
    let slice = slices["found"]["chunks"][0]["id"]
        .as_str()
        .expect("a slice")
        .to_owned();
    let by_slice = agmem
        .call("forget", json!({ "ids": [slice], "purge": true }))
        .await
        .expect_err("a slice is not a thing anyone forgets");
    assert!(
        refusal(&by_slice, "one slice of episode:"),
        "got: {by_slice}"
    );

    let preview = agmem
        .forget(json!({ "ids": [format!("episode:{episode}")], "purge": true, "dry_run": true }))
        .await;
    assert_eq!(preview["matched"][0]["kind"].as_str(), Some("episode"));
    assert_eq!(
        preview["matched"][0]["derived"].as_u64(),
        Some(1),
        "what the text leaves behind is part of the scope: {preview}"
    );

    let done = agmem
        .forget(json!({ "ids": [format!("episode:{episode}")], "purge": true }))
        .await;
    assert_eq!(ids(&done["purged"]), vec![episode.as_str()]);
    assert_eq!(done["chunks_purged"].as_u64(), Some(1));

    let stats = agmem.stats().await;
    assert_eq!(
        (stats.episodes, stats.chunks, stats.memories),
        (0, 0, 1),
        "purging text does not purge what was learned from it"
    );

    let seen = agmem.inspect(&claim).await;
    assert_eq!(
        seen["found"]["memory"]["source"].as_str(),
        Some(format!("episode:{episode}").as_str()),
        "the claim still says where it came from"
    );
    assert!(
        seen["found"]["episode"].is_null(),
        "there is simply nothing left to quote: {seen}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn consolidate_clusters_near_duplicates_and_says_how_loose_the_group_is() {
    let agmem = Harness::start(Arc::new(AngleEmbedder)).await;
    // 0°, 20° and 40°: each adjacent pair clears the clustering bar, the ends
    // do not. One group, chained — which is the case worth reporting honestly,
    // because merging it whole would lose a distinction.
    agmem
        .remember(json!({
            "memories": [
                { "content": "the user formats python with black" },
                { "content": "python here is formatted by blackfmt" },
                { "content": "formatting runs blake over python" },
                { "content": "the kitchen tap drips at night" },
            ]
        }))
        .await;

    let found = agmem.consolidate(json!({})).await;
    let clusters = found["near_duplicates"].as_array().expect("an array");
    assert_eq!(clusters.len(), 1, "{found:#}");

    let members = clusters[0]["members"].as_array().expect("an array");
    assert_eq!(
        members.len(),
        3,
        "the orthogonal note joins nothing: {found:#}"
    );
    for member in members {
        let content = member["content"]
            .as_str()
            .expect("every member is readable");
        assert!(content.contains("python"), "{content}");
        assert!(
            member["id"].as_str().is_some(),
            "and addressable: {member:#}"
        );
    }

    let min = clusters[0]["min_similarity"].as_f64().expect("a number");
    let max = clusters[0]["max_similarity"].as_f64().expect("a number");
    assert!(
        min < 0.80,
        "the two ends of the chain are not a duplicate pair, and the answer \
         has to say so: {min}"
    );
    assert!(max > 0.93, "the adjacent pairs are what linked them: {max}");

    // Two of those pairs sit in the contradiction band, and neither claim
    // names an entity — nothing is offered as a disagreement on similarity
    // alone.
    assert!(
        found["contradictions"]
            .as_array()
            .expect("an array")
            .is_empty(),
        "{found:#}"
    );
    assert_eq!(found["scanned"][0]["compared"], 4);
    assert_eq!(found["scanned"][0]["truncated"], false);
    assert_eq!(found["spaces"], json!(["default"]));
    assert!(found.get("note").is_none(), "{found:#}");
}

#[tokio::test]
async fn consolidate_pairs_claims_about_one_subject_that_may_disagree() {
    let agmem = Harness::start(Arc::new(AngleEmbedder)).await;
    agmem
        .remember(json!({
            "memories": [
                { "content": "the user formats python with black", "entities": ["python"] },
                { "content": "the user formats python with ruff", "entities": ["Python"] },
                { "content": "the kitchen tap drips at night", "entities": ["home"] },
            ]
        }))
        .await;

    let found = agmem.consolidate(json!({})).await;
    assert!(
        found["near_duplicates"]
            .as_array()
            .expect("an array")
            .is_empty(),
        "30° apart is one subject stated twice, not one claim: {found:#}"
    );

    let pairs = found["contradictions"].as_array().expect("an array");
    assert_eq!(pairs.len(), 1, "{found:#}");
    let shared = pairs[0]["shared_entities"].as_array().expect("an array");
    assert_eq!(shared.len(), 1, "{found:#}");
    assert_eq!(
        shared[0].as_str().expect("a subject").to_lowercase(),
        "python",
        "the same subject spelled two ways is one subject"
    );

    let (left, right) = (
        pairs[0]["a"]["content"].as_str().expect("readable"),
        pairs[0]["b"]["content"].as_str().expect("readable"),
    );
    assert!(
        left.contains("black") != right.contains("black"),
        "both sides carry their text, or there is nothing to decide between: \
         {left:?} / {right:?}"
    );
    let similarity = pairs[0]["similarity"].as_f64().expect("a number");
    assert!((0.75..0.90).contains(&similarity), "{similarity}");
}

#[tokio::test]
async fn consolidate_offers_one_close_pair_as_both_a_merge_and_a_disagreement() {
    let agmem = Harness::start(Arc::new(AngleEmbedder)).await;
    agmem
        .remember(json!({
            "memories": [
                { "content": "the user formats python with black", "entities": ["python"] },
                { "content": "python here is formatted by blackfmt", "entities": ["python"] },
            ]
        }))
        .await;

    // 20° apart is 0.94, over the clustering bar — and that is exactly where a
    // real contradiction lands. Measured against BGE-small, seven contradicting
    // pairs scored 0.919 to 0.974, while the one pair that did *not* contradict
    // scored 0.898. So the two lists overlap, and one pair has to be able to
    // appear in both: which of those two readings applies is the caller.
    let found = agmem.consolidate(json!({})).await;
    let clusters = found["near_duplicates"].as_array().expect("an array");
    assert_eq!(clusters.len(), 1, "{found:#}");

    let pairs = found["contradictions"].as_array().expect("an array");
    assert_eq!(pairs.len(), 1, "{found:#}");
    let similarity = pairs[0]["similarity"].as_f64().expect("a number");
    assert!(
        similarity > 0.93,
        "the same pair, reported over the cluster bar: {similarity}"
    );
    let shared = pairs[0]["shared_entities"].as_array().expect("an array");
    assert_eq!(shared.len(), 1, "{found:#}");
    assert_eq!(
        shared[0].as_str().expect("a subject").to_lowercase(),
        "python",
        "the shared subject is what keeps this list from copying the other"
    );
}

#[tokio::test]
async fn consolidate_reports_short_lived_notes_that_recall_kept_alive() {
    // No embedder at all: the stale arm is the one finding that needs no
    // vectors, and it has to keep working in BM25-only mode.
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let written = agmem
        .remember(json!({
            "memories": [
                { "content": "the branch under review is called spike", "decay_class": "fast" },
                { "content": "the failing test is called roundtrip", "decay_class": "fast" },
            ]
        }))
        .await;
    let created = ids(&written["created"]);
    // Recalled thirty times, so `strength` bought it roughly 620 days against
    // a class whose unreinforced horizon is twenty.
    age(&agmem.db, created[0], 200, 31.0, 30).await;
    // Equally idle and never used: the prune closes this one on its own.
    age(&agmem.db, created[1], 200, 1.0, 1).await;

    let found = agmem.consolidate(json!({})).await;
    let stale = found["stale_contexts"].as_array().expect("an array");
    assert_eq!(stale.len(), 1, "{found:#}");
    assert_eq!(stale[0]["claim"]["id"], created[0]);
    assert!(
        stale[0]["claim"]["content"]
            .as_str()
            .expect("readable")
            .contains("spike"),
        "{found:#}"
    );
    assert_eq!(stale[0]["claim"]["decay_class"], "fast");
    assert!(stale[0]["idle_days"].as_f64().expect("a number") > 199.0);
    assert!(
        stale[0]["expires_in_days"].as_f64().expect("a number") > 300.0,
        "the finding is that the sweep will not reach it for a year: {found:#}"
    );

    let note = found["note"].as_str().expect("a note");
    assert!(
        note.contains("without an embedder"),
        "an empty similarity section has to say whether it is empty or blind: {note}"
    );
}

#[tokio::test]
async fn consolidate_on_an_empty_store_answers_empty_rather_than_failing() {
    let agmem = Harness::start(Arc::new(AngleEmbedder)).await;
    let found = agmem.consolidate(json!({})).await;

    assert_eq!(found["near_duplicates"], json!([]));
    assert_eq!(found["contradictions"], json!([]));
    assert_eq!(found["stale_contexts"], json!([]));
    assert_eq!(found["spaces"], json!(["default"]));
    assert_eq!(
        found["scanned"],
        json!([{ "space": "default", "compared": 0, "truncated": false }])
    );
    assert!(
        found.get("note").is_none(),
        "nothing limited this answer: {found:#}"
    );
}

#[tokio::test]
async fn consolidate_stays_in_the_current_space_unless_asked_otherwise() {
    let agmem = Harness::start(Arc::new(AngleEmbedder)).await;
    agmem
        .remember(json!({
            "space": "user",
            "memories": [
                { "content": "the user formats python with black" },
                { "content": "python here is formatted by blackfmt" },
            ]
        }))
        .await;

    // Every other read defaults to the current space *and* `user`. This one
    // does not: a tidy-up should not reach across projects by default.
    let here = agmem.consolidate(json!({})).await;
    assert_eq!(here["spaces"], json!(["default"]));
    assert!(
        here["near_duplicates"]
            .as_array()
            .expect("an array")
            .is_empty(),
        "{here:#}"
    );

    let asked = agmem.consolidate(json!({ "space": "user" })).await;
    assert_eq!(asked["spaces"], json!(["user"]));
    assert_eq!(
        asked["near_duplicates"].as_array().expect("an array").len(),
        1,
        "{asked:#}"
    );
}

#[tokio::test]
async fn a_reflection_is_recallable_and_walks_back_to_its_evidence() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let stored = agmem
        .remember(json!({
            "memories": [{
                "content": "The cargo build failed twice on a cold disk cache",
                "entities": ["cargo"]
            }],
            "episode": { "content": "Cold cache again. The build took nine minutes." }
        }))
        .await;
    let evidence = stored["created"][0]
        .as_str()
        .expect("the claim id")
        .to_owned();
    let episode = stored["episode"]
        .as_str()
        .expect("the episode id")
        .to_owned();

    let reflected = agmem
        .reflect(json!({
            "insight": "Timing a build on this machine means warming the cargo cache first",
            "derived_from": [evidence.clone(), format!("episode:{episode}")],
            "entities": ["cargo"]
        }))
        .await;
    assert_eq!(reflected["created"], json!(true), "{reflected}");
    let citations = json!([format!("memory:{evidence}"), format!("episode:{episode}")]);
    assert_eq!(
        reflected["derived_from"], citations,
        "a bare id comes back qualified, in the order it was cited: {reflected}"
    );
    let insight = reflected["id"].as_str().expect("the insight id").to_owned();

    // Recallable like anything else: a reflection is a memory row, not a
    // second kind of record. Filters only, so the order is not a search
    // engine's opinion.
    let lessons = agmem.recall(json!({ "kinds": ["lesson"] })).await;
    assert_eq!(
        hit_contents(&lessons),
        vec!["Timing a build on this machine means warming the cargo cache first"],
        "{lessons}"
    );

    // …and walkable back: `inspect` renders the citations as refs it takes as
    // they stand, so the evidence behind a conclusion is one call away.
    let audited = agmem.inspect(&insight).await;
    assert_eq!(
        audited["found"]["memory"]["derived_from"], citations,
        "{audited}"
    );
    for cited in citations.as_array().expect("citations") {
        let followed = agmem.inspect(cited.as_str().expect("a ref")).await;
        assert_eq!(
            followed["ref"], *cited,
            "a citation is a ref inspect answers to unchanged: {followed}"
        );
    }
    let text = agmem.inspect(&format!("episode:{episode}")).await;
    assert!(
        text["found"]["derived"]
            .as_array()
            .expect("the claims drawn from the text")
            .iter()
            .any(|claim| claim["id"].as_str() == Some(evidence.as_str())),
        "the cited episode still lists what was distilled from it: {text}"
    );

    // A claim nobody reflected out of anything says so by omission.
    let plain = agmem.inspect(&evidence).await;
    assert_eq!(
        plain["found"]["memory"]["derived_from"],
        Value::Null,
        "an empty citation list is absent rather than empty: {plain}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_reflection_has_to_cite_something_the_store_holds() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let claim = agmem
        .remember(json!({ "memories": [{ "content": "The project pins surrealdb to 3.x" }] }))
        .await["created"][0]
        .as_str()
        .expect("the claim id")
        .to_owned();
    const ABSENT: &str = "01M145SMNET1XRYA713EWAQTD3";

    for (arguments, expected) in [
        (
            json!({ "insight": "an insight", "derived_from": [] }),
            "derived_from is empty",
        ),
        (
            json!({ "insight": "   ", "derived_from": [claim.clone()] }),
            "insight is empty",
        ),
        (
            json!({ "insight": "an insight", "derived_from": [ABSENT] }),
            "no memory or episode",
        ),
        (
            // The prefix is checked rather than trusted: an id that names the
            // other table is a mistake worth naming.
            json!({ "insight": "an insight", "derived_from": [format!("episode:{claim}")] }),
            "that id names a memory",
        ),
        (
            json!({ "insight": "an insight", "derived_from": [format!("chunk:{ABSENT}")] }),
            "derived_from takes ids",
        ),
    ] {
        let error = agmem
            .call("reflect", arguments.clone())
            .await
            .expect_err("the call must be refused");
        assert!(
            refusal(&error, expected),
            "{arguments} should be refused with {expected:?}, got {error:?}"
        );
    }

    assert!(
        agmem.memories().await.len() == 1,
        "a refused reflection writes nothing"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn the_same_insight_twice_is_reported_rather_than_stored_again() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let evidence = agmem
        .remember(json!({ "memories": [{ "content": "python scripts here are throwaway" }] }))
        .await["created"][0]
        .as_str()
        .expect("the claim id")
        .to_owned();

    let first = agmem
        .reflect(json!({
            "insight": "rust is the language this project reaches for",
            "derived_from": [evidence.clone()]
        }))
        .await;
    assert_eq!(first["created"], json!(true), "{first}");

    let again = agmem
        .reflect(json!({
            "insight": "rust is what gets written here, whatever the task",
            "derived_from": [evidence]
        }))
        .await;
    assert_eq!(again["created"], json!(false), "{again}");
    assert_eq!(
        again["id"], first["id"],
        "the answer is the id of the claim that already said this: {again}"
    );
    assert_eq!(
        again["content"], first["content"],
        "and its wording, so a correction can be told from a repetition: {again}"
    );
    assert_eq!(
        again["derived_from"],
        json!([]),
        "nothing was written, so nothing was cited: {again}"
    );
    assert_eq!(
        again["note"],
        Value::Null,
        "the claim that blocked it carries its own evidence, so this duplicate \
         really is a no-op and there is nothing to advise: {again}"
    );
    assert_eq!(
        agmem.memories().await.len(),
        2,
        "the store holds the evidence and one insight"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn an_insight_blocked_by_an_uncited_claim_is_told_how_to_cite_it() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let evidence = agmem
        .remember(json!({ "memories": [{ "content": "python scripts here are throwaway" }] }))
        .await["created"][0]
        .as_str()
        .expect("the evidence id")
        .to_owned();

    // The conclusion as an agent actually writes it mid-session: through
    // `remember`, with nothing behind it.
    let uncited = agmem
        .remember(json!({
            "memories": [{
                "content": "rust is the language this project reaches for",
                "kind": "lesson"
            }]
        }))
        .await["created"][0]
        .as_str()
        .expect("the lesson id")
        .to_owned();

    // The same conclusion at checkpoint, now carrying its evidence. The gate
    // blocks it, and `created: false` on its own would read as "already
    // handled" while the provenance goes nowhere (2 of 3 measured runs, #26).
    let blocked = agmem
        .reflect(json!({
            "insight": "rust is what this project reaches for, whatever the task",
            "derived_from": [evidence.clone()]
        }))
        .await;
    assert_eq!(blocked["created"], json!(false), "{blocked}");
    assert_eq!(blocked["id"], json!(uncited), "{blocked}");
    assert_eq!(blocked["derived_from"], json!([]), "{blocked}");
    let note = blocked["note"]
        .as_str()
        .expect("a blocked insight carries the move that is left");
    assert!(
        note.contains(&uncited) && note.contains("supersedes"),
        "the note has to name the id and the verb, not just report a no-op: {note}"
    );

    // Doing what it says.
    let cited = agmem
        .reflect(json!({
            "insight": "rust is what this project reaches for, whatever the task",
            "derived_from": [evidence.clone()],
            "supersedes": uncited.clone()
        }))
        .await;
    assert_eq!(cited["created"], json!(true), "{cited}");
    assert_eq!(
        cited["derived_from"],
        json!([format!("memory:{evidence}")]),
        "{cited}"
    );
    assert_eq!(cited["superseded"], json!(uncited), "{cited}");
    assert_eq!(
        cited["note"],
        Value::Null,
        "a write that happened has nothing to advise: {cited}"
    );

    // The store agrees: one live cited conclusion, the uncited one closed.
    let stored = agmem.memories().await;
    let old = stored
        .iter()
        .find(|memory| memory.id.as_str() == uncited)
        .expect("the uncited claim is still readable");
    assert!(!old.is_live() && old.derived_from.is_empty(), "{old:?}");
    let new = stored
        .iter()
        .find(|memory| memory.id.as_str() == cited["id"].as_str().expect("id"))
        .expect("the cited claim");
    assert!(new.is_live() && new.derived_from.len() == 1, "{new:?}");
    agmem.shutdown().await;
}
