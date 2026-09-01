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

mod harness;

use std::sync::Arc;

use agmem_core::{Kind, Source, Writer};
use agmem_embed::NoopEmbedder;
use agmem_server::config::ToolDescriptions;
use agmem_store::repo::{self, Batch, Lookup, NewMemory};
use harness::*;
use rmcp::model::{
    ContentBlock, ErrorCode, GetPromptRequestParams, ReadResourceRequestParams, Role,
};
use rmcp::service::ServiceError;
use serde_json::{Value, json};
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
    assert!(
        info.capabilities.resources.is_some(),
        "and the resource capability, or `memory://` is unreachable"
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
async fn resources_list_serves_one_index_per_space() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let listed = agmem
        .client
        .list_resources(None)
        .await
        .expect("list_resources");

    let index = listed
        .resources
        .iter()
        .find(|resource| resource.uri == "memory://default")
        .unwrap_or_else(|| panic!("the served space must be listed: {:?}", listed.resources));
    assert_eq!(index.name, "default");
    assert_eq!(index.mime_type.as_deref(), Some("application/json"));
    assert!(
        listed
            .resources
            .iter()
            .all(|resource| resource.uri.starts_with("memory://")),
        "every listed resource is a space index: {:?}",
        listed.resources
    );

    agmem.shutdown().await;
}

#[tokio::test]
async fn resource_templates_publish_the_record_form() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let listed = agmem
        .client
        .list_resource_templates(None)
        .await
        .expect("list_resource_templates");

    let [template] = &listed.resource_templates[..] else {
        panic!("one template — records are addressed, never enumerated: {listed:?}");
    };
    assert_eq!(template.uri_template, "memory://{space}/{id}");

    agmem.shutdown().await;
}

#[tokio::test]
async fn a_space_index_lists_live_claims_and_their_uris() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let first = agmem
        .remember(json!({ "memories": [{ "content": "The deploy target is Fly.io." }] }))
        .await;
    let stored = ids(&first["created"])[0].to_owned();
    let correction = agmem
        .remember(json!({ "memories": [
            { "content": "The deploy target moved to Railway.", "supersedes": [stored] }
        ] }))
        .await;
    let live = ids(&correction["created"])[0].to_owned();

    let index = read_json(&agmem, "memory://default").await;
    assert_eq!(index["space"], "default");
    assert_eq!(
        index["live"], 1,
        "a superseded claim is not part of the index: {index}"
    );
    let entry = &index["memories"][0];
    assert_eq!(entry["id"], live.as_str(), "{index}");
    assert_eq!(entry["content"], "The deploy target moved to Railway.");
    assert_eq!(
        entry["uri"],
        format!("memory://default/{live}"),
        "each entry carries the URI that reads it whole"
    );
    assert!(
        index.get("truncated").is_none(),
        "an index that fits in one read carries no truncation marker: {index}"
    );

    agmem.shutdown().await;
}

/// Issue #69: a space larger than one lookup serves must say so — `live`
/// keeps the true count and `truncated` marks the cut, instead of the index
/// rendering 1000 entries beside a bigger number as if it were complete.
#[tokio::test]
async fn a_space_index_larger_than_one_read_marks_the_cut() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let over = repo::MAX_POOL + 1;
    repo::insert_batch(
        &agmem.db,
        Batch {
            space: space(),
            episode: None,
            memories: (0..over)
                .map(|n| NewMemory::new(Kind::Fact, format!("claim number {n}")))
                .collect(),
            writer: Writer::default(),
        },
    )
    .await
    .expect("seed past MAX_POOL");

    let index = read_json(&agmem, "memory://default").await;
    assert_eq!(
        index["live"], over as u64,
        "the count is the space's: {index}"
    );
    assert_eq!(
        index["memories"].as_array().expect("memories").len(),
        repo::MAX_POOL,
        "the listing is one lookup's page"
    );
    let note = index["truncated"]
        .as_str()
        .unwrap_or_else(|| panic!("a cut index must carry the marker in words: {index}"));
    assert!(
        note.contains(&repo::MAX_POOL.to_string()) && note.contains(&over.to_string()),
        "the marker names both sides of the cut: {note}"
    );

    agmem.shutdown().await;
}

#[tokio::test]
async fn a_memory_uri_reads_the_inspect_answer() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let written = agmem
        .remember(
            json!({ "memories": [{ "content": "The CI cache key includes the rustc version." }] }),
        )
        .await;
    let stored = ids(&written["created"])[0].to_owned();

    let answer = read_json(&agmem, &format!("memory://default/{stored}")).await;
    assert_eq!(
        answer["ref"],
        format!("memory:{stored}"),
        "the URI resolves to inspect's canonical reference: {answer}"
    );
    assert_eq!(answer["found"]["kind"], "memory", "{answer}");
    assert_eq!(
        answer["found"]["memory"]["content"],
        "The CI cache key includes the rustc version."
    );
    assert_eq!(
        answer["found"]["chain"].as_array().expect("chain").len(),
        1,
        "provenance comes with the record: {answer}"
    );

    agmem.shutdown().await;
}

#[tokio::test]
async fn a_uri_naming_nothing_is_resource_not_found() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;

    for uri in [
        "memory://default/01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "memory://no-such-space",
        "file:///etc/passwd",
    ] {
        let error = agmem
            .client
            .read_resource(ReadResourceRequestParams::new(uri))
            .await
            .expect_err(uri);
        let ServiceError::McpError(error) = &error else {
            panic!("{uri}: expected a protocol error, got {error:?}");
        };
        assert_eq!(
            error.code,
            ErrorCode::RESOURCE_NOT_FOUND,
            "{uri}: {error:?}"
        );
    }

    agmem.shutdown().await;
}

/// One `resources/read`, its single JSON text block parsed.
async fn read_json(agmem: &Harness, uri: &str) -> Value {
    let result = agmem
        .client
        .read_resource(ReadResourceRequestParams::new(uri))
        .await
        .unwrap_or_else(|error| panic!("{uri}: {error}"));
    let [
        rmcp::model::ResourceContents::TextResourceContents {
            text,
            mime_type,
            uri: echoed,
            ..
        },
    ] = &result.contents[..]
    else {
        panic!("{uri}: one text block, got {result:?}");
    };
    assert_eq!(mime_type.as_deref(), Some("application/json"), "{uri}");
    assert_eq!(echoed, uri, "the contents name the URI they answer");
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{uri}: {error}: {text}"))
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

/// Issue #75: every write records who made it, and `inspect` shows it. The
/// client name is whatever the MCP client introduced itself as — asserted
/// non-empty rather than as a literal, because the test client reports
/// rmcp's own build info — and `tool` names the verb that wrote the row.
#[tokio::test]
async fn a_write_records_its_writer_and_inspect_shows_it() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let diff = agmem
        .remember(json!({ "memories": [{ "content": "The deploy target moved to Railway." }] }))
        .await;
    let remembered = ids(&diff["created"])[0].to_owned();

    let found = agmem.inspect(&remembered).await;
    let writer = &found["found"]["memory"]["writer"];
    assert_eq!(writer["tool"].as_str(), Some("remember"), "{found}");
    assert!(
        writer["client"]
            .as_str()
            .is_some_and(|name| !name.is_empty()),
        "the writer names the client that introduced itself: {found}"
    );
    assert!(
        writer["session"]
            .as_str()
            .is_some_and(|session| !session.is_empty()),
        "a client that offers no session id gets the connection's: {found}"
    );

    let insight = agmem
        .reflect(json!({
            "insight": "Deploys keep moving toward automation.",
            "derived_from": [remembered]
        }))
        .await;
    let reflected = agmem.inspect(insight["id"].as_str().expect("an id")).await;
    assert_eq!(
        reflected["found"]["memory"]["writer"]["tool"].as_str(),
        Some("reflect"),
        "each verb stamps its own name: {reflected}"
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
                "supersedes": [old],
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

/// Issue #62: a supersede whose target is already closed is a report, not a
/// rewrite — the first close keeps its date and successor, and the caller is
/// told what stands instead of reading silence as a landed closure.
#[tokio::test]
async fn a_second_correction_reports_the_close_that_stands() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let first = agmem
        .remember(json!({ "memories": [{ "content": "The deploy target is Fly.io" }] }))
        .await;
    let old = ids(&first["created"])[0].to_owned();
    let corrected = agmem
        .remember(json!({
            "memories": [{ "content": "The deploy target moved to Railway", "supersedes": [old] }]
        }))
        .await;
    let successor = ids(&corrected["created"])[0].to_owned();

    // The same target again — a retried call, or a second session racing.
    let raced = agmem
        .remember(json!({
            "memories": [{ "content": "The deploy target moved to Render", "supersedes": [old] }]
        }))
        .await;

    assert_eq!(
        ids(&raced["created"]).len(),
        1,
        "the new claim itself lands: {raced}"
    );
    assert!(
        ids(&raced["superseded"]).is_empty(),
        "but it closed nothing: {raced}"
    );
    let skipped = raced["already_closed"]
        .as_array()
        .expect("already_closed is a list");
    assert_eq!(skipped.len(), 1, "the skipped target is reported: {raced}");
    assert_eq!(
        (
            skipped[0]["id"].as_str(),
            skipped[0]["reason"].as_str(),
            skipped[0]["superseded_by"].as_str()
        ),
        (
            Some(old.as_str()),
            Some("superseded"),
            Some(successor.as_str())
        ),
        "naming the close that stands: {raced}"
    );

    let memories = agmem.memories().await;
    let closed = memories
        .iter()
        .find(|memory| memory.id.as_str() == old)
        .expect("the closed memory is still readable");
    assert_eq!(
        closed.superseded_by.as_ref().map(|id| id.as_str()),
        Some(successor.as_str()),
        "the first close keeps its successor"
    );
    agmem.shutdown().await;
}

/// Issue #65: the read side's space vocabulary works on writes too. Before
/// this, `remember(space: "current")` created a literal space *named*
/// `current` — real rows, unreachable by every future read that says the
/// same word.
#[tokio::test]
async fn write_side_space_keywords_resolve_instead_of_becoming_slugs() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;

    agmem
        .remember(json!({
            "space": "current",
            "memories": [{ "content": "the keyword lands in the configured space" }]
        }))
        .await;
    let rows = agmem.memories().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].space.as_str(),
        "default",
        "`current` is vocabulary, not a slug"
    );

    agmem
        .remember(json!({
            "space": "user",
            "memories": [{ "content": "a fact that follows the person" }]
        }))
        .await;
    let found = agmem.recall(json!({ "space": "user" })).await;
    assert_eq!(
        hits(&found).len(),
        1,
        "the write went where the read looks: {found}"
    );

    let refused = agmem
        .call(
            "remember",
            json!({ "space": "all", "memories": [{ "content": "everywhere at once" }] }),
        )
        .await
        .expect_err("a write into `all` is not a scope");
    assert!(
        matches!(&refused, ServiceError::McpError(data)
            if data.code == ErrorCode::INVALID_PARAMS && data.message.contains("read-only")),
        "{refused}"
    );

    let registry = repo::spaces(&agmem.db).await.expect("spaces");
    assert!(
        !registry
            .iter()
            .any(|space| matches!(space.as_str(), "current" | "all")),
        "no keyword ever becomes a literal space: {registry:?}"
    );
    agmem.shutdown().await;
}

/// Issue #57: a word-for-word re-send with `supersedes` used to block at the
/// exact-hash gate with the supersede riding on the blocked entry — so the
/// retry the description asks for ("re-send yours with the id in supersedes")
/// looped forever whenever the correction was identical to an already-live
/// claim. The duplicate must close the targets anyway, in favour of the row
/// that already holds the content.
#[tokio::test]
async fn a_duplicate_carrying_supersedes_still_closes_its_targets() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let seeded = agmem
        .remember(json!({
            "memories": [
                { "content": "The user formats Python with ruff" },
                { "content": "The user formats Python with black" },
            ]
        }))
        .await;
    let created = ids(&seeded["created"]);
    let (live, stale) = (created[0].to_owned(), created[1].to_owned());

    let diff = agmem
        .remember(json!({
            "memories": [{
                "content": "The user formats Python with ruff",
                "supersedes": [live, stale],
            }]
        }))
        .await;

    let duplicates = diff["duplicates"].as_array().expect("an array");
    assert_eq!(duplicates.len(), 1, "{diff}");
    assert_eq!(
        duplicates[0]["id"], live,
        "reported against the row that already holds the claim: {diff}"
    );
    assert!(ids(&diff["created"]).is_empty(), "{diff}");
    assert_eq!(
        ids(&diff["superseded"]),
        [stale.as_str()],
        "the target closed even though nothing was written — the retry terminates: {diff}"
    );

    let memories = agmem.memories().await;
    let closed = memories
        .iter()
        .find(|memory| memory.id.as_str() == stale)
        .expect("the closed claim is still readable");
    assert_eq!(
        (
            closed.invalid_reason.map(|reason| reason.as_str()),
            closed.superseded_by.as_ref().map(|id| id.as_str())
        ),
        (Some("superseded"), Some(live.as_str())),
        "closed pointing forward at the already-stored claim"
    );
    let survivor = memories
        .iter()
        .find(|memory| memory.id.as_str() == live)
        .expect("the live claim");
    assert_eq!(
        survivor.invalid_reason, None,
        "naming the duplicate row in `supersedes` does not close it against itself"
    );
    agmem.shutdown().await;
}

/// Issue #42: a duplicate cluster is merged by one call, not by one
/// supersession and N `forget`s. `supersedes` takes a list precisely so the
/// rest of a cluster keeps its history — `forget` would take it away.
#[tokio::test]
async fn one_call_merges_a_whole_duplicate_cluster() {
    let agmem = Harness::start(Arc::new(KeywordEmbedder)).await;
    let seeded = agmem
        .remember(json!({
            "memories": [
                { "content": "The user writes rust" },
                { "content": "Rust is what the user reaches for" },
                { "content": "For new work the user picks rust" }
            ]
        }))
        .await;
    let cluster: Vec<String> = ids(&seeded["created"])
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
    assert_eq!(cluster.len(), 3, "same-batch entries never gate each other");

    let merged = agmem
        .remember(json!({
            "memories": [{
                "content": "The user writes rust for everything new",
                "supersedes": cluster,
                "valid_from": "2026-08-28T09:00:00Z"
            }]
        }))
        .await;
    let survivor = ids(&merged["created"])[0].to_owned();
    let mut closed = ids(&merged["superseded"]);
    closed.sort_unstable();
    let mut expected: Vec<&str> = cluster.iter().map(String::as_str).collect();
    expected.sort_unstable();
    assert_eq!(closed, expected, "all three closed by one call: {merged}");

    // Closed, not forgotten: each keeps its reason and points at the survivor.
    for memory in agmem.memories().await {
        if memory.id.as_str() == survivor {
            continue;
        }
        assert_eq!(
            (
                memory.invalid_reason.map(|reason| reason.as_str()),
                memory.superseded_by.as_ref().map(|id| id.as_str())
            ),
            (Some("superseded"), Some(survivor.as_str())),
            "{} was merged away, not deleted",
            memory.content
        );
    }

    // And the survivor names every wording it replaced, so the merge is
    // readable afterwards rather than only inferable from three closed rows.
    let found = agmem.inspect(&survivor).await;
    let mut names = ids(&found["found"]["memory"]["supersedes"]);
    names.sort_unstable();
    assert_eq!(names, expected, "{found}");
    assert_eq!(
        found["found"]["chain"].as_array().expect("chain").len(),
        4,
        "one walk reaches all three closed members and the survivor: {found}"
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
            json!({ "memories": [{ "content": "a claim", "supersedes": ["nonsense"] }] }),
            "memories[0].supersedes[0]",
        ),
        (
            json!({ "memories": [{ "content": "a claim", "supersedes": ["01M145SMNET1XRYA713EWAQTD3"] }] }),
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
        2,
        "the matching claim and the episode's one chunk compete in a single \
         order; what the query is not about fell off the knee (issue #77): {found}"
    );
    assert!(
        !hit_contents(&found).contains(&"The kitchen tap drips at night"),
        "what the query is not about is not on the page: {found}"
    );
    assert_eq!(
        found["cut"]["kept"].as_u64(),
        Some(2),
        "the page admits the trim: {found}"
    );
    assert_eq!(found["cut"]["considered"].as_u64(), Some(3), "{found}");
    assert!(
        found["cut"]["note"]
            .as_str()
            .is_some_and(|note| note.contains("drop in match quality")),
        "the note says why the tail is gone: {found}"
    );

    let best = &hits(&found)[0];
    assert_eq!(
        best["signals"]["rrf_normalized"].as_f64(),
        Some(1.0),
        "the strongest retrieval hit normalises to 1"
    );
    assert!(
        best["signals"]["similarity"].as_f64().is_some(),
        "a vector-measured hit reports its absolute similarity: {best}"
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
async fn a_query_nothing_matches_abstains_instead_of_filling_the_page() {
    let agmem = Harness::start(Arc::new(AngleEmbedder)).await;
    agmem
        .remember(json!({ "memories": [
            { "content": "black formats the whole workspace" },
            { "content": "black runs in CI on every push" }
        ] }))
        .await;

    // No marker word and no shared term: the vector arm measures 0.0 to
    // everything, the text arms match nothing — yet without the floor the
    // page would still fill, best hit normalised to 1.0 (issue #77).
    let found = agmem
        .recall(json!({ "query": "harbour tides tomorrow" }))
        .await;
    assert_eq!(
        found["hits"].as_array().map(Vec::len),
        Some(0),
        "nothing matched well enough to act on: {found}"
    );
    let cut = &found["cut"];
    assert_eq!(cut["kept"].as_u64(), Some(0), "{found}");
    assert!(
        cut["considered"].as_u64().is_some_and(|count| count >= 1),
        "the abstention says what it considered: {found}"
    );
    assert!(
        cut["best_similarity"]
            .as_f64()
            .is_some_and(|best| best.abs() < 0.01),
        "orthogonal vectors measure ~0.0: {found}"
    );
    assert!(
        cut["note"]
            .as_str()
            .is_some_and(|note| note.contains("not an empty store")
                && note.contains("Ask in different words")),
        "the note carries the next move: {found}"
    );
    assert!(
        found["capped"].is_null() && found["truncated"].is_null(),
        "both describe a page that no longer exists: {found}"
    );

    // The same store, asked something it holds: cosine 0.87 clears the
    // floor, and a two-row page has no knee.
    let found = agmem.recall(json!({ "query": "ruff configuration" })).await;
    assert_eq!(
        found["hits"].as_array().map(Vec::len),
        Some(2),
        "a related question is answered, not abstained: {found}"
    );
    assert!(found["cut"].is_null(), "{found}");
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_temporal_window_rescores_without_hiding_anything() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let stored = agmem
        .remember(json!({ "memories": [{
            "content": "The header colour is blue across the site",
            "valid_from": "2025-01-01T00:00:00Z"
        }] }))
        .await;
    let old = ids(&stored["created"])[0].to_owned();
    agmem
        .remember(json!({ "memories": [{
            "content": "The header colour is green now; blue is retired",
            "valid_from": "2025-06-01T00:00:00Z",
            "supersedes": [old]
        }] }))
        .await;

    let found = agmem
        .recall(json!({
            "query": "header colour",
            "until": "2025-03-01T00:00:00Z",
            "include_invalidated": true
        }))
        .await;
    assert_eq!(
        hits(&found).len(),
        2,
        "soft window: nothing is hidden for missing it: {found}"
    );
    let fit_of = |needle: &str| {
        hits(&found)
            .iter()
            .find(|hit| {
                hit["content"]
                    .as_str()
                    .is_some_and(|text| text.contains(needle))
            })
            .expect("the row is on the page")["signals"]["temporal"]
            .as_f64()
            .expect("a windowed call reports each hit's fit")
    };
    assert_eq!(fit_of("blue across"), 1.0, "in the window: {found}");
    assert!(
        fit_of("green now") < 0.1,
        "months outside the window: {found}"
    );
    let dated = &found["dated"];
    assert_eq!(dated["until"].as_str(), Some("2025-03-01T00:00:00Z"));
    assert!(
        dated["note"].as_str().is_some_and(
            |note| note.contains("rescores rather than filters") && !note.contains("live-only")
        ),
        "the live-only caveat only applies to live reads: {found}"
    );
    assert!(dated["best_fit"].as_f64() == Some(1.0), "{found}");

    // A live read over a past window gets told what a soft window cannot do.
    let live = agmem
        .recall(json!({ "query": "header colour", "until": "2025-03-01T00:00:00Z" }))
        .await;
    assert!(
        live["dated"]["note"]
            .as_str()
            .is_some_and(|note| note.contains("include_invalidated")),
        "{live}"
    );

    // And the everyday path is untouched: no window, no field anywhere.
    let plain = agmem.recall(json!({ "query": "header colour" })).await;
    assert!(plain["dated"].is_null(), "{plain}");
    for hit in hits(&plain) {
        assert!(
            hit["signals"].get("temporal").is_none(),
            "no window, no fit to report: {hit}"
        );
    }
    agmem.shutdown().await;
}

#[tokio::test]
async fn an_inverted_window_is_refused() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let error = agmem
        .call(
            "recall",
            json!({
                "query": "anything",
                "since": "2026-01-01T00:00:00Z",
                "until": "2025-01-01T00:00:00Z"
            }),
        )
        .await
        .expect_err("an empty window honours no intent");
    assert!(
        matches!(&error, ServiceError::McpError(data)
            if data.code == ErrorCode::INVALID_PARAMS
                && data.message.contains("since must not be after until")),
        "{error}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_deployment_with_no_vectors_never_abstains() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({ "memories": [
            { "content": "the payment gateway retries three times" },
            { "content": "the payment gateway settles nightly" }
        ] }))
        .await;

    // BM25-only: nothing is ever measured, and the absence of a measurement
    // is not evidence of irrelevance — the floor must stay off however weak
    // the match.
    let found = agmem.recall(json!({ "query": "gateway retries" })).await;
    assert!(
        found["hits"]
            .as_array()
            .is_some_and(|hits| !hits.is_empty()),
        "text matches stand on their own evidence: {found}"
    );
    assert!(
        found["cut"].is_null(),
        "no measurement, no floor and no knee: {found}"
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
                "supersedes": [old],
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
async fn as_of_reads_only_episodes_that_had_already_happened() {
    // Episodes have no supersession, so before schema v4 they sat outside
    // as-of entirely: a chunk stored today came back — ranked first — for an
    // instant years before its episode occurred.
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({
            "memories": [],
            "episode": {
                "content": "the deploy ran from a laptop",
                "occurred_at": "2026-01-01T00:00:00Z"
            }
        }))
        .await;
    agmem
        .remember(json!({
            "memories": [],
            "episode": {
                "content": "the deploy runs from CI now",
                "occurred_at": "2026-06-01T00:00:00Z"
            }
        }))
        .await;

    let then = agmem
        .recall(json!({ "query": "deploy", "as_of": "2026-03-01T00:00:00Z" }))
        .await;
    assert_eq!(
        hit_contents(&then),
        ["the deploy ran from a laptop"],
        "text recorded after the instant was not known then: {then}"
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

/// Issue #76: no single source may flood a page. Six claims distilled from
/// one episode all match the query; the cap keeps the strongest of them and
/// gives the freed slots to agent-sourced claims ranked below — and the
/// answer admits the cut the way `truncated` admits `k`'s.
#[tokio::test]
async fn one_dominant_episode_cannot_flood_a_page() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let flood = agmem
        .remember(json!({
            "memories": [
                { "content": "The payment gateway retries failed charges three times" },
                { "content": "The payment gateway settles in nightly batches" },
                { "content": "The payment gateway sandbox needs its own API key" },
                { "content": "The payment gateway rejects amounts over ten thousand" },
                { "content": "The payment gateway webhooks are signed with HMAC" },
                { "content": "The payment gateway dashboard lags settlement by a day" }
            ],
            "episode": {
                "content": "A long call about the payment gateway migration."
            }
        }))
        .await;
    let episode = flood["episode"].as_str().expect("the episode id");
    agmem
        .remember(json!({ "memories": [
            { "content": "The payment gateway choice was made for its refund API" },
            { "content": "The team owns the payment gateway integration" },
            { "content": "Invoices reconcile against the payment gateway monthly" }
        ] }))
        .await;

    let page = agmem
        .recall(json!({ "query": "payment gateway", "k": 5 }))
        .await;
    let flood_key = format!("episode:{episode}");
    let from_flood = hits(&page)
        .iter()
        .filter(|hit| hit["source"].as_str() == Some(flood_key.as_str()))
        .count();
    assert!(
        from_flood <= 3,
        "one source holds at most cap(5) = 3 of 5 slots: {page}"
    );
    assert!(
        hits(&page)
            .iter()
            .any(|hit| hit["source"].as_str() == Some("agent")),
        "the freed slots went to hits from elsewhere: {page}"
    );

    let capped = &page["capped"];
    assert_eq!(capped["cap"].as_u64(), Some(3), "{page}");
    assert!(
        capped["displaced"].as_u64().is_some_and(|count| count >= 1),
        "{page}"
    );
    assert_eq!(
        capped["sources"]
            .as_array()
            .expect("sources")
            .iter()
            .map(|source| source.as_str().expect("a string"))
            .collect::<Vec<_>>(),
        [flood_key.as_str()],
        "the answer names the flooder, ready for `inspect`: {page}"
    );

    // A page the cap did not change carries no `capped` — absence must keep
    // meaning "this is exactly the ranking".
    let calm = agmem
        .recall(json!({ "query": "refund API choice", "k": 10 }))
        .await;
    assert!(
        calm["capped"].is_null(),
        "a page nothing was deferred from admits no cut: {calm}"
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
async fn a_historical_read_reinforces_nothing() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({ "memories": [{ "content": "The user prefers Rust" }] }))
        .await;

    // Both audit shapes return the row and leave its ranking state alone
    // (issue #63): "what was believed at T" must not change what is believed
    // to matter now.
    let audit = agmem
        .recall(json!({ "as_of": "2999-01-01T00:00:00Z" }))
        .await;
    assert_eq!(hits(&audit).len(), 1, "{audit}");
    let memory = agmem.memories().await.remove(0);
    assert_eq!(
        (memory.strength, memory.access_count),
        (1.0, 0),
        "an as_of read mutates nothing"
    );

    let everything = agmem.recall(json!({ "include_invalidated": true })).await;
    assert_eq!(hits(&everything).len(), 1, "{everything}");
    assert_eq!(
        agmem.memories().await.remove(0).access_count,
        0,
        "an include_invalidated read mutates nothing"
    );

    let present = agmem.recall(json!({})).await;
    assert_eq!(hits(&present).len(), 1, "{present}");
    assert_eq!(
        agmem.memories().await.remove(0).access_count,
        1,
        "a live-present read still reinforces"
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
            memory["supersedes"] = json!([previous]);
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
                "supersedes": [ids(&first["created"])[0]],
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

/// Issue #66: the confirming call acts on the dry run's snapshot, not on a
/// re-run of the query — a row written between the two calls was never
/// previewed, and forgetting it on the strength of someone else's list is the
/// scope surprise the two-step exists to prevent.
#[tokio::test]
async fn a_confirm_refuses_rows_the_dry_run_never_showed() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(
            json!({ "memories": [{ "content": "the kitchen renovation finished in March" }] }),
        )
        .await;

    let preview = agmem
        .forget(json!({ "query": "kitchen", "dry_run": true }))
        .await;
    assert_eq!(match_ids(&preview["matched"]).len(), 1, "{preview}");

    // A second session's write lands between preview and confirm.
    let landed = agmem
        .remember(json!({ "memories": [{ "content": "the kitchen tiles are still on order" }] }))
        .await;
    let unseen = ids(&landed["created"])[0].to_owned();

    let refused = agmem
        .call("forget", json!({ "query": "kitchen" }))
        .await
        .expect_err("a grown match list must not be acted on");
    assert!(
        refusal(&refused, &unseen) && refusal(&refused, "dry_run"),
        "the refusal names the unpreviewed row and the way forward: {refused}"
    );
    assert_eq!(
        agmem.stats().await.live,
        2,
        "nothing was forgotten on a stale confirmation"
    );

    // The fresh dry run shows both rows; its confirmation goes through.
    let fresh = agmem
        .forget(json!({ "query": "kitchen", "dry_run": true }))
        .await;
    assert_eq!(match_ids(&fresh["matched"]).len(), 2, "{fresh}");
    let done = agmem.forget(json!({ "query": "kitchen" })).await;
    assert_eq!(ids(&done["invalidated"]).len(), 2, "{done}");
    agmem.shutdown().await;
}

/// The other direction of the same discipline: a store that *shrank* between
/// the calls is safe — everything acted on was previewed — so the confirm
/// proceeds on what is left rather than demanding a pointless re-run.
#[tokio::test]
async fn a_confirm_proceeds_when_the_store_only_shrank() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    let written = agmem
        .remember(json!({
            "memories": [
                { "content": "the kitchen renovation finished in March" },
                { "content": "the kitchen budget closed at 12k" }
            ]
        }))
        .await;
    let first = ids(&written["created"])[0].to_owned();

    let preview = agmem
        .forget(json!({ "query": "kitchen", "dry_run": true }))
        .await;
    assert_eq!(match_ids(&preview["matched"]).len(), 2, "{preview}");

    agmem
        .forget(json!({ "ids": [first], "dry_run": false }))
        .await;

    let done = agmem.forget(json!({ "query": "kitchen" })).await;
    assert_eq!(
        ids(&done["invalidated"]).len(),
        1,
        "the surviving previewed row is closed; the already-closed one simply \
         does not come back: {done}"
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
                "supersedes": [format!("memory:{old}")]
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
    // Recalled thirty times. Uncapped, that strength bought roughly 620 days
    // against a class whose unreinforced horizon is twenty; the cap holds the
    // reprieve to five horizons (#52), and 60 days idle sits inside it.
    age(&agmem.db, created[0], 60, 31.0, 30).await;
    // Equally overdue and never used: the prune closes this one on its own.
    age(&agmem.db, created[1], 60, 1.0, 1).await;

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
    assert!(stale[0]["idle_days"].as_f64().expect("a number") > 59.0);
    let reprieve = stale[0]["expires_in_days"].as_f64().expect("a number");
    assert!(
        (30.0..45.0).contains(&reprieve),
        "the reprieve reads off the capped strength — five horizons minus the \
         idle time, not the 559 days the raw strength would claim: {found:#}"
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
async fn consolidate_notes_the_cut_when_it_finds_more_than_one_answer_carries() {
    let agmem = Harness::start(Arc::new(AngleEmbedder)).await;
    // Three claims naming one subject per space: 0°, 20° and 40° are three
    // pairs in the contradiction band, every one under the 0.95 write gate.
    // Seven spaces make 21 candidates, one more than one answer carries —
    // the shape `space: "all"` reaches while each space stays well under
    // the row-fetch cap, so `scanned` confesses nothing (issue #68).
    for space in 1..=7 {
        agmem
            .remember(json!({
                "space": format!("team-{space}"),
                "memories": [
                    { "content": "the user formats python with black",
                      "entities": ["formatter"] },
                    { "content": "python here is formatted by blackfmt",
                      "entities": ["formatter"] },
                    { "content": "formatting runs blake over python",
                      "entities": ["formatter"] },
                ]
            }))
            .await;
    }

    let found = agmem.consolidate(json!({ "space": "all" })).await;
    let contradictions = found["contradictions"].as_array().expect("an array");
    assert_eq!(contradictions.len(), 20, "the cap holds: {found:#}");
    for scan in found["scanned"].as_array().expect("an array") {
        assert_eq!(
            scan["truncated"], false,
            "no space came anywhere near the row-fetch cut: {found:#}"
        );
    }

    let note = found["note"]
        .as_str()
        .expect("a capped answer carries a note");
    assert!(
        note.contains("than one answer carries"),
        "the cut has to be admitted, not silent: {note}"
    );
    assert_eq!(
        found["near_duplicates"].as_array().expect("an array").len(),
        7,
        "one chained cluster per space, nowhere near its own cap: {found:#}"
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
            "supersedes": [uncited.clone()]
        }))
        .await;
    assert_eq!(cited["created"], json!(true), "{cited}");
    assert_eq!(
        cited["derived_from"],
        json!([format!("memory:{evidence}")]),
        "{cited}"
    );
    assert_eq!(cited["superseded"], json!([uncited]), "{cited}");
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

/// A store holding a two-link ownership chain and the decoys around it.
///
/// The shape is `docs/eval/multihop-gate/`'s scenario in four rows: the
/// question matches only the first, whose `entities` name `harbour-crew`, and
/// the row that answers the follow-up shares no word with the question — it
/// is reachable only through that entity.
async fn chain_store() -> Harness {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({
            "memories": [
                { "content": "Atlas ingestion is owned by the harbour crew",
                  "entities": ["atlas", "harbour-crew"] },
                { "content": "Nadia Osei leads that team",
                  "entities": ["harbour-crew", "nadia-osei"] },
                { "content": "Ingestion alerts land in a shared mailbox" },
                { "content": "Nobody owns snacks for standup" }
            ]
        }))
        .await;
    agmem
}

#[tokio::test]
async fn a_chain_row_arrives_without_a_second_call() {
    let agmem = chain_store().await;
    let found = agmem
        .recall(json!({ "query": "who owns atlas ingestion" }))
        .await;
    let hop = hits(&found)
        .iter()
        .find(|hit| hit["content"] == "Nadia Osei leads that team")
        .expect("the row no arm of the query can reach arrives anyway");
    assert!(
        hop["signals"]["rrf"].as_f64().expect("rrf") > 0.0,
        "it holds a real retrieval score, not a zero smuggled past fusion: {hop}"
    );
    assert_eq!(
        hop["entities"],
        json!(["harbour-crew", "nadia-osei"]),
        "and carries the entities the *next* hop would need: {hop}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn the_hop_arm_cannot_outrank_what_matched() {
    let agmem = chain_store().await;
    let found = agmem
        .recall(json!({ "query": "who owns atlas ingestion" }))
        .await;
    let contents = hit_contents(&found);
    assert_eq!(contents.len(), 4, "three matches and one hop row: {found}");
    assert_eq!(
        contents[0], "Atlas ingestion is owned by the harbour crew",
        "the head of the page is still what the query matched best"
    );
    assert_eq!(
        contents.last(),
        Some(&"Nadia Osei leads that team"),
        "a hop-only row sits under every row the query itself matched"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn nothing_to_hop_from_changes_nothing() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    agmem
        .remember(json!({
            "memories": [
                { "content": "The deploy runs from tagged releases only" },
                { "content": "A deploy needs two approvals" },
                { "content": "Lunch is at noon" }
            ]
        }))
        .await;
    let found = agmem.recall(json!({ "query": "deploy approvals" })).await;
    // No row carries entities, so the hop has nothing to seed from: the
    // answer is the primary arm alone, and every fused score is a pure
    // 1 / (60 + rank) with nothing hop-weighted added on.
    assert_eq!(
        hits(&found).len(),
        2,
        "only what the words matched: {found}"
    );
    for (rank, hit) in hits(&found).iter().enumerate() {
        let pure = 1.0 / (60 + rank + 1) as f64;
        let rrf = hit["signals"]["rrf"].as_f64().expect("rrf");
        assert!(
            (rrf - pure).abs() < 1e-9,
            "rank {rank} scores exactly one arm's vote, got {rrf}: {hit}"
        );
    }
    agmem.shutdown().await;
}

#[tokio::test]
async fn an_entity_filter_turns_the_hop_off() {
    let agmem = chain_store().await;
    let found = agmem
        .recall(json!({ "query": "who owns atlas ingestion", "entities": ["atlas"] }))
        .await;
    for hit in hits(&found) {
        assert!(
            hit["entities"]
                .as_array()
                .expect("entities")
                .contains(&json!("atlas")),
            "a caller's entity filter binds every hit, hop rows included: {hit}"
        );
    }
    assert_eq!(
        hit_contents(&found),
        ["Atlas ingestion is owned by the harbour crew"],
        "no hop row leaked past the filter: {found}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_hub_entity_never_seeds() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    // Eight rows carrying the same entity make it the pool's topic — hopping
    // on it would re-fetch the pool — plus one row reachable only through it.
    let mut rows: Vec<Value> = (1..=8)
        .map(|n| {
            json!({
                "content": format!("Atlas checklist item number {n}"),
                "entities": ["atlas"]
            })
        })
        .collect();
    rows.push(json!({ "content": "Nadia keeps the spare keys", "entities": ["atlas"] }));
    agmem.remember(json!({ "memories": rows })).await;

    let found = agmem.recall(json!({ "query": "atlas checklist" })).await;
    assert_eq!(
        hits(&found).len(),
        8,
        "the eight checklist rows, and nothing hopped in: {found}"
    );
    assert!(
        !hit_contents(&found).contains(&"Nadia keeps the spare keys"),
        "a hub entity is the topic, not a link, and never seeds a hop: {found}"
    );
    agmem.shutdown().await;
}

#[tokio::test]
async fn a_full_page_reserves_its_last_slot_for_the_hop() {
    let agmem = Harness::start(Arc::new(NoopEmbedder)).await;
    // Eleven rows match the query's words and only the first names entities,
    // so the chain row enters through the hop alone and lands past a k of 10
    // — issue #43's shape, where the page used to cut the very row the hop
    // fetched for it.
    let mut rows = vec![json!({
        "content": "Atlas ingestion is owned by the harbour crew",
        "entities": ["atlas", "harbour-crew"]
    })];
    rows.extend((1..=10).map(|n| json!({ "content": format!("Atlas backlog note number {n}") })));
    rows.push(json!({
        "content": "Nadia Osei leads that team",
        "entities": ["harbour-crew", "nadia-osei"]
    }));
    agmem.remember(json!({ "memories": rows })).await;

    let found = agmem
        .recall(json!({ "query": "who owns atlas ingestion" }))
        .await;
    let contents = hit_contents(&found);
    assert_eq!(
        contents.len(),
        10,
        "the page still fills exactly k: {found}"
    );
    assert_eq!(
        contents[0], "Atlas ingestion is owned by the harbour crew",
        "the head of the page is untouched"
    );
    assert_eq!(
        contents.last(),
        Some(&"Nadia Osei leads that team"),
        "the hop row holds the reserved last slot instead of being cut: {found}"
    );
    agmem.shutdown().await;
}

/// Issue #29's acceptance, minus the doctor half: a remember → recall
/// roundtrip through the real static model on a fresh store, exercising the
/// 256-wide vector space end to end (every other test runs at a stub's
/// width). Ignored: the first run downloads ~30 MB. Run with
/// `cargo test -p agmem-server --features static --test protocol -- --ignored`.
#[cfg(feature = "static")]
#[tokio::test]
#[ignore = "downloads the static model on first run"]
async fn recall_roundtrips_through_the_static_embedder() {
    let embedder =
        Arc::new(agmem_embed::static_m2v::StaticBackend::new(None).expect("load the static model"));
    let agmem = Harness::start(embedder).await;

    agmem
        .remember(json!({
            "memories": [
                { "content": "The user prefers Rust over Python for CLI tools" },
                { "content": "The kitchen tap drips at night" }
            ]
        }))
        .await;

    let found = agmem
        .recall(json!({ "query": "which language does this person like writing command line programs in" }))
        .await;
    assert_eq!(
        hit_contents(&found).first(),
        Some(&"The user prefers Rust over Python for CLI tools"),
        "the language memory should rank first: {found}"
    );
}
