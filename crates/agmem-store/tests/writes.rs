//! The write path against the real query engine.
//!
//! `mem://` is the same engine as `surrealkv://` minus the disk, so the things
//! that can only be proven against a live planner — that a duplicate is a
//! reported result rather than an aborted transaction, that a rejected row
//! takes the whole batch down with it — are proven here rather than assumed.

use agmem_core::{DecayClass, Derivation, InvalidReason, Kind, MemoryId, Source, SpaceName};
use agmem_store::db::Db;
use agmem_store::repo::{
    self, Batch, Forget, Liveness, Lookup, NewChunk, NewEpisode, NewMemory, Written,
};
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
async fn registering_a_space_twice_leaves_one_row() {
    let db = store().await;
    for _ in 0..2 {
        repo::ensure_space(&db, &space())
            .await
            .expect("ensure space");
    }
    repo::ensure_space(&db, &"other".parse().expect("valid slug"))
        .await
        .expect("ensure space");

    assert_eq!(
        column::<String>(&db, "SELECT VALUE name FROM space ORDER BY name").await,
        ["other", "test"],
        "startup runs every launch, so the registry must not grow"
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
    correction.supersedes = vec![old.clone()];
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
            "SELECT VALUE array::join(array::map(supersedes, |$l| record::id($l)), ',')
             FROM memory WHERE array::len(supersedes) > 0"
        )
        .await,
        [old.to_string()],
        "and the successor points back"
    );
}

#[tokio::test]
async fn one_claim_closes_a_whole_duplicate_cluster() {
    let db = store().await;
    let cluster = repo::insert_batch(
        &db,
        batch(vec![
            NewMemory::new(Kind::Fact, "deploys go out on Friday"),
            NewMemory::new(Kind::Fact, "the team deploys on Fridays"),
            NewMemory::new(Kind::Fact, "Friday is deploy day"),
        ]),
    )
    .await
    .expect("the cluster")
    .memories
    .into_iter()
    .map(Written::into_id)
    .collect::<Vec<_>>();

    let mut merged = NewMemory::new(Kind::Fact, "the team deploys every Friday");
    merged.supersedes = cluster.clone();
    let outcome = repo::insert_batch(&db, batch(vec![merged]))
        .await
        .expect("the merge");
    let survivor = outcome.memories[0].id().clone();

    assert_eq!(outcome.superseded, cluster, "all three were closed");
    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE content FROM memory WHERE invalid_at IS NONE"
        )
        .await,
        ["the team deploys every Friday"],
        "one wording survives the merge"
    );
    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE record::id(superseded_by) FROM memory
             WHERE invalid_reason = 'superseded' ORDER BY content"
        )
        .await,
        vec![survivor.to_string(); 3],
        "every closed member points at the one survivor — none of them was forgotten"
    );

    // The whole point of a merge over three `forget` calls: the history is
    // still walkable, in both directions, from any member.
    let chain = repo::history_chain(&db, &space(), &cluster[0])
        .await
        .expect("the chain");
    assert_eq!(
        chain.len(),
        2,
        "one closed member and the claim that replaced it"
    );
    assert_eq!(chain[1].id, survivor);
    assert_eq!(chain[1].supersedes, cluster, "the survivor names all three");
}

#[tokio::test]
async fn a_merge_is_refused_whole_when_one_member_does_not_exist() {
    let db = store().await;
    let live = repo::insert_batch(
        &db,
        batch(vec![NewMemory::new(Kind::Fact, "deploys go out on Friday")]),
    )
    .await
    .expect("the live row")
    .memories
    .remove(0)
    .into_id();

    let mut merged = NewMemory::new(Kind::Fact, "the team deploys every Friday");
    merged.supersedes = vec![
        live.clone(),
        MemoryId::new("01M145SMNET1XRYA713EWAQTD3").expect("a ULID"),
    ];
    let error = repo::insert_batch(&db, batch(vec![merged]))
        .await
        .expect_err("one member is not in this space");
    assert!(
        matches!(error, StoreError::UnknownMemory { .. }),
        "{error:?} names the missing id"
    );
    assert_eq!(
        column::<String>(
            &db,
            "SELECT VALUE content FROM memory WHERE invalid_at IS NONE"
        )
        .await,
        ["deploys go out on Friday"],
        "and nothing was closed or written"
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
        correction.supersedes = vec![target.clone()];
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

/// Every memory a space holds, closed ones included.
async fn all_memories(db: &Db, space: &SpaceName) -> Vec<agmem_core::MemoryRecord> {
    let mut lookup = Lookup::new(vec![space.clone()]);
    lookup.liveness = Liveness::Any;
    repo::direct_lookup(db, &lookup).await.expect("lookup")
}

/// Write one memory into a space of its own and hand back its id.
async fn elsewhere(db: &Db, space: &SpaceName, content: &str) -> MemoryId {
    repo::insert_batch(
        db,
        Batch {
            space: space.clone(),
            episode: None,
            memories: vec![NewMemory::new(Kind::Fact, content)],
        },
    )
    .await
    .expect("write")
    .memories
    .remove(0)
    .into_id()
}

#[tokio::test]
async fn a_soft_forget_closes_what_is_live_and_leaves_the_rest_alone() {
    let db = store().await;
    let outcome = repo::insert_batch(
        &db,
        batch(vec![
            NewMemory::new(Kind::Fact, "the office is on the third floor"),
            NewMemory::new(Kind::Fact, "the office is on the fourth floor"),
            NewMemory::new(Kind::Lesson, "the lift is out on Tuesdays"),
        ]),
    )
    .await
    .expect("write");
    let ids: Vec<MemoryId> = outcome.memories.into_iter().map(Written::into_id).collect();
    let (third, fourth, lift) = (ids[0].clone(), ids[1].clone(), ids[2].clone());
    repo::supersede(&db, &space(), &third, &fourth)
        .await
        .expect("correct the floor");

    let other: SpaceName = "elsewhere".parse().expect("valid slug");
    let outsider = elsewhere(&db, &other, "another space's business").await;

    let forgotten = repo::forget(
        &db,
        &Forget {
            spaces: vec![space()],
            memories: vec![third.clone(), fourth.clone(), outsider.clone()],
            episodes: Vec::new(),
            purge: false,
        },
    )
    .await
    .expect("forget");

    assert_eq!(
        forgotten.memories,
        vec![fourth.clone()],
        "only the live row in this space moved: the corrected one is already closed, \
         and the outsider is outside the space guard"
    );
    assert!(forgotten.episodes.is_empty());
    assert_eq!(forgotten.chunks, 0);

    let rows = all_memories(&db, &space()).await;
    let reason = |id: &MemoryId| {
        rows.iter()
            .find(|row| row.id == *id)
            .expect("the row is still there")
            .invalid_reason
    };
    assert_eq!(
        reason(&third),
        Some(InvalidReason::Superseded),
        "a forget never rewrites another close"
    );
    assert_eq!(reason(&fourth), Some(InvalidReason::Forgotten));
    assert_eq!(reason(&lift), None, "nothing it was not asked about moved");
    assert_eq!(
        all_memories(&db, &other).await[0].invalid_reason,
        None,
        "an id is a capability inside its space, not across the store"
    );
}

#[tokio::test]
async fn a_purge_takes_an_episodes_slices_and_leaves_the_claims_drawn_from_it() {
    let db = store().await;
    let outcome = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: Some(NewEpisode {
                content: "I like Rust. Python is fine too.".to_owned(),
                occurred_at: None,
                session: None,
                chunks: vec![
                    NewChunk {
                        text: "I like Rust.".to_owned(),
                        embedding: None,
                    },
                    NewChunk {
                        text: "Python is fine too.".to_owned(),
                        embedding: None,
                    },
                ],
            }),
            memories: vec![NewMemory::new(Kind::Fact, "the user prefers Rust")],
        },
    )
    .await
    .expect("write");
    let episode = outcome.episode.expect("an episode was written").into_id();
    let claim = outcome.memories[0].id().clone();

    let other: SpaceName = "elsewhere".parse().expect("valid slug");
    let outsider = elsewhere(&db, &other, "another space's business").await;

    let forgotten = repo::forget(
        &db,
        &Forget {
            spaces: vec![space()],
            memories: vec![outsider.clone()],
            episodes: vec![episode.clone()],
            purge: true,
        },
    )
    .await
    .expect("purge");

    assert_eq!(forgotten.episodes, vec![episode.clone()]);
    assert_eq!(forgotten.chunks, 2, "the slices go with the text");
    assert!(
        forgotten.memories.is_empty(),
        "the space guard holds on the destructive path too"
    );

    let stats = repo::stats(&db, &space()).await.expect("stats");
    assert_eq!(
        (stats.episodes, stats.chunks, stats.memories),
        (0, 0, 1),
        "the text is gone and the claim distilled from it is not"
    );
    assert_eq!(
        all_memories(&db, &space()).await[0].id,
        claim,
        "purging text does not purge what was learned from it"
    );
    assert_eq!(
        all_memories(&db, &other).await.len(),
        1,
        "the outsider was never in scope"
    );
}

/// Backdate a row's last use and set the strength a few recalls would have
/// left it with — the two inputs to the decay curve, and the only two no write
/// API sets directly.
async fn age(db: &Db, id: &MemoryId, days: i64, strength: f64) {
    db.query(
        "UPDATE type::record('memory', $id)
         SET last_accessed = time::now() - duration::from_secs($idle),
             strength = $strength",
    )
    .bind(("id", id.to_string()))
    .bind(("idle", days * 86_400))
    .bind(("strength", strength))
    .await
    .expect("age the row")
    .check()
    .expect("statements");
}

#[tokio::test]
async fn the_startup_prune_expires_stale_working_context_and_nothing_else() {
    let db = store().await;
    let working = |content: &str| {
        let mut memory = NewMemory::new(Kind::Fact, content);
        memory.decay_class = Some(DecayClass::Fast);
        memory
    };
    let outcome = repo::insert_batch(
        &db,
        batch(vec![
            working("the build is running in the second terminal"),
            working("the branch under review is called spike"),
            working("the failing test is called roundtrip"),
            // Their kinds' default classes: normal, slow, pinned.
            NewMemory::new(Kind::Fact, "the office is on the third floor"),
            NewMemory::new(Kind::Lesson, "the lift is out on Tuesdays"),
            NewMemory::new(Kind::Instruction, "answer in English"),
        ]),
    )
    .await
    .expect("write");
    let ids: Vec<MemoryId> = outcome.memories.into_iter().map(Written::into_id).collect();
    let (stale, reinforced, fresh) = (ids[0].clone(), ids[1].clone(), ids[2].clone());

    age(&db, &stale, 30, 1.0).await;
    age(&db, &reinforced, 30, 5.0).await;
    age(&db, &fresh, 3, 1.0).await;
    for slower in &ids[3..] {
        age(&db, slower, 400, 1.0).await;
    }

    let closed = repo::prune_expired(&db).await.expect("prune");

    assert_eq!(
        closed,
        vec![stale.clone()],
        "only the working note nothing has touched in twenty days"
    );
    let rows = all_memories(&db, &space()).await;
    let row = |id: &MemoryId| {
        rows.iter()
            .find(|row| row.id == *id)
            .expect("the row is still there")
    };
    assert_eq!(
        row(&stale).invalid_reason,
        Some(InvalidReason::Expired),
        "closed, not deleted: the history is still readable"
    );
    assert!(row(&stale).invalid_at.is_some());
    assert_eq!(
        row(&reinforced).invalid_reason,
        None,
        "five recalls buy a working note five times the horizon"
    );
    assert_eq!(row(&fresh).invalid_reason, None);
    for slower in &ids[3..] {
        assert_eq!(
            row(slower).invalid_reason,
            None,
            "only `fast` has a TTL — the rest end at a correction or a forget"
        );
    }
}

#[tokio::test]
async fn the_prune_reaches_a_row_reinforced_past_the_cap() {
    let db = store().await;
    let mut memory = NewMemory::new(Kind::Fact, "the demo login is on the whiteboard");
    memory.decay_class = Some(DecayClass::Fast);
    let hot = repo::insert_batch(&db, batch(vec![memory]))
        .await
        .expect("write")
        .memories
        .remove(0)
        .into_id();

    // Fifty recalls' worth of strength, idle past the capped horizon (five
    // times ~20 days) but far inside the uncapped one (~3 years): the row
    // issue #52 was filed about, written before the ceiling existed.
    age(&db, &hot, 150, 50.0).await;

    assert_eq!(
        repo::prune_expired(&db).await.expect("prune"),
        vec![hot],
        "the sweep clamps strength in its comparison, so no migration is needed"
    );
}

#[tokio::test]
async fn a_second_prune_moves_nothing_the_first_one_left() {
    let db = store().await;
    let working = |content: &str| {
        let mut memory = NewMemory::new(Kind::Fact, content);
        memory.decay_class = Some(DecayClass::Fast);
        memory
    };
    let ids: Vec<MemoryId> = repo::insert_batch(
        &db,
        batch(vec![
            working("the build is running in the second terminal"),
            working("the branch under review is called spike"),
        ]),
    )
    .await
    .expect("write")
    .memories
    .into_iter()
    .map(Written::into_id)
    .collect();
    let (stale, retired) = (ids[0].clone(), ids[1].clone());

    repo::forget(
        &db,
        &Forget {
            spaces: vec![space()],
            memories: vec![retired.clone()],
            episodes: Vec::new(),
            purge: false,
        },
    )
    .await
    .expect("forget");
    age(&db, &stale, 30, 1.0).await;
    age(&db, &retired, 30, 1.0).await;

    assert_eq!(
        repo::prune_expired(&db).await.expect("first prune"),
        vec![stale.clone()],
        "the row a forget already closed is not expired a second time"
    );
    let after_first: Vec<(MemoryId, Option<InvalidReason>, Option<jiff::Timestamp>)> =
        all_memories(&db, &space())
            .await
            .into_iter()
            .map(|row| (row.id, row.invalid_reason, row.invalid_at))
            .collect();

    assert!(
        repo::prune_expired(&db)
            .await
            .expect("second prune")
            .is_empty(),
        "a start with nothing new to expire is a no-op"
    );
    let after_second: Vec<(MemoryId, Option<InvalidReason>, Option<jiff::Timestamp>)> =
        all_memories(&db, &space())
            .await
            .into_iter()
            .map(|row| (row.id, row.invalid_reason, row.invalid_at))
            .collect();

    assert_eq!(
        after_first, after_second,
        "no date moves on a second run: a close happens once"
    );
    assert_eq!(
        after_first
            .iter()
            .find(|(id, ..)| id == &retired)
            .expect("the forgotten row")
            .1,
        Some(InvalidReason::Forgotten),
        "and it keeps the reason it was closed for"
    );
}

#[tokio::test]
async fn a_reflection_carries_its_citations_and_they_read_back_typed() {
    let db = store().await;
    let seeded = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: Some(NewEpisode::new("cargo build failed on a cold cache")),
            memories: vec![NewMemory::new(
                Kind::Fact,
                "cargo builds fail when the disk cache is cold",
            )],
        },
    )
    .await
    .expect("seed the evidence");
    let evidence = seeded.memories[0].id().clone();
    let episode = seeded.episode.expect("an episode was written").into_id();

    let mut insight = NewMemory::new(
        Kind::Lesson,
        "warm the cargo cache before timing a build on this machine",
    );
    insight.derived_from = vec![
        Derivation::Memory(evidence.clone()),
        Derivation::Episode(episode.clone()),
    ];
    let written = repo::insert_batch(&db, batch(vec![insight]))
        .await
        .expect("write the reflection");
    let id = written.memories[0].id().clone();

    let stored = all_memories(&db, &space())
        .await
        .into_iter()
        .find(|row| row.id == id)
        .expect("the reflection");
    assert_eq!(
        stored.derived_from,
        vec![Derivation::Memory(evidence), Derivation::Episode(episode),],
        "citations keep the order they were written in, and each one knows \
         which table it points at"
    );
    assert!(
        all_memories(&db, &space())
            .await
            .into_iter()
            .filter(|row| row.id != id)
            .all(|row| row.derived_from.is_empty()),
        "everything else cites nothing"
    );
}

#[tokio::test]
async fn locate_says_which_table_each_id_belongs_to() {
    let db = store().await;
    let seeded = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: Some(NewEpisode::new("the verbatim text")),
            memories: vec![NewMemory::new(Kind::Fact, "the distilled claim")],
        },
    )
    .await
    .expect("seed");
    let memory = seeded.memories[0].id().clone();
    let episode = seeded.episode.expect("an episode").into_id();

    let other: SpaceName = "other".parse().expect("valid slug");
    let stranger = elsewhere(&db, &other, "a claim in another space").await;

    let asked = vec![
        episode.to_string(),
        memory.to_string(),
        stranger.to_string(),
        "01M145SMNET1XRYA713EWAQTD3".to_owned(),
    ];
    assert_eq!(
        repo::locate(&db, &[space()], &asked).await.expect("locate"),
        vec![
            Some(Derivation::Episode(episode)),
            Some(Derivation::Memory(memory)),
            // An id is a capability inside its space: one from another space
            // is as unknown here as one that names nothing at all.
            None,
            None,
        ]
    );
    assert!(
        repo::locate(&db, &[space()], &[])
            .await
            .expect("no ids")
            .is_empty(),
        "nothing asked about is nothing to ask the store"
    );
}
