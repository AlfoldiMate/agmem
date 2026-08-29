//! `--reindex` end to end, with a stub backend standing in for a model.
//!
//! The acceptance case for issue #28 is "a store built with the noop backend,
//! reindexed to fastembed" — run here with a stub of a different width
//! instead, because CI points `FASTEMBED_CACHE_DIR` somewhere unwritable on
//! purpose and a test that loads the real model is a test that downloads one.
//! Nothing in the pass looks at what a vector *means*, only that every row has
//! one of the width the index was rebuilt at, so the stub proves the same
//! thing the model would.

use std::sync::Arc;

use agmem_core::{Kind, SpaceName};
use agmem_embed::{EmbedError, Embedder, NoopEmbedder};
use agmem_server::reindex;
use agmem_store::db::Db;
use agmem_store::repo::{self, Batch, NewChunk, NewEpisode, NewMemory, Search};
use agmem_store::{db, migrate};

/// A deterministic backend of any width: one-hot on the text's byte sum.
struct Stub {
    model: &'static str,
    dim: usize,
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

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(passages.iter().map(|text| self.vector(text)).collect())
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(self.vector(query))
    }
}

fn stub(model: &'static str, dim: usize) -> Arc<dyn Embedder> {
    Arc::new(Stub { model, dim })
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
    migrate::ensure_embedder(&db, "stub-8", 8)
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
    migrate::ensure_embedder(&db, "stub-4", 4)
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

    migrate::ensure_embedder(&db, "stub-4", 4)
        .await
        .expect("the guard now passes for the new model");
    migrate::ensure_embedder(&db, "stub-8", 8)
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
    migrate::set_embedder(&db, "stub-8", 8)
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

/// The flag itself: it must reach `reindex::run` rather than start a server,
/// and its report — including the refusal — goes to stderr, because stdout is
/// the MCP wire even in a maintenance pass.
#[test]
fn the_flag_runs_the_pass_instead_of_serving() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args([
            "--reindex",
            "--db",
            "mem://",
            "--embedder",
            "none",
            "--data",
        ])
        .arg(dir.path())
        .output()
        .expect("run agmem --reindex");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a dimensionless backend has no space to reindex into: {stderr}"
    );
    assert!(out.stdout.is_empty(), "stdout stays the MCP wire: {stderr}");
    assert!(
        stderr.contains("agmem reindex") && stderr.contains("--embedder none"),
        "the report and the reason both go to stderr: {stderr}"
    );
}

/// The same path with the real model behind it: ignored, because it needs the
/// model on disk and CI's cache is unwritable on purpose.
///
/// `cargo test -p agmem-server --test reindex -- --ignored`
#[test]
#[ignore = "loads the real embedding model"]
fn the_flag_reindexes_a_real_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_agmem"))
        .args(["--reindex", "--data"])
        .arg(dir.path())
        .output()
        .expect("run agmem --reindex");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    assert!(out.stdout.is_empty(), "stdout stays the MCP wire: {stderr}");
    assert!(stderr.contains("row(s) re-embedded"), "got: {stderr}");
}
