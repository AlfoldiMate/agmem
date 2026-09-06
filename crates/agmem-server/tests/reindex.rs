//! `--reindex` end to end, with a stub backend standing in for a model.
//!
//! The acceptance case for issue #28 is "a store built with the noop backend,
//! reindexed to the real model" — run here with a stub of a different width
//! instead, because CI points `AGMEM_MODEL_DIR` somewhere unwritable on
//! purpose and a test that loads the real model is a test that downloads one.
//! Nothing in the pass looks at what a vector *means*, only that every row has
//! one of the width the index was rebuilt at, so the stub proves the same
//! thing the model would.

mod harness;

use std::sync::Arc;

use agmem_core::{Kind, SpaceName, Writer};
use agmem_embed::{EmbedError, Embedder, NoopEmbedder};
use agmem_server::{reindex, startup};
use agmem_store::db::Db;
use agmem_store::repo::{self, Batch, NewChunk, NewEpisode, NewMemory, Search};
use agmem_store::{db, migrate};

/// A deterministic backend of any width: one-hot on the text's byte sum.
struct Stub {
    model: &'static str,
    dim: usize,
    revision: Option<&'static str>,
}

impl Stub {
    fn vector(&self, text: &str) -> Vec<f32> {
        let mut vector = vec![0.0; self.dim];
        let slot = text.bytes().map(usize::from).sum::<usize>() % self.dim;
        vector[slot] = 1.0;
        vector
    }
}

impl Embedder for Stub {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_id(&self) -> &str {
        self.model
    }

    fn revision(&self) -> Option<&str> {
        self.revision
    }

    fn thresholds(&self) -> agmem_core::dedup::Thresholds {
        agmem_core::dedup::Thresholds::BGE_SMALL
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(passages.iter().map(|text| self.vector(text)).collect())
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(self.vector(query))
    }
}

fn stub(model: &'static str, dim: usize) -> Arc<dyn Embedder> {
    Arc::new(Stub {
        model,
        dim,
        revision: None,
    })
}

/// The same backend, claiming a pinned revision.
fn pinned(model: &'static str, dim: usize, revision: &'static str) -> Arc<dyn Embedder> {
    Arc::new(Stub {
        model,
        dim,
        revision: Some(revision),
    })
}

/// A store as v0.2 left it: bge on ONNX Runtime under the id no release
/// since can run, every row vectored at 384. Built with a stub of that id
/// and width, which is indistinguishable from the real thing to everything
/// under test — the move never reads a vector, only clears them.
async fn pre_v03_store() -> Db {
    let db = bm25_only_store().await;
    reindex::execute(&db, stub("bge-small-en-v1.5-q", 384))
        .await
        .expect("embed as v0.2 did");
    assert_eq!(
        repo::reindex::pending_count(&db).await.expect("count"),
        0,
        "the old store is whole under its own model"
    );
    db
}

fn space() -> SpaceName {
    "test".parse().expect("valid slug")
}

/// A migrated store holding two memories and an episode chunk, written the
/// way BM25-only mode writes them: no vectors, and no embedder recorded.
async fn bm25_only_store() -> Db {
    let db = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&db).await.expect("migrate");
    let mut episode = NewEpisode::new("a conversation about languages");
    episode.chunks = vec![NewChunk {
        text: "the user prefers Rust".to_owned(),
        embedding: None,
    }];
    repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: Some(episode),
            memories: vec![
                NewMemory::new(Kind::Fact, "the user prefers Rust over Python"),
                NewMemory::new(Kind::Instruction, "answer in English"),
            ],
            writer: Writer::default(),
        },
    )
    .await
    .expect("seed");
    db
}

/// What a vector-only recall finds: `text` is left unset, so a hit can only
/// have come through the HNSW index.
async fn vector_recall(db: &Db, embedder: &dyn Embedder, query: &str) -> usize {
    let mut search = Search::new(vec![space()]);
    search.vector = Some(embedder.embed_query(query).expect("embed query"));
    repo::search_hybrid(db, &search)
        .await
        .expect("search")
        .len()
}

#[tokio::test]
async fn a_store_written_without_vectors_gets_them() {
    let db = bm25_only_store().await;
    let embedder = stub("stub-8", 8);
    // Not asked through `search_hybrid`, because a store still carrying the
    // schema's 384-wide indexes rejects an 8-wide query vector outright —
    // which is the same engine rule the pass is built around. Before the
    // reindex, every row simply has no vector.
    assert_eq!(
        repo::reindex::pending_count(&db).await.expect("count"),
        3,
        "rows written in BM25-only mode are invisible to a vector recall"
    );

    let report = reindex::execute(&db, Arc::clone(&embedder))
        .await
        .expect("reindex");
    assert_eq!(report.embedded, 3, "two memories and one chunk");
    assert!(report.moved, "the store had no vector space before");

    assert!(
        vector_recall(&db, embedder.as_ref(), "the user prefers Rust over Python").await > 0,
        "and now recall reaches them"
    );
    migrate::ensure_embedder(&db, "stub-8", 8, None)
        .await
        .expect("the startup guard passes afterwards");
}

#[tokio::test]
async fn a_second_run_finds_nothing_to_do() {
    let db = bm25_only_store().await;
    let embedder = stub("stub-8", 8);
    reindex::execute(&db, Arc::clone(&embedder))
        .await
        .expect("first run");

    let again = reindex::execute(&db, embedder).await.expect("second run");
    assert_eq!(again.embedded, 0, "every row already has its vector");
    assert!(!again.moved, "and the store is already in that space");
}

#[tokio::test]
async fn another_model_moves_the_store_and_the_guard_follows_it() {
    let db = bm25_only_store().await;
    reindex::execute(&db, stub("stub-8", 8))
        .await
        .expect("first run");

    let narrower = stub("stub-4", 4);
    migrate::ensure_embedder(&db, "stub-4", 4, None)
        .await
        .expect_err("before the reindex the guard refuses the new model");

    let report = reindex::execute(&db, Arc::clone(&narrower))
        .await
        .expect("reindex to a narrower model");
    assert!(report.moved);
    assert_eq!(
        report.embedded, 3,
        "every row is re-embedded, not just new ones"
    );

    migrate::ensure_embedder(&db, "stub-4", 4, None)
        .await
        .expect("the guard now passes for the new model");
    migrate::ensure_embedder(&db, "stub-8", 8, None)
        .await
        .expect_err("and refuses the old one");
    assert!(vector_recall(&db, narrower.as_ref(), "answer in English").await > 0);
}

/// A run killed between the reset and the end of the embed loop leaves a
/// store whose `meta` already names the new model and whose rows are half
/// converted. Re-running must finish it rather than start over — which is the
/// whole reason the pair is written before the loop.
#[tokio::test]
async fn an_interrupted_run_resumes_where_it_stopped() {
    let db = bm25_only_store().await;
    let embedder = stub("stub-8", 8);

    // The first two phases of `execute`, and then nothing — the crash.
    repo::reindex::reset_vectors(&db, 8).await.expect("reset");
    migrate::set_embedder(&db, "stub-8", 8, None)
        .await
        .expect("record the target");
    let first = repo::reindex::pending(&db, 1).await.expect("one row");
    let vectors = vec![embedder.embed_query(first[0].text()).expect("embed")];
    repo::reindex::write_vectors(&db, first, vectors)
        .await
        .expect("one row lands");

    let report = reindex::execute(&db, Arc::clone(&embedder))
        .await
        .expect("resume");
    assert!(
        !report.moved,
        "the store already knows which model it is being moved to"
    );
    assert_eq!(report.embedded, 2, "only the rows the crash left behind");
    assert_eq!(
        repo::reindex::pending_count(&db).await.expect("count"),
        0,
        "and the store is whole again"
    );
}

/// The upgrade path (issue #138): a v0.2 store opened by a binary whose
/// configured model is another one. The configured model wins — the store is
/// moved on open, sub-second, and every row is left pending for the drain.
#[tokio::test]
async fn opening_a_pre_v03_store_moves_it_and_leaves_every_row_pending() {
    let db = pre_v03_store().await;
    let gemma = stub("stub-768", 768);

    let state = startup::resolve(&db, gemma.as_ref())
        .await
        .expect("the configured model wins; a mismatch is not an error");
    assert_eq!(
        state.moved_from().map(|from| from.model.as_str()),
        Some("bge-small-en-v1.5-q"),
        "the move names what it moved from"
    );
    assert_eq!(state.pending(), 3, "every row is waiting for a new vector");
    migrate::ensure_embedder(&db, "stub-768", 768, None)
        .await
        .expect("meta already names the new model, before a single row is embedded");

    let again = startup::resolve(&db, gemma.as_ref())
        .await
        .expect("a second open");
    assert!(
        again.moved_from().is_none(),
        "an interrupted move resumes: nothing is cleared twice"
    );
    assert_eq!(again.pending(), 3);
}

/// A store recorded under the same id and width but another pinned revision
/// is another space (the weights changed): moved like a retired id. A store
/// that recorded no revision is not — it predates the field, and takes it.
#[tokio::test]
async fn a_moved_pin_moves_the_store_but_a_missing_one_is_backfilled() {
    let db = bm25_only_store().await;
    startup::resolve(&db, stub("stub-8", 8).as_ref())
        .await
        .expect("a backend without a revision records none");

    let state = startup::resolve(&db, pinned("stub-8", 8, "aaaa").as_ref())
        .await
        .expect("the first pinned run");
    assert!(
        state.moved_from().is_none(),
        "no revision on record is not a mismatch"
    );
    assert_eq!(
        migrate::stored_embedder(&db)
            .await
            .expect("read")
            .expect("recorded")
            .revision
            .as_deref(),
        Some("aaaa"),
        "and it is backfilled"
    );

    let state = startup::resolve(&db, pinned("stub-8", 8, "bbbb").as_ref())
        .await
        .expect("another pin");
    assert_eq!(
        state
            .moved_from()
            .and_then(|from| from.revision.clone())
            .as_deref(),
        Some("aaaa"),
        "a different pin of the same id moves the store"
    );
}

/// The drain finishes what the move left, in the background of a store that
/// keeps taking writes, and the counter it reports reaches zero exactly when
/// the store is whole.
#[tokio::test]
async fn the_drain_finishes_the_move_while_writes_keep_landing() {
    let db = pre_v03_store().await;
    // Enough rows that the drain takes several batches.
    let extra = (0..40)
        .map(|n| NewMemory::new(Kind::Fact, format!("fact number {n} about the user")))
        .collect();
    repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: None,
            memories: extra,
            writer: Writer::default(),
        },
    )
    .await
    .expect("seed more");
    let embedder = stub("stub-8", 8);
    let state = startup::resolve(&db, embedder.as_ref())
        .await
        .expect("move");
    assert_eq!(state.pending(), 43);

    // A `remember` mid-drain writes its vector under the new model at once;
    // the drain must never touch it, and must still end at zero.
    let concurrent = {
        let db = db.clone();
        let embedder = Arc::clone(&embedder);
        async move {
            tokio::task::yield_now().await;
            let mut memory = NewMemory::new(Kind::Fact, "written while the drain ran");
            memory.embedding = Some(
                embedder
                    .embed_passages(&[memory.content.clone()])
                    .expect("embed")[0]
                    .clone(),
            );
            repo::insert_batch(
                &db,
                Batch {
                    space: space(),
                    episode: None,
                    memories: vec![memory],
                    writer: Writer::default(),
                },
            )
            .await
            .expect("a write lands mid-drain");
        }
    };
    tokio::join!(
        reindex::drain(db.clone(), Arc::clone(&embedder), state.clone()),
        concurrent
    );

    assert_eq!(
        state.pending(),
        0,
        "the counter the notice reads ends at zero"
    );
    assert_eq!(
        repo::reindex::pending_count(&db).await.expect("count"),
        0,
        "and the store agrees"
    );
    assert!(vector_recall(&db, embedder.as_ref(), "answer in English").await > 0);
    assert!(
        vector_recall(&db, embedder.as_ref(), "written while the drain ran").await > 0,
        "the concurrent write is reachable too"
    );
}

/// Acceptance item 1 of issue #138, on the wire: a store another backend
/// wrote, opened by this one, answers every tool with a line saying how many
/// rows a vector recall cannot reach yet — and stops saying it once the
/// drain is through. Nothing else tells the agent, which is why it is on
/// every result rather than in a log.
#[tokio::test]
async fn every_tool_result_says_how_many_rows_are_still_to_embed() {
    let db = pre_v03_store().await;
    let embedder = stub("stub-8", 8);
    let harness = harness::Harness::start_on(db.clone(), Arc::clone(&embedder)).await;

    for (tool, arguments) in [
        ("recall", serde_json::json!({ "query": "Rust" })),
        ("context", serde_json::json!({})),
        (
            "remember",
            serde_json::json!({ "memories": [{ "content": "the user likes clear notices" }] }),
        ),
    ] {
        let result = harness
            .call(tool, arguments)
            .await
            .expect("the tool answers");
        let last = harness::texts(&result)
            .pop()
            .expect("at least one text block");
        assert!(
            last.starts_with("notice: 3 row(s) are still being re-embedded for stub-8"),
            "{tool} ends with the notice: {last}"
        );
    }

    // The drain runs on the daemon's runtime in the binary; here it is run to
    // completion by hand, on the same `VectorState` the service reads.
    reindex::drain(db, embedder, harness.vectors.clone()).await;
    let result = harness
        .call("recall", serde_json::json!({ "query": "Rust" }))
        .await
        .expect("recall");
    assert!(
        harness::texts(&result)
            .iter()
            .all(|text| !text.starts_with("notice:")),
        "once every row carries a vector the notice is gone"
    );
}

#[tokio::test]
async fn a_dimensionless_backend_has_nowhere_to_reindex_into() {
    let db = bm25_only_store().await;
    let err = reindex::execute(&db, Arc::new(NoopEmbedder))
        .await
        .expect_err("--embedder none produces no vectors");
    assert!(
        err.to_string().contains("--embedder none"),
        "the refusal names the flag that caused it: {err}"
    );
}

/// The verb itself: `agmem reindex` must reach `reindex::run` rather than
/// start a server, and its report — including the refusal — goes to stderr,
/// because stdout is the MCP wire even in a maintenance pass. The hidden
/// `--reindex` flag from before v0.3.1 still lands on the same path.
#[test]
fn the_verb_runs_the_pass_instead_of_serving() {
    for spelling in [
        &["reindex"][..],
        &["reindex", "--model", "bge-small-en-v1.5"][..],
        &["--reindex"][..],
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_agmem"))
            .args(["--db", "mem://", "--embedder", "none", "--data"])
            .arg(dir.path())
            .args(spelling)
            .output()
            .expect("run agmem reindex");

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "a dimensionless backend has no space to reindex into ({spelling:?}): {stderr}"
        );
        assert!(out.stdout.is_empty(), "stdout stays the MCP wire: {stderr}");
        assert!(
            stderr.contains("agmem reindex") && stderr.contains("--embedder none"),
            "the report and the reason both go to stderr ({spelling:?}): {stderr}"
        );
    }
}

/// The same path with the real model behind it: ignored, because it needs the
/// model on disk and CI's cache is unwritable on purpose.
///
/// `cargo test -p agmem-server --test reindex -- --ignored`
#[test]
#[ignore = "loads the real embedding model"]
fn the_verb_reindexes_a_real_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_agmem"))
        .arg("--data")
        .arg(dir.path())
        .arg("reindex")
        .output()
        .expect("run agmem reindex");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(out.stdout.is_empty(), "stdout stays the MCP wire: {stderr}");
    assert!(stderr.contains("row(s) re-embedded"), "got: {stderr}");
}
