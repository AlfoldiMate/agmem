//! Migration gate against the real (in-memory) engine.

use agmem_core::{EpisodeId, SpaceName, Writer};
use agmem_store::repo::{self, Forget, Lookup};
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

/// An episode written before v9 carries none of the document columns and
/// never gains them: it reads as anonymous, which is true (issue #132). The
/// purge path must still reach it, and the memory distilled from it — which
/// has no `spans` sidecar either — must still read.
#[tokio::test]
async fn an_episode_written_before_v9_reads_as_anonymous_and_still_purges() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    conn.query(include_str!("../src/migrations/v1_schema.surql"))
        .await
        .expect("v1")
        .check()
        .expect("v1 statements");
    conn.query(
        "CREATE episode:01M145SMNET1XRYA713EWAQTD5 SET space = 'test',
             content_hash = 'v1-ep', content = 'recorded before documents existed';
         CREATE episode_chunk:ulid() SET episode = episode:01M145SMNET1XRYA713EWAQTD5,
             space = 'test', position = 0, text = 'recorded before documents existed';
         CREATE memory:01M145SMNET1XRYA713EWAQTD6 SET space = 'test', kind = 'fact',
             content_hash = 'v1-m', content = 'documents did not exist yet',
             source = { kind: 'episode', ref: episode:01M145SMNET1XRYA713EWAQTD5 }",
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
    let episode: EpisodeId = "01M145SMNET1XRYA713EWAQTD5".parse().expect("a ULID");
    let detail = repo::episode(&conn, &space, &episode)
        .await
        .expect("a pre-v9 episode still reads");
    assert_eq!(detail.chunks.len(), 1);
    assert_eq!(
        detail.derived.len(),
        1,
        "the claim drawn from it still reads with no spans sidecar"
    );

    let forgotten = repo::forget(
        &conn,
        &Forget {
            spaces: vec![space.clone()],
            memories: vec![],
            episodes: vec![episode.clone()],
            purge: true,
        },
    )
    .await
    .expect("purge a pre-v9 episode");
    assert_eq!(forgotten.episodes, vec![episode]);
    assert_eq!(forgotten.chunks, 1);
    let stats = repo::stats(&conn, &space).await.expect("stats");
    assert_eq!((stats.episodes, stats.chunks, stats.memories), (0, 0, 1));
}

/// The engine's complaint about `statement`, or `None` if it was accepted.
async fn rejection(db: &db::Db, statement: &str) -> Option<String> {
    match db.query(statement).await.expect("query").check() {
        Ok(_) => None,
        Err(err) => Some(err.to_string()),
    }
}

/// The planner's chosen plan as text — proof a query rides an index.
async fn plan_for(db: &db::Db, query: &str) -> String {
    let mut resp = db
        .query(query)
        .await
        .expect("explain query")
        .check()
        .expect("explain statements");
    let plan: Vec<serde_json::Value> = resp.take(0).expect("plan rows");
    serde_json::Value::from(plan).to_string()
}

/// A document is an episode with a name and a kind (issue #132): the kind is
/// an enum the schema enforces, and lookups by title or tag ride their own
/// indexes rather than scanning the table.
#[tokio::test]
async fn a_document_carries_its_name_and_kind_and_the_title_index_serves_it() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&conn).await.expect("migrate");

    assert_eq!(
        rejection(
            &conn,
            "CREATE episode:ulid() SET space = 'test', content_hash = 'doc-1',
                 content = 'the plan for the widget', title = 'widget plan',
                 doc_kind = 'plan', tags = ['role:architect'], mime = 'text/markdown'",
        )
        .await,
        None,
        "a fully described document is accepted"
    );
    let err = rejection(
        &conn,
        "CREATE episode:ulid() SET space = 'test', content_hash = 'doc-2',
             content = 'dear diary', title = 'diary', doc_kind = 'diary'",
    )
    .await
    .expect("an unknown doc_kind must be rejected");
    assert!(err.contains("doc_kind"), "{err}");

    let mut resp = conn
        .query(
            "SELECT VALUE title FROM episode WHERE space = 'test' AND title = 'widget plan';
             SELECT VALUE title FROM episode WHERE tags CONTAINS 'role:architect';",
        )
        .await
        .expect("lookups")
        .check()
        .expect("lookup statements");
    let by_title: Vec<String> = resp.take(0).expect("title hits");
    let by_tag: Vec<String> = resp.take(1).expect("tag hits");
    assert_eq!(by_title, ["widget plan"]);
    assert_eq!(by_tag, by_title);

    let plan = plan_for(
        &conn,
        "SELECT id FROM episode WHERE space = 'test' AND title = 'widget plan' EXPLAIN",
    )
    .await;
    assert!(
        plan.contains("ep_title"),
        "a title lookup must ride ep_title: {plan}"
    );
    let plan = plan_for(
        &conn,
        "SELECT id FROM episode WHERE tags CONTAINS 'role:architect' EXPLAIN",
    )
    .await;
    assert!(
        plan.contains("ep_tags"),
        "a tag lookup must ride ep_tags: {plan}"
    );
}

/// The citation span is a typed sidecar on the memory (issue #132): each
/// element names an episode and two char offsets, and a memory with no
/// sidecar at all is still a memory the write path can reinforce.
#[tokio::test]
async fn a_span_sidecar_is_typed() {
    let conn = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&conn).await.expect("migrate");
    conn.query(
        "CREATE episode:01M145SMNET1XRYA713EWAQTD5 SET space = 'test',
             content_hash = 'doc-1', content = 'the user prefers Rust over Python',
             title = 'preferences', doc_kind = 'report'",
    )
    .await
    .expect("seed")
    .check()
    .expect("seed statement");

    assert_eq!(
        rejection(
            &conn,
            "CREATE memory:ulid() SET space = 'test', kind = 'fact', content_hash = 'm-1',
                 content = 'the user prefers Rust', source = { kind: 'agent' },
                 spans = [{ ref: episode:01M145SMNET1XRYA713EWAQTD5, start: 0, end: 21 }]",
        )
        .await,
        None,
        "a span naming an episode with two char offsets is accepted"
    );
    for (field, statement) in [
        (
            "ref",
            "CREATE memory:ulid() SET space = 'test', kind = 'fact', content_hash = 'm-2',
                 content = 'x', source = { kind: 'agent' },
                 spans = [{ ref: memory:01M145SMNET1XRYA713EWAQTD5, start: 0, end: 1 }]",
        ),
        (
            "start",
            "CREATE memory:ulid() SET space = 'test', kind = 'fact', content_hash = 'm-3',
                 content = 'x', source = { kind: 'agent' },
                 spans = [{ ref: episode:01M145SMNET1XRYA713EWAQTD5, start: 'x', end: 1 }]",
        ),
    ] {
        let err = rejection(&conn, statement)
            .await
            .unwrap_or_else(|| panic!("spans.*.{field} must be typed"));
        assert!(err.contains(field), "{field}: {err}");
    }

    // A memory written with no sidecar is the common case; reinforcing it is
    // the UPDATE that re-coerces every field, and it has to land.
    let space: SpaceName = "test".parse().expect("valid slug");
    let outcome = repo::insert_batch(
        &conn,
        repo::Batch {
            space: space.clone(),
            episode: None,
            memories: vec![repo::NewMemory::new(
                agmem_core::Kind::Fact,
                "no sidecar here",
            )],
            writer: Writer::default(),
        },
    )
    .await
    .expect("write");
    let id = outcome.memories[0].id().clone();
    repo::reinforce(&conn, &[id])
        .await
        .expect("reinforcing a spanless row lands");
}
