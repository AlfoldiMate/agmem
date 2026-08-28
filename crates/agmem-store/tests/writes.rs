//! The write path against the real query engine.
//!
//! `mem://` is the same engine as `surrealkv://` minus the disk, so the things
//! that can only be proven against a live planner — that a duplicate is a
//! reported result rather than an aborted transaction, that a rejected row
//! takes the whole batch down with it — are proven here rather than assumed.

use agmem_core::{Kind, MemoryId, Source, SpaceName};
use agmem_store::db::Db;
use agmem_store::repo::{self, Batch, NewChunk, NewEpisode, NewMemory, Written};
use agmem_store::{StoreError, db, migrate};
use surrealdb::types::SurrealValue;

/// A migrated, empty store.
async fn store() -> Db {
    let db = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&db).await.expect("migrate");
    db
}

fn space() -> SpaceName {
    "test".parse().expect("valid slug")
}

/// A one-hot vector of the schema's width; the HNSW indexes reject any other.
fn axis(n: usize) -> Vec<f32> {
    let mut vector = vec![0.0; migrate::EMBEDDING_DIM];
    vector[n] = 1.0;
    vector
}

/// Every value a single-column projection returned.
async fn column<T: SurrealValue>(db: &Db, query: &str) -> Vec<T> {
    let mut resp = db
        .query(query)
        .await
        .expect("query")
        .check()
        .expect("statements");
    resp.take(0).expect("rows")
}

fn batch(memories: Vec<NewMemory>) -> Batch {
    Batch {
        space: space(),
        episode: None,
        memories,
    }
}

#[tokio::test]
async fn episode_chunks_and_memories_commit_together() {
    let db = store().await;
    let mut distilled = NewMemory::new(Kind::Fact, "the user prefers Rust over Python");
    distilled.entities = vec!["user".to_owned()];
    distilled.tags = vec!["identity".to_owned()];
    distilled.embedding = Some(axis(0));
    let mut imported = NewMemory::new(Kind::Instruction, "answer in English");
    imported.source = Some(Source::External {
        origin: "https://example.com".to_owned(),
    });

    let outcome = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: Some(NewEpisode {
                content: "I like Rust. Python is fine too.".to_owned(),
                occurred_at: None,
                session: Some("s-1".to_owned()),
                chunks: vec![
                    NewChunk {
                        text: "I like Rust.".to_owned(),
                        embedding: Some(axis(1)),
                    },
                    NewChunk {
                        text: "Python is fine too.".to_owned(),
                        embedding: None,
                    },
                ],
            }),
            memories: vec![distilled, imported],
        },
    )
    .await
    .expect("batch");

    let episode = match outcome.episode.as_ref().expect("an episode was written") {
        Written::Created(id) => id.clone(),
        Written::Duplicate(id) => panic!("fresh store returned a duplicate: {id}"),
    };
    assert!(outcome.memories.iter().all(Written::is_created));
    assert!(outcome.superseded.is_empty());

    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE text FROM episode_chunk ORDER BY position"
        )
        .await,
        ["I like Rust.", "Python is fine too."],
        "chunks keep the order they were given"
    );
    assert_eq!(
        column::<String>(&db, "SELECT VALUE record::id(episode) FROM episode_chunk").await,
        [episode.as_str(), episode.as_str()],
        "every chunk links to the episode written in the same transaction"
    );
    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE <string> source.ref FROM memory ORDER BY content"
        )
        .await,
        [
            "https://example.com".to_owned(),
            format!("episode:{episode}")
        ],
        "a memory with no source of its own is provenanced to the batch's episode"
    );
    assert_eq!(
        column::<String>(&db, "SELECT VALUE decay_class FROM memory ORDER BY content").await,
        ["pinned", "normal"],
        "decay class falls back to the one the kind implies"
    );
    assert_eq!(
        column::<i64>(
            &db,
            "SELECT VALUE array::len(embedding ?? []) FROM memory ORDER BY content"
        )
        .await,
        [0, migrate::EMBEDDING_DIM as i64],
        "the vector is stored as given, and its absence stays absent"
    );
}

#[tokio::test]
async fn an_exact_duplicate_reports_the_id_that_already_holds_it() {
    let db = store().await;
    let first = repo::insert_batch(
        &db,
        batch(vec![NewMemory::new(Kind::Fact, "the user prefers Rust")]),
    )
    .await
    .expect("first write");
    let id = first.memories[0].id().clone();
    assert!(first.memories[0].is_created());

    // Normalization folds case and whitespace, so this is the same claim.
    let second = repo::insert_batch(
        &db,
        batch(vec![NewMemory::new(
            Kind::Lesson,
            "The   user\nprefers  RUST",
        )]),
    )
    .await
    .expect("second write");

    assert_eq!(second.memories, vec![Written::Duplicate(id.clone())]);
    assert_eq!(
        column::<String>(&db, "SELECT VALUE content FROM memory").await,
        ["the user prefers Rust"],
        "the stored row is untouched, not overwritten"
    );

    let within = repo::insert_batch(
        &db,
        batch(vec![
            NewMemory::new(Kind::Fact, "a brand new claim"),
            NewMemory::new(Kind::Fact, "a brand new claim"),
        ]),
    )
    .await
    .expect("third write");
    let fresh = within.memories[0].id().clone();
    assert_eq!(
        within.memories,
        vec![Written::Created(fresh.clone()), Written::Duplicate(fresh)],
        "two identical memories inside one batch collapse to one row"
    );
    assert_eq!(
        column::<String>(&db, "SELECT VALUE record::id(id) FROM memory")
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn a_repeated_episode_is_reused_rather_than_re_chunked() {
    let db = store().await;
    let episode = || NewEpisode {
        content: "I like Rust.".to_owned(),
        occurred_at: None,
        session: None,
        chunks: vec![NewChunk {
            text: "I like Rust.".to_owned(),
            embedding: None,
        }],
    };
    let first = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: Some(episode()),
            memories: vec![NewMemory::new(Kind::Fact, "the user likes Rust")],
        },
    )
    .await
    .expect("first write");
    let id = first.episode.expect("episode").into_id();

    let second = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: Some(episode()),
            memories: vec![NewMemory::new(Kind::Fact, "the user dislikes Python")],
        },
    )
    .await
    .expect("second write");

    assert_eq!(second.episode, Some(Written::Duplicate(id.clone())));
    assert_eq!(
        column::<String>(&db, "SELECT VALUE text FROM episode_chunk").await,
        ["I like Rust."],
        "the chunks of an episode are written once"
    );
    assert_eq!(
        column::<String>(&db, "SELECT VALUE <string> source.ref FROM memory").await,
        [format!("episode:{id}"), format!("episode:{id}")],
        "the second batch is provenanced to the episode that already existed"
    );
}

#[tokio::test]
async fn supersession_closes_the_old_row_and_keeps_it_readable() {
    let db = store().await;
    let old = repo::insert_batch(
        &db,
        batch(vec![NewMemory::new(Kind::Fact, "the user prefers Python")]),
    )
    .await
    .expect("first write")
    .memories
    .remove(0)
    .into_id();

    let boundary: jiff::Timestamp = "2026-08-28T09:00:00Z".parse().expect("timestamp");
    let mut correction = NewMemory::new(Kind::Fact, "the user prefers Rust");
    correction.supersedes = Some(old.clone());
    correction.valid_from = Some(boundary);
    let outcome = repo::insert_batch(&db, batch(vec![correction]))
        .await
        .expect("correction");
    let new = outcome.memories[0].id().clone();

    assert_eq!(outcome.superseded, vec![old.clone()]);
    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE content FROM memory WHERE invalid_at IS NONE"
        )
        .await,
        ["the user prefers Rust"],
        "only the correction is live"
    );
    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE string::concat(invalid_reason, ' ', record::id(superseded_by),
                 ' ', <string> invalid_at)
             FROM memory WHERE invalid_at IS NOT NONE"
        )
        .await,
        [format!("superseded {new} 2026-08-28T09:00:00Z")],
        "the closed row still reads back, pointing forward at its successor"
    );
    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE record::id(supersedes) FROM memory WHERE supersedes IS NOT NONE"
        )
        .await,
        [old.to_string()],
        "and the successor points back"
    );
}

#[tokio::test]
async fn supersede_runs_on_its_own_too() {
    let db = store().await;
    let mut written = repo::insert_batch(
        &db,
        batch(vec![
            NewMemory::new(Kind::Fact, "the user prefers Python"),
            NewMemory::new(Kind::Fact, "the user prefers Rust"),
        ]),
    )
    .await
    .expect("write")
    .memories;
    let new = written.remove(1).into_id();
    let old = written.remove(0).into_id();

    repo::supersede(&db, &space(), &old, &new)
        .await
        .expect("supersede");

    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE content FROM memory WHERE invalid_at IS NONE"
        )
        .await,
        ["the user prefers Rust"]
    );
    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE record::id(superseded_by) FROM memory WHERE invalid_at IS NOT NONE"
        )
        .await,
        [new.to_string()]
    );
}

#[tokio::test]
async fn one_rejected_row_rolls_the_whole_batch_back() {
    let db = store().await;
    let mut malformed = NewMemory::new(Kind::Fact, "a vector of the wrong width");
    // The HNSW index is defined at one dimension; anything else is rejected.
    malformed.embedding = Some(vec![0.0, 1.0, 0.0]);

    let error = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: Some(NewEpisode::new("verbatim ground truth")),
            memories: vec![
                NewMemory::new(Kind::Fact, "a perfectly good claim"),
                malformed,
            ],
        },
    )
    .await
    .expect_err("the bad vector must take the batch down");

    assert!(
        error.to_string().contains("vector dimension"),
        "the reported error must be the one that explains the failure, not the \
         generic follow-on every other statement reports: {error}"
    );
    for table in ["memory", "episode", "episode_chunk"] {
        assert!(
            column::<String>(&db, &format!("SELECT VALUE record::id(id) FROM {table}"))
                .await
                .is_empty(),
            "{table} must be empty after a rolled-back batch"
        );
    }
}

#[tokio::test]
async fn a_supersedes_target_outside_this_space_is_rejected_before_anything_is_written() {
    let db = store().await;
    let elsewhere = repo::insert_batch(
        &db,
        Batch {
            space: "other".parse().expect("valid slug"),
            episode: None,
            memories: vec![NewMemory::new(Kind::Fact, "a claim in another space")],
        },
    )
    .await
    .expect("write")
    .memories
    .remove(0)
    .into_id();

    for target in [
        elsewhere,
        MemoryId::new("01M145SMNET1XRYA713EWAQTD3").expect("valid ulid"),
    ] {
        let mut correction = NewMemory::new(Kind::Fact, "a claim in this space");
        correction.supersedes = Some(target.clone());
        let error = repo::insert_batch(&db, batch(vec![correction]))
            .await
            .expect_err("an unreachable target must be refused");
        assert!(
            matches!(&error, StoreError::UnknownMemory { space, id }
                if space == &self::space() && id == &target),
            "{error}"
        );
    }
    assert!(
        column::<String>(&db, "SELECT VALUE content FROM memory WHERE space = 'test'")
            .await
            .is_empty(),
        "nothing is written when validation refuses the batch"
    );
}
