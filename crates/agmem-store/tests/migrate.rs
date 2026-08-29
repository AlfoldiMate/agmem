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

/// v1 and v2 wrote `supersedes` as a single record link. v3 redefines the
/// column as a list, and a redefined TYPE — like a DEFAULT — does not touch
/// rows already on disk, so the migration has to rewrite them: the one link
/// becomes a one-element list, and everything else the empty one. Without
/// that, `array::map` over the column errors on every old row and the read
/// path fails on stores it must keep opening.
#[tokio::test]
async fn a_correction_written_before_v3_reads_as_a_one_element_list() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    conn.query(include_str!("../src/migrations/v1_schema.surql"))
        .await
        .expect("v1")
        .check()
        .expect("v1 statements");
    conn.query(
        "CREATE memory:01M145SMNET1XRYA713EWAQTD3 SET space = 'test', kind = 'fact',
             content_hash = 'v1-old', content = 'the user prefers Python',
             source = { kind: 'agent' }, invalid_at = time::now(),
             invalid_reason = 'superseded',
             superseded_by = memory:01M145SMNET1XRYA713EWAQTD4;
         CREATE memory:01M145SMNET1XRYA713EWAQTD4 SET space = 'test', kind = 'fact',
             content_hash = 'v1-new', content = 'the user prefers Rust',
             source = { kind: 'agent' },
             supersedes = memory:01M145SMNET1XRYA713EWAQTD3",
    )
    .await
    .expect("seed")
    .check()
    .expect("seed statements");

    assert_eq!(
        migrate::ensure(&conn).await.expect("upgrade"),
        migrate::SCHEMA_VERSION
    );

    let space: SpaceName = "test".parse().expect("valid slug");
    let mut rows = repo::direct_lookup(&conn, &Lookup::new(vec![space.clone()]))
        .await
        .expect("lookup");
    rows.sort_by(|left, right| left.content.cmp(&right.content));
    assert_eq!(rows.len(), 1, "only the correction is live");
    assert_eq!(
        rows[0]
            .supersedes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["01M145SMNET1XRYA713EWAQTD3"],
        "the single link became a one-element list"
    );

    // And a row that never superseded anything reads as an empty list rather
    // than as a missing column.
    let old = "01M145SMNET1XRYA713EWAQTD3".parse().expect("a ULID");
    let chain = repo::history_chain(&conn, &space, &old)
        .await
        .expect("the chain still walks");
    assert_eq!(chain.len(), 2, "both links, in one walk");
    assert!(chain[0].supersedes.is_empty(), "the oldest closed nothing");
}

/// A fresh store adopts its first backend's width (issue #29): recording a
/// 256-wide embedder redefines the HNSW indexes the v1 schema bakes at 384,
/// so the first write lands instead of failing with "Incorrect vector
/// dimension". Only the first recording adopts — the pair then guards.
#[tokio::test]
async fn a_fresh_store_adopts_the_first_embedders_width() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&conn).await.expect("ensure");

    migrate::ensure_embedder(&conn, "potion-base-8M", 256)
        .await
        .expect("first run records the pair and adopts its width");

    let mut components = vec!["1".to_owned()];
    components.resize(256, "0".to_owned());
    conn.query(format!(
        "CREATE memory:ulid() SET space = 'test', kind = 'fact', content_hash = 'w-1',
             content = 'stored at the adopted width', source = {{ kind: 'agent' }},
             embedding = [{}]",
        components.join(",")
    ))
    .await
    .expect("write")
    .check()
    .expect("a 256-wide vector lands in the redefined index");

    migrate::ensure_embedder(&conn, "potion-base-8M", 256)
        .await
        .expect("the same pair is still fine");
    let err = migrate::ensure_embedder(&conn, "bge-small-en-v1.5-q", 384)
        .await
        .expect_err("the baked width is now the wrong one");
    assert!(err.to_string().contains("potion-base-8M"), "{err}");
}
