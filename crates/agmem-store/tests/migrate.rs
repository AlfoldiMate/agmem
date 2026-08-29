//! Migration gate against the real (in-memory) engine.

use agmem_core::SpaceName;
use agmem_store::repo::{self, Lookup};
use agmem_store::{db, migrate};

#[tokio::test]
async fn fresh_store_migrates_then_reruns_as_noop() {
    let conn = db::connect("mem://").await.expect("connect mem://");

    let v = migrate::ensure(&conn).await.expect("first ensure");
    assert_eq!(v, migrate::SCHEMA_VERSION);
    assert_eq!(migrate::current_version(&conn).await.unwrap(), v);

    let again = migrate::ensure(&conn).await.expect("second ensure");
    assert_eq!(again, v, "re-run must be a no-op");
}

#[tokio::test]
async fn the_embedder_is_recorded_once_and_then_enforced() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&conn).await.expect("ensure");

    migrate::ensure_embedder(&conn, "bge-small-en-v1.5-q", 384)
        .await
        .expect("first run records the embedder");
    migrate::ensure_embedder(&conn, "bge-small-en-v1.5-q", 384)
        .await
        .expect("the same embedder is fine");

    let err = migrate::ensure_embedder(&conn, "potion-base-8m", 256)
        .await
        .expect_err("another model must be refused");
    let message = err.to_string();
    assert!(message.contains("bge-small-en-v1.5-q"), "{message}");
    assert!(
        message.contains("--reindex"),
        "must name the remedy: {message}"
    );

    migrate::ensure_embedder(&conn, "none", 0)
        .await
        .expect("BM25-only mode claims no vector space and opens any store");
}

/// A row written before v2 carries no `derived_from` column at all — a
/// `DEFAULT` applies to a write, not to rows already on disk — so the read has
/// to answer "cites nothing" rather than fail on a missing column.
#[tokio::test]
async fn a_row_written_before_v2_reads_as_citing_nothing() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    // The v1 schema on its own, as a store from the previous release carries
    // it: `ensure` then reads version 0 and walks both batches.
    conn.query(include_str!("../src/migrations/v1_schema.surql"))
        .await
        .expect("v1")
        .check()
        .expect("v1 statements");
    conn.query(
        "CREATE memory:ulid() SET space = 'test', kind = 'fact', content_hash = 'v1-1',
             content = 'written before reflect existed', source = { kind: 'agent' }",
    )
    .await
    .expect("seed")
    .check()
    .expect("seed statement");

    assert_eq!(
        migrate::ensure(&conn).await.expect("upgrade"),
        migrate::SCHEMA_VERSION
    );

    let space: SpaceName = "test".parse().expect("valid slug");
    let rows = repo::direct_lookup(&conn, &Lookup::new(vec![space]))
        .await
        .expect("lookup");
    assert_eq!(rows.len(), 1, "the upgrade keeps the row");
    assert!(
        rows[0].derived_from.is_empty(),
        "an old row cites nothing, and reading it must not fail"
    );
}
