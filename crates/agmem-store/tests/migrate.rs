//! Migration gate against the real (in-memory) engine.

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
