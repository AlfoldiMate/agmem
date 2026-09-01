//! Migration gate against the real (in-memory) engine.

use agmem_core::{SpaceName, Writer};
use agmem_store::repo::{self, Lookup};
use agmem_store::{StoreError, db, migrate};

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

/// Issue #72: concurrent first runs on one shared store race the
/// check-then-write. However the statements interleave — and a `mem://`
/// engine is free to interleave them anywhere between the read and the
/// guarded write — exactly one pair may land, and every other contender gets
/// the ordinary mismatch refusal rather than silently overwriting the winner.
#[tokio::test]
async fn concurrent_first_runs_record_exactly_one_embedder() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&conn).await.expect("ensure");

    let contenders: Vec<_> = (0..8)
        .map(|n| {
            let db = conn.clone();
            tokio::spawn(async move {
                migrate::ensure_embedder(&db, &format!("contender-{n}"), 384).await
            })
        })
        .collect();

    let mut winners = Vec::new();
    for (n, contender) in contenders.into_iter().enumerate() {
        match contender.await.expect("no contender panics") {
            Ok(()) => winners.push(format!("contender-{n}")),
            Err(StoreError::EmbedderMismatch { stored_model, .. }) => {
                assert!(
                    stored_model.starts_with("contender-"),
                    "a loser is refused with the winner's pair: {stored_model}"
                );
            }
            Err(other) => panic!("losing the race is a mismatch, not: {other}"),
        }
    }
    assert_eq!(winners.len(), 1, "exactly one first run records its pair");

    let (model, dim) = migrate::stored_embedder(&conn)
        .await
        .expect("read the pair")
        .expect("a pair was recorded");
    assert_eq!(model, winners[0], "the store holds the winner's model");
    assert_eq!(dim, 384);
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

    // v5's backfill keyed that closed row by its close time, so its wording
    // is free to be asserted again on the upgraded store (issue #61) — the
    // proof that the migration reached rows written before it existed.
    let outcome = repo::insert_batch(
        &conn,
        repo::Batch {
            writer: Writer::default(),
            space,
            episode: None,
            memories: vec![repo::NewMemory::new(
                agmem_core::Kind::Fact,
                "the user prefers Python",
            )],
        },
    )
    .await
    .expect("re-assert closed pre-v5 content");
    assert!(
        outcome.memories[0].is_created(),
        "a row closed before the upgrade does not answer as the duplicate: {:?}",
        outcome.memories
    );
}

/// A row written before v6 carries no `writer` — the field cannot be
/// backfilled, which is the whole reason it lands early (issue #75) — so it
/// must read as `None` rather than fail or invent a sentinel. And because a
/// SurrealDB UPDATE re-coerces every field (the v3 lesson), the close-path
/// UPDATEs must still reach a writerless row: a required sub-field would make
/// supersession refuse every pre-v6 row.
#[tokio::test]
async fn a_row_written_before_v6_reads_with_no_writer_and_still_closes() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    conn.query(include_str!("../src/migrations/v1_schema.surql"))
        .await
        .expect("v1")
        .check()
        .expect("v1 statements");
    conn.query(
        "CREATE memory:01M145SMNET1XRYA713EWAQTD5 SET space = 'test', kind = 'fact',
             content_hash = 'v1-w', content = 'recorded before writers existed',
             source = { kind: 'agent' }",
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
    let rows = repo::direct_lookup(&conn, &Lookup::new(vec![space.clone()]))
        .await
        .expect("lookup");
    assert_eq!(rows.len(), 1, "the upgrade keeps the row");
    assert!(
        rows[0].writer.is_none(),
        "a pre-v6 row records no writer, and reading it must not fail"
    );

    // Correcting the old row is the UPDATE that re-coerces it; it has to land.
    let mut correction =
        repo::NewMemory::new(agmem_core::Kind::Fact, "recorded after writers existed");
    correction.supersedes = vec!["01M145SMNET1XRYA713EWAQTD5".parse().expect("a ULID")];
    let stamp = Writer {
        client: "test-client".to_owned(),
        client_version: Some("1.2.3".to_owned()),
        session: "session-1".to_owned(),
        tool: "remember".to_owned(),
    };
    let outcome = repo::insert_batch(
        &conn,
        repo::Batch {
            space: space.clone(),
            episode: None,
            memories: vec![correction],
            writer: stamp.clone(),
        },
    )
    .await
    .expect("supersede a writerless row");
    assert!(outcome.memories[0].is_created());

    let rows = repo::direct_lookup(&conn, &Lookup::new(vec![space]))
        .await
        .expect("lookup after the close");
    assert_eq!(rows.len(), 1, "only the correction is live");
    assert_eq!(
        rows[0].writer,
        Some(stamp),
        "the new row carries the writer it was stamped with"
    );
}

/// A row written before v7 carries no `novelty` column, and never gains one —
/// the measurement records the store as it stood at write time, which is gone
/// (issue #83). It must read as `None`, and because a SurrealDB UPDATE
/// re-coerces every field (the v3 lesson), reinforcement must still reach it.
#[tokio::test]
async fn a_row_written_before_v7_reads_with_no_novelty_and_still_reinforces() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    conn.query(include_str!("../src/migrations/v1_schema.surql"))
        .await
        .expect("v1")
        .check()
        .expect("v1 statements");
    conn.query(
        "CREATE memory:01M145SMNET1XRYA713EWAQTD7 SET space = 'test', kind = 'fact',
             content_hash = 'v1-n', content = 'recorded before novelty existed',
             source = { kind: 'agent' }",
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
    let rows = repo::direct_lookup(&conn, &Lookup::new(vec![space.clone()]))
        .await
        .expect("lookup");
    assert_eq!(rows.len(), 1, "the upgrade keeps the row");
    assert!(
        rows[0].novelty.is_none(),
        "a pre-v7 row records no novelty, and reading it must not fail"
    );

    // Reinforcement is the UPDATE that re-coerces the row; it has to land.
    let id = "01M145SMNET1XRYA713EWAQTD7".parse().expect("a ULID");
    let touched = repo::reinforce(&conn, std::slice::from_ref(&id))
        .await
        .expect("reinforce a noveltyless row");
    assert_eq!(touched, 1, "the update reached the pre-v7 row");
}

/// Chunks written before v4 carry no `occurred_at` — the column arrives with
/// that migration — and the as-of clause reads the date off the chunk, so the
/// upgrade has to copy each episode's date down. A chunk left at NONE would
/// silently sit out every as-of recall.
#[tokio::test]
async fn a_chunk_written_before_v4_takes_its_episodes_date() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    conn.query(include_str!("../src/migrations/v1_schema.surql"))
        .await
        .expect("v1")
        .check()
        .expect("v1 statements");
    conn.query(
        "CREATE episode:01M145SMNET1XRYA713EWAQTE1 SET space = 'test',
             content = 'an old conversation', content_hash = 'v1-ep',
             occurred_at = d'2025-05-01T00:00:00Z';
         CREATE episode_chunk:ulid() SET
             episode = episode:01M145SMNET1XRYA713EWAQTE1, space = 'test',
             text = 'an old conversation', position = 0",
    )
    .await
    .expect("seed")
    .check()
    .expect("seed statements");

    assert_eq!(
        migrate::ensure(&conn).await.expect("upgrade"),
        migrate::SCHEMA_VERSION
    );

    let mut resp = conn
        .query("SELECT VALUE <string> occurred_at FROM episode_chunk")
        .await
        .expect("read back");
    let dates: Vec<String> = resp.take(0).expect("dates");
    assert_eq!(
        dates,
        ["2025-05-01T00:00:00Z"],
        "the backfill copies the episode's date onto its chunk"
    );
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
