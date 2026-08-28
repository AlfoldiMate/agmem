//! The v1 schema against the real query engine.
//!
//! `mem://` is the same engine as `surrealkv://` minus the disk, so an index
//! that parses and serves a query here does the same on a user's machine —
//! which is the point: index syntax errors and planner surprises surface in
//! CI, not at the first `recall`.

use agmem_store::db::Db;
use agmem_store::{db, migrate};
use surrealdb::types::SurrealValue;

const SPACE: &str = "test";

/// A one-hot vector of the schema's width. Distinct axes sit at cosine
/// distance 1 from each other and 0 from themselves, so KNN order is exact.
fn axis(n: usize) -> Vec<f32> {
    let mut vector = vec![0.0; migrate::EMBEDDING_DIM];
    vector[n] = 1.0;
    vector
}

/// A migrated store holding one episode with three chunks and three memories;
/// one chunk and one memory deliberately carry no embedding.
async fn seeded() -> Db {
    let db = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&db).await.expect("migrate");
    db.query(
        "CREATE space:ulid() SET name = $space;
         CREATE episode:ulid() SET space = $space, content_hash = 'ep-1',
             content = 'the cat sat on the mat; the dog barked at the postman';
         LET $ep = (SELECT VALUE id FROM ONLY episode WHERE content_hash = 'ep-1' LIMIT 1);
         CREATE episode_chunk:ulid() SET episode = $ep, space = $space, position = 0,
             text = 'the cat sat on the mat', embedding = $v0;
         CREATE episode_chunk:ulid() SET episode = $ep, space = $space, position = 1,
             text = 'the dog barked at the postman', embedding = $v1;
         CREATE episode_chunk:ulid() SET episode = $ep, space = $space, position = 2,
             text = 'nobody wrote this one down';
         CREATE memory:ulid() SET space = $space, kind = 'fact', content_hash = 'm-1',
             content = 'the user prefers Rust over Python',
             entities = ['user'], tags = ['pref'], embedding = $v0,
             source = { kind: 'agent' };
         CREATE memory:ulid() SET space = $space, kind = 'lesson', content_hash = 'm-2',
             content = 'cargo builds fail when the disk cache is cold',
             entities = ['cargo'], embedding = $v1,
             source = { kind: 'episode', ref: $ep };
         CREATE memory:ulid() SET space = $space, kind = 'instruction', content_hash = 'm-3',
             content = 'answer in English', decay_class = 'pinned',
             source = { kind: 'external', ref: 'https://example.com' };",
    )
    .bind(("space", SPACE))
    .bind(("v0", axis(0)))
    .bind(("v1", axis(1)))
    .await
    .expect("seed query")
    .check()
    .expect("seed statements");
    db
}

/// The planner's chosen plan as text; asserting on it is how we prove a query
/// rides the index instead of scanning the table.
async fn plan_for(db: &Db, query: &str) -> String {
    let mut resp = db
        .query(query)
        .bind(("q", axis(0)))
        .await
        .expect("explain query")
        .check()
        .expect("explain statements");
    let plan: Vec<serde_json::Value> = resp.take(0).expect("plan rows");
    serde_json::Value::from(plan).to_string()
}

/// The engine's complaint about `statement`, or `None` if it was accepted.
async fn rejection(db: &Db, statement: &str) -> Option<String> {
    match db.query(statement).await.expect("query").check() {
        Ok(_) => None,
        Err(err) => Some(err.to_string()),
    }
}

/// Rows come back through `SurrealValue`, not serde — the 3.x client maps
/// query results onto its own value type.
#[derive(SurrealValue)]
struct Hit {
    content: String,
    score: f32,
    highlight: String,
}

#[derive(SurrealValue)]
struct ChunkHit {
    text: String,
    score: f32,
}

#[derive(SurrealValue)]
struct Neighbour {
    content: String,
    dist: f32,
}

#[derive(SurrealValue)]
struct ChunkNeighbour {
    text: String,
    dist: f32,
}

#[tokio::test]
async fn fulltext_index_scores_and_highlights_memories() {
    let db = seeded().await;

    let mut resp = db
        .query(
            "SELECT content, search::score(1) AS score,
                    search::highlight('<em>', '</em>', 1) AS highlight
             FROM memory WHERE content @1@ 'prefers rust' ORDER BY score DESC",
        )
        .await
        .expect("fulltext query")
        .check()
        .expect("fulltext statements");
    let hits: Vec<Hit> = resp.take(0).expect("hits");

    assert_eq!(hits.len(), 1, "only one memory mentions Rust");
    assert_eq!(hits[0].content, "the user prefers Rust over Python");
    assert!(hits[0].score > 0.0, "BM25 scored {}", hits[0].score);
    assert!(
        hits[0].highlight.contains("<em>Rust</em>"),
        "HIGHLIGHTS must mark the term: {}",
        hits[0].highlight
    );

    let plan = plan_for(
        &db,
        "SELECT id FROM memory WHERE content @1@ 'rust' EXPLAIN",
    )
    .await;
    assert!(
        plan.contains("mem_ft"),
        "must use the fulltext index: {plan}"
    );
}

#[tokio::test]
async fn english_analyzer_stems_chunk_queries() {
    let db = seeded().await;

    let mut resp = db
        .query(
            "SELECT text, search::score(1) AS score
             FROM episode_chunk WHERE text @1@ 'cats' ORDER BY score DESC",
        )
        .await
        .expect("fulltext query")
        .check()
        .expect("fulltext statements");
    let hits: Vec<ChunkHit> = resp.take(0).expect("hits");

    assert_eq!(hits.len(), 1, "'cats' must stem to 'cat'");
    assert_eq!(hits[0].text, "the cat sat on the mat");
    assert!(hits[0].score > 0.0, "BM25 scored {}", hits[0].score);

    let plan = plan_for(
        &db,
        "SELECT id FROM episode_chunk WHERE text @1@ 'cat' EXPLAIN",
    )
    .await;
    assert!(
        plan.contains("ec_ft"),
        "must use the fulltext index: {plan}"
    );
}

#[tokio::test]
async fn hnsw_knn_ranks_memories_and_skips_unembedded_rows() {
    let db = seeded().await;

    let mut resp = db
        .query(
            "SELECT content, vector::distance::knn() AS dist
             FROM memory WHERE embedding <|10,40|> $q ORDER BY dist",
        )
        .bind(("q", axis(0)))
        .await
        .expect("knn query")
        .check()
        .expect("knn statements");
    let hits: Vec<Neighbour> = resp.take(0).expect("neighbours");

    assert_eq!(hits.len(), 2, "the instruction has no vector to match");
    assert_eq!(hits[0].content, "the user prefers Rust over Python");
    assert!(hits[0].dist < 1e-6, "same axis, got {}", hits[0].dist);
    assert!(
        (hits[1].dist - 1.0).abs() < 1e-6,
        "orthogonal axis, got {}",
        hits[1].dist
    );

    let plan = plan_for(
        &db,
        "SELECT id FROM memory WHERE embedding <|10,40|> $q EXPLAIN",
    )
    .await;
    assert!(plan.contains("mem_vec"), "must use the HNSW index: {plan}");
}

#[tokio::test]
async fn hnsw_knn_ranks_episode_chunks() {
    let db = seeded().await;

    let mut resp = db
        .query(
            "SELECT text, vector::distance::knn() AS dist
             FROM episode_chunk WHERE embedding <|10,40|> $q ORDER BY dist",
        )
        .bind(("q", axis(1)))
        .await
        .expect("knn query")
        .check()
        .expect("knn statements");
    let hits: Vec<ChunkNeighbour> = resp.take(0).expect("neighbours");
    let texts: Vec<String> = hits.into_iter().map(|hit| hit.text).collect();

    assert_eq!(
        texts,
        ["the dog barked at the postman", "the cat sat on the mat"],
        "nearest axis first, unembedded chunk absent"
    );

    let plan = plan_for(
        &db,
        "SELECT id FROM episode_chunk WHERE embedding <|10,40|> $q EXPLAIN",
    )
    .await;
    assert!(plan.contains("ec_vec"), "must use the HNSW index: {plan}");
}

#[tokio::test]
async fn entity_and_tag_indexes_serve_contains_lookups() {
    let db = seeded().await;

    let mut resp = db
        .query(
            "SELECT VALUE content FROM memory WHERE entities CONTAINS 'user';
             SELECT VALUE content FROM memory WHERE tags CONTAINS 'pref';",
        )
        .await
        .expect("lookup query")
        .check()
        .expect("lookup statements");
    let by_entity: Vec<String> = resp.take(0).expect("entity hits");
    let by_tag: Vec<String> = resp.take(1).expect("tag hits");

    assert_eq!(by_entity, ["the user prefers Rust over Python"]);
    assert_eq!(by_tag, by_entity);

    let plan = plan_for(
        &db,
        "SELECT id FROM memory WHERE entities CONTAINS 'user' EXPLAIN",
    )
    .await;
    assert!(
        plan.contains("mem_entities"),
        "CONTAINS must ride the array index: {plan}"
    );
}

#[tokio::test]
async fn record_ids_are_ulids() {
    let db = seeded().await;

    let mut resp = db
        .query("SELECT VALUE record::id(id) FROM memory")
        .await
        .expect("id query")
        .check()
        .expect("id statements");
    let ids: Vec<String> = resp.take(0).expect("ids");

    assert_eq!(ids.len(), 3);
    for id in ids {
        assert_eq!(id.len(), 26, "ULIDs are 26 chars: {id}");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "Crockford base32: {id}"
        );
    }
}

#[tokio::test]
async fn constraints_reject_duplicates_and_unknown_enum_values() {
    let db = seeded().await;

    let duplicate = rejection(
        &db,
        "CREATE memory:ulid() SET space = 'test', kind = 'fact', content = 'dup',
             content_hash = 'm-1', source = { kind: 'agent' }",
    )
    .await
    .expect("duplicate (space, content_hash) must be rejected");
    assert!(duplicate.contains("mem_hash"), "{duplicate}");

    assert_eq!(
        rejection(
            &db,
            "CREATE memory:ulid() SET space = 'other', kind = 'fact', content = 'dup',
                 content_hash = 'm-1', source = { kind: 'agent' }",
        )
        .await,
        None,
        "the same hash in another space is a different memory"
    );

    for (field, statement) in [
        (
            "kind",
            "CREATE memory:ulid() SET space = 'test', kind = 'note', content = 'x',
                 content_hash = 'bad-1', source = { kind: 'agent' }",
        ),
        (
            "decay_class",
            "CREATE memory:ulid() SET space = 'test', kind = 'fact', content = 'x',
                 content_hash = 'bad-2', decay_class = 'glacial', source = { kind: 'agent' }",
        ),
        (
            "invalid_reason",
            "CREATE memory:ulid() SET space = 'test', kind = 'fact', content = 'x',
                 content_hash = 'bad-3', invalid_reason = 'because', source = { kind: 'agent' }",
        ),
        (
            "source",
            "CREATE memory:ulid() SET space = 'test', kind = 'fact', content = 'x',
                 content_hash = 'bad-4', source = { kind: 'telepathy' }",
        ),
    ] {
        let err = rejection(&db, statement)
            .await
            .unwrap_or_else(|| panic!("{field} must be constrained"));
        assert!(err.contains(field), "{field}: {err}");
    }
}
