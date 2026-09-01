//! Re-embedding against the real engine.
//!
//! The order the reindex pass runs in is not a preference — it is what the
//! HNSW index will accept — so the negative cases here are the point: they
//! are what says the clear cannot be skipped.

use agmem_core::{Kind, SpaceName, Writer};
use agmem_store::db::Db;
use agmem_store::repo::{self, Batch, NewChunk, NewEpisode, NewMemory};
use agmem_store::{StoreError, db, migrate};

/// A migrated, empty store.
async fn store() -> Db {
    let db = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&db).await.expect("migrate");
    db
}

fn space() -> SpaceName {
    "test".parse().expect("valid slug")
}

/// A one-hot vector of the given width.
fn axis(width: usize, n: usize) -> Vec<f32> {
    let mut vector = vec![0.0; width];
    vector[n % width] = 1.0;
    vector
}

/// Two memories and one episode chunk — every vectored row the schema has.
/// `width` of `None` writes them the way BM25-only mode does, with no vectors
/// at all. Returns how many rows carry a vector column.
async fn seed(db: &Db, width: Option<usize>) -> usize {
    let vector = |n: usize| width.map(|w| axis(w, n));
    let mut fact = NewMemory::new(Kind::Fact, "the user prefers Rust over Python");
    fact.embedding = vector(0);
    let mut instruction = NewMemory::new(Kind::Instruction, "answer in English");
    instruction.embedding = vector(1);
    let mut episode = NewEpisode::new("a conversation about languages");
    episode.chunks = vec![NewChunk {
        text: "the user prefers Rust".to_owned(),
        embedding: vector(2),
    }];
    repo::insert_batch(
        db,
        Batch {
            writer: Writer::default(),
            space: space(),
            episode: Some(episode),
            memories: vec![fact, instruction],
        },
    )
    .await
    .expect("seed");
    3
}

/// Ask the vector index directly: this is the assertion that the redefinition
/// took, because a KNN operator with an `ef` needs an HNSW index to run at
/// all, and the index is what carries the width.
async fn knn(db: &Db, vector: &[f32]) -> Vec<String> {
    let literal = vector
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut resp = db
        .query(format!(
            "SELECT VALUE record::id(id) FROM memory WHERE embedding <|2,20|> [{literal}]"
        ))
        .await
        .expect("knn query")
        .check()
        .expect("knn statement");
    resp.take(0).expect("ids")
}

#[tokio::test]
async fn a_new_width_is_refused_until_the_vectors_are_cleared() {
    let db = store().await;
    seed(&db, Some(migrate::EMBEDDING_DIM)).await;

    // The engine checks every write against the live index definition, which
    // is the whole reason the pass clears before it redefines.
    let mut narrow = NewMemory::new(Kind::Fact, "written by another model");
    narrow.embedding = Some(axis(8, 0));
    let err = repo::insert_batch(
        &db,
        Batch {
            writer: Writer::default(),
            space: space(),
            episode: None,
            memories: vec![narrow],
        },
    )
    .await
    .expect_err("a 384-wide index must refuse an 8-wide vector");
    assert!(
        err.to_string().contains("dimension"),
        "the engine names the width it expected: {err}"
    );
}

#[tokio::test]
async fn the_reset_moves_every_table_to_the_new_width() {
    let db = store().await;
    let rows = seed(&db, Some(migrate::EMBEDDING_DIM)).await;

    repo::reindex::reset_vectors(&db, 8).await.expect("reset");
    assert_eq!(
        repo::reindex::pending_count(&db).await.expect("count"),
        rows,
        "clearing the vectors is what makes every row pending"
    );

    let batch = repo::reindex::pending(&db, 64).await.expect("pending");
    assert_eq!(batch.len(), rows, "both tables, in one page");
    assert!(
        batch
            .iter()
            .any(|row| row.text() == "the user prefers Rust"),
        "an episode chunk is embedded from its text, not its episode's"
    );

    let vectors: Vec<Vec<f32>> = (0..batch.len()).map(|n| axis(8, n)).collect();
    repo::reindex::write_vectors(&db, batch, vectors)
        .await
        .expect("write vectors");
    assert_eq!(
        repo::reindex::pending_count(&db).await.expect("count"),
        0,
        "nothing is left to do"
    );

    let hits = knn(&db, &axis(8, 0)).await;
    assert_eq!(
        hits.len(),
        2,
        "the rebuilt index serves the new width: {hits:?}"
    );
}

#[tokio::test]
async fn an_interrupted_pass_is_exactly_the_rows_it_did_not_reach() {
    let db = store().await;
    let rows = seed(&db, Some(migrate::EMBEDDING_DIM)).await;
    repo::reindex::reset_vectors(&db, 8).await.expect("reset");

    let first = repo::reindex::pending(&db, 1).await.expect("one row");
    assert_eq!(first.len(), 1, "the page size is honoured");
    repo::reindex::write_vectors(&db, first, vec![axis(8, 0)])
        .await
        .expect("write one");

    assert_eq!(
        repo::reindex::pending_count(&db).await.expect("count"),
        rows - 1,
        "the rows without vectors are the whole record of what is left"
    );
    let left = repo::reindex::pending(&db, 64).await.expect("pending");
    assert_eq!(left.len(), rows - 1, "and asking again returns only those");
}

#[tokio::test]
async fn a_shortchanged_batch_lands_nowhere() {
    let db = store().await;
    seed(&db, Some(migrate::EMBEDDING_DIM)).await;
    repo::reindex::reset_vectors(&db, 8).await.expect("reset");

    let batch = repo::reindex::pending(&db, 64).await.expect("pending");
    let sent = batch.len();
    let err = repo::reindex::write_vectors(&db, batch, vec![axis(8, 0)])
        .await
        .expect_err("a backend that returned one vector for three passages");
    assert!(
        matches!(err, StoreError::VectorCount { want, got } if want == sent && got == 1),
        "{err}"
    );
    assert_eq!(
        repo::reindex::pending_count(&db).await.expect("count"),
        sent,
        "half a batch must not land"
    );
}

/// A row written before v3 is missing columns a later `UPDATE` re-coerces, and
/// clearing the vectors is an `UPDATE` over every memory in the store. The
/// migration is what makes that safe, so the two have to be exercised in the
/// order the binary runs them.
#[tokio::test]
async fn a_store_from_before_v3_reindexes_after_it_migrates() {
    let db = db::connect("mem://").await.expect("connect mem://");
    db.query(include_str!("../src/migrations/v1_schema.surql"))
        .await
        .expect("v1")
        .check()
        .expect("v1 statements");
    db.query(
        "CREATE memory:01M145SMNET1XRYA713EWAQTD3 SET space = 'test', kind = 'fact',
             content_hash = 'v1-old', content = 'written before reflect existed',
             source = { kind: 'agent' }",
    )
    .await
    .expect("seed")
    .check()
    .expect("seed statement");

    migrate::ensure(&db).await.expect("upgrade");
    repo::reindex::reset_vectors(&db, 8).await.expect("reset");
    let batch = repo::reindex::pending(&db, 64).await.expect("pending");
    assert_eq!(batch.len(), 1, "the old row is pending like any other");
    repo::reindex::write_vectors(&db, batch, vec![axis(8, 0)])
        .await
        .expect("an old row takes a new vector");
    assert_eq!(knn(&db, &axis(8, 0)).await.len(), 1);
}
