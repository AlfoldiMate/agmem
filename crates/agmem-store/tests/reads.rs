//! The read path against the real query engine.
//!
//! Retrieval is the one part of agmem that cannot be unit-tested: BM25
//! ranking, HNSW neighbours and `search::rrf` fusion are the engine's, and
//! what matters is that they compose into one order. So the fixture is
//! deliberately literal — one-hot vectors and disjoint vocabulary — which
//! makes every rank below arithmetic rather than a judgement call.
//!
//! One trap the fixture works around: BM25's IDF goes negative and clamps to
//! zero once a term appears in most of the corpus, which would make the
//! fulltext arm's order arbitrary. Every term asserted on here appears in
//! exactly one row of several.

use agmem_core::{EpisodeId, Kind, MemoryId, Source, SpaceName, dedup};
use agmem_store::db::Db;
use agmem_store::repo::{
    self, Batch, Candidate, Filters, Hit, Liveness, Lookup, NewChunk, NewEpisode, NewMemory, Search,
};
use agmem_store::{StoreError, db, migrate};
use jiff::Timestamp;

/// When every seeded memory starts being true.
const SEEDED_AT: &str = "2026-01-01T00:00:00Z";
/// When the correction in the history tests takes over.
const CORRECTED_AT: &str = "2026-06-01T00:00:00Z";

fn space() -> SpaceName {
    "test".parse().expect("valid slug")
}

fn stamp(text: &str) -> Timestamp {
    text.parse().expect("timestamp")
}

/// A one-hot vector of the schema's width. Distinct axes sit at cosine
/// distance 1 from each other and 0 from themselves, so KNN order is exact.
fn axis(n: usize) -> Vec<f32> {
    let mut vector = vec![0.0; migrate::EMBEDDING_DIM];
    vector[n] = 1.0;
    vector
}

fn memory(kind: Kind, content: &str, axis_n: usize) -> NewMemory {
    let mut memory = NewMemory::new(kind, content);
    memory.embedding = Some(axis(axis_n));
    memory.valid_from = Some(stamp(SEEDED_AT));
    memory
}

/// A migrated store holding one episode and five memories, each on its own
/// vector axis and sharing no content words with any other.
async fn seeded() -> Db {
    let db = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&db).await.expect("migrate");

    let mut profile = memory(Kind::Fact, "the user prefers Rust over Python", 5);
    profile.entities = vec!["user".to_owned()];
    profile.tags = vec!["identity".to_owned()];
    let mut garden = memory(Kind::Fact, "gardening in spring requires patience", 0);
    garden.entities = vec!["garden".to_owned()];
    garden.tags = vec!["hobby".to_owned()];
    let mut lesson = memory(
        Kind::Lesson,
        "cargo builds fail when the disk cache is cold",
        6,
    );
    lesson.entities = vec!["cargo".to_owned()];
    let instruction = memory(Kind::Instruction, "answer in English", 7);
    let mut home = memory(Kind::Fact, "the kitchen tap drips at night", 8);
    home.entities = vec!["home".to_owned()];

    repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: Some(NewEpisode {
                content: "a long conversation".to_owned(),
                occurred_at: None,
                session: None,
                chunks: vec![
                    NewChunk {
                        text: "the borrow checker took an hour to explain".to_owned(),
                        embedding: Some(axis(1)),
                    },
                    NewChunk {
                        text: "then somebody made coffee".to_owned(),
                        embedding: Some(axis(9)),
                    },
                    NewChunk {
                        text: "and the meeting ended".to_owned(),
                        embedding: None,
                    },
                ],
            }),
            memories: vec![profile, garden, lesson, instruction, home],
        },
    )
    .await
    .expect("seed batch");
    db
}

/// What each candidate matched on, in fused order.
fn contents(candidates: &[Candidate]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| match &candidate.hit {
            Hit::Memory(memory) => memory.content.clone(),
            Hit::Chunk(chunk) => chunk.text.clone(),
        })
        .collect()
}

/// The live memory in this space whose content starts with `prefix`.
async fn live_id(db: &Db, prefix: &str) -> MemoryId {
    let live = repo::direct_lookup(db, &Lookup::new(vec![space()]))
        .await
        .expect("lookup");
    live.into_iter()
        .find(|memory| memory.content.starts_with(prefix))
        .unwrap_or_else(|| panic!("no live memory starting {prefix:?}"))
        .id
}

#[tokio::test]
async fn a_question_finds_a_claim_that_does_not_contain_every_word_of_it() {
    let db = seeded().await;
    let mut search = Search::new(vec![space()]);
    // `@N@` ANDs the words inside one reference, so this whole query used to
    // match nothing: no row contains "which", "language" or "does". A question
    // always carries a word the answer does not, and `recall`'s description
    // asks agents to ask in words — so under AND the fulltext arm was empty on
    // nearly every real call (issue #39).
    search.text = Some("which language does the user prefer?".to_owned());
    search.vector = None;
    search.episodes = false;

    let found = contents(&repo::search_hybrid(&db, &search).await.expect("search"));
    assert!(
        found.contains(&"the user prefers Rust over Python".to_owned()),
        "the claim shares 'the', 'user' and 'prefer' with the question: {found:?}"
    );
    assert_eq!(
        found.first().map(String::as_str),
        Some("the user prefers Rust over Python"),
        "and matching three terms outranks matching one: {found:?}"
    );
}

#[tokio::test]
async fn a_query_with_no_words_in_it_is_not_a_fulltext_arm() {
    let db = seeded().await;
    let mut search = Search::new(vec![space()]);
    search.text = Some("?!  —  ...".to_owned());
    search.vector = None;

    assert!(
        repo::search_hybrid(&db, &search)
            .await
            .expect("search")
            .is_empty(),
        "punctuation yields no terms, and a request with no arms matches \
         nothing rather than everything"
    );
}

#[tokio::test]
async fn fusion_surfaces_a_keyword_only_and_a_vector_only_match_together() {
    let db = seeded().await;
    let mut search = Search::new(vec![space()]);
    // "Rust" is in the profile memory alone, whose vector is orthogonal to the
    // query's; the query vector *is* the gardening memory's, which shares no
    // word with the text. Neither arm can find both.
    search.text = Some("Rust".to_owned());
    search.vector = Some(axis(0));
    search.episodes = false;

    let hits = repo::search_hybrid(&db, &search).await.expect("search");
    let found = contents(&hits);

    assert_eq!(
        found.iter().take(2).collect::<Vec<_>>(),
        [
            "the user prefers Rust over Python",
            "gardening in spring requires patience"
        ],
        "the keyword match places in both arms and leads; the exact vector \
         match places first in one and follows: {found:?}"
    );
    assert!(
        hits[0].rrf > hits[1].rrf,
        "matching two arms must beat matching one: {} vs {}",
        hits[0].rrf,
        hits[1].rrf
    );
    assert_eq!(found.len(), 5, "the pool holds every embedded memory");
}

#[tokio::test]
async fn episode_chunks_compete_only_when_asked_for() {
    let db = seeded().await;
    let mut search = Search::new(vec![space()]);
    // No memory mentions the borrow checker; one chunk does.
    search.text = Some("borrow checker".to_owned());

    let with = repo::search_hybrid(&db, &search).await.expect("search");
    search.episodes = false;
    let without = repo::search_hybrid(&db, &search).await.expect("search");

    assert_eq!(
        contents(&with),
        ["the borrow checker took an hour to explain"],
        "verbatim ground truth answers what no distilled memory covers"
    );
    assert!(
        without.is_empty(),
        "and stays out of a memories-only recall: {:?}",
        contents(&without)
    );
    assert!(
        matches!(&with[0].hit, Hit::Chunk(chunk) if chunk.position == 0),
        "a chunk hit carries its place in the episode"
    );
}

#[tokio::test]
async fn a_closed_memory_is_invisible_until_the_window_asks_for_it() {
    let db = seeded().await;
    let old = live_id(&db, "the user prefers Rust").await;
    let mut correction = NewMemory::new(Kind::Fact, "the user now prefers Rust over Go");
    correction.supersedes = Some(old.clone());
    correction.valid_from = Some(stamp(CORRECTED_AT));
    repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: None,
            memories: vec![correction],
        },
    )
    .await
    .expect("correction");

    let mut search = Search::new(vec![space()]);
    // "Python" survives only on the closed row.
    search.text = Some("Python".to_owned());
    search.episodes = false;

    let live = repo::search_hybrid(&db, &search).await.expect("live");
    search.liveness = Liveness::Any;
    let any = repo::search_hybrid(&db, &search).await.expect("any");
    search.liveness = Liveness::AsOf(stamp("2026-03-01T00:00:00Z"));
    let before = repo::search_hybrid(&db, &search).await.expect("as of");
    search.liveness = Liveness::AsOf(stamp("2026-08-01T00:00:00Z"));
    let after = repo::search_hybrid(&db, &search).await.expect("as of");

    assert!(
        live.is_empty(),
        "the claim is no longer true: {:?}",
        contents(&live)
    );
    assert_eq!(contents(&any), ["the user prefers Rust over Python"]);
    assert_eq!(
        contents(&before),
        ["the user prefers Rust over Python"],
        "it was true in March"
    );
    assert!(
        after.is_empty(),
        "and not in August: {:?}",
        contents(&after)
    );
}

#[tokio::test]
async fn recall_reinforces_what_it_returned() {
    let db = seeded().await;
    let id = live_id(&db, "the user prefers Rust").await;
    let before = repo::direct_lookup(&db, &Lookup::new(vec![space()]))
        .await
        .expect("lookup");
    let untouched = before.iter().find(|m| m.id == id).expect("seeded memory");
    assert_eq!((untouched.strength, untouched.access_count), (1.0, 0));

    let missing = MemoryId::new("01M145SMNET1XRYA713EWAQTD3").expect("valid ulid");
    let touched = repo::reinforce(&db, &[id.clone(), missing])
        .await
        .expect("reinforce");

    assert_eq!(touched, 1, "an id naming nothing is skipped, not refused");
    let after = repo::direct_lookup(&db, &Lookup::new(vec![space()]))
        .await
        .expect("lookup");
    let reinforced = after.iter().find(|m| m.id == id).expect("seeded memory");
    assert_eq!((reinforced.strength, reinforced.access_count), (2.0, 1));
    assert!(
        reinforced.last_accessed > untouched.last_accessed,
        "the decay clock restarts on use"
    );
    assert_eq!(
        after[0].id, id,
        "and a direct lookup returns the strongest first"
    );
}

#[tokio::test]
async fn direct_lookup_filters_on_the_indexed_columns() {
    let db = seeded().await;
    let contents_of = async |filters: Filters| {
        let mut lookup = Lookup::new(vec![space()]);
        lookup.filters = filters;
        repo::direct_lookup(&db, &lookup)
            .await
            .expect("lookup")
            .into_iter()
            .map(|memory| memory.content)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        contents_of(Filters {
            kinds: vec![Kind::Instruction],
            ..Filters::default()
        })
        .await,
        ["answer in English"]
    );
    assert_eq!(
        contents_of(Filters {
            tags: vec!["identity".to_owned()],
            ..Filters::default()
        })
        .await,
        ["the user prefers Rust over Python"]
    );

    let mut any_of = contents_of(Filters {
        entities: vec!["garden".to_owned(), "home".to_owned()],
        ..Filters::default()
    })
    .await;
    any_of.sort();
    assert_eq!(
        any_of,
        [
            "gardening in spring requires patience",
            "the kitchen tap drips at night"
        ],
        "entities within one filter are alternatives"
    );
    assert!(
        contents_of(Filters {
            kinds: vec![Kind::Instruction],
            tags: vec!["identity".to_owned()],
            ..Filters::default()
        })
        .await
        .is_empty(),
        "but separate filters compound"
    );
    assert_eq!(
        contents_of(Filters::default()).await.len(),
        5,
        "and no filter is not a filter"
    );
}

#[tokio::test]
async fn a_history_walk_returns_the_whole_chain_from_any_link() {
    let db = seeded().await;
    let first = live_id(&db, "the user prefers Rust").await;
    let mut second = NewMemory::new(Kind::Fact, "the user prefers Rust over Go");
    second.supersedes = Some(first.clone());
    second.valid_from = Some(stamp(CORRECTED_AT));
    let second = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: None,
            memories: vec![second],
        },
    )
    .await
    .expect("first correction")
    .memories
    .remove(0)
    .into_id();
    let mut third = NewMemory::new(Kind::Fact, "the user prefers Rust over everything");
    third.supersedes = Some(second.clone());
    let third = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: None,
            memories: vec![third],
        },
    )
    .await
    .expect("second correction")
    .memories
    .remove(0)
    .into_id();

    let expected = [first.clone(), second.clone(), third.clone()];
    for link in &expected {
        let chain = repo::history_chain(&db, &space(), link)
            .await
            .expect("chain");
        let ids: Vec<MemoryId> = chain.iter().map(|memory| memory.id.clone()).collect();
        assert_eq!(ids, expected, "walked from {link}");
    }

    let lone = live_id(&db, "answer in English").await;
    assert_eq!(
        repo::history_chain(&db, &space(), &lone)
            .await
            .expect("chain")
            .len(),
        1,
        "a memory nobody corrected is a chain of one"
    );
    let elsewhere = "other".parse().expect("valid slug");
    assert!(
        matches!(
            repo::history_chain(&db, &elsewhere, &first).await,
            Err(StoreError::UnknownMemory { .. })
        ),
        "a chain is only walkable from the space that holds it"
    );
}

#[tokio::test]
async fn the_near_dup_gate_measures_the_nearest_live_neighbour() {
    let db = seeded().await;
    let profile = live_id(&db, "the user prefers Rust").await;

    let probes = repo::nearest_live(&db, &space(), &[axis(5), axis(2)])
        .await
        .expect("probe");

    assert_eq!(probes.len(), 2, "one answer per probe, in input order");
    let exact = probes[0].first().expect("the space holds vectors");
    assert_eq!(exact.id, profile);
    assert_eq!(
        exact.content, "the user prefers Rust over Python",
        "the gate reports what the neighbour says, not only that it exists"
    );
    assert!(
        (exact.similarity - 1.0).abs() < 1e-6,
        "a vector identical to a stored one is a similarity of 1: {}",
        exact.similarity
    );
    assert!(dedup::is_near_duplicate(exact.similarity));
    assert!(
        probes[0]
            .windows(2)
            .all(|pair| pair[0].similarity >= pair[1].similarity),
        "neighbours come back closest first"
    );
    let orthogonal = probes[1].first().expect("the space holds vectors");
    assert!(
        !dedup::is_near_duplicate(orthogonal.similarity),
        "every fixture axis is orthogonal to every other: {}",
        orthogonal.similarity
    );
    assert!(
        !dedup::is_correction_candidate(orthogonal.similarity),
        "an orthogonal axis is not a correction candidate either: {}",
        orthogonal.similarity
    );

    let elsewhere = "other".parse().expect("valid slug");
    assert!(
        repo::nearest_live(&db, &elsewhere, &[axis(5)])
            .await
            .expect("probe")[0]
            .is_empty(),
        "a space holding no vectors has no neighbour to offer"
    );
    assert!(
        repo::nearest_live(&db, &space(), &[])
            .await
            .expect("probe")
            .is_empty(),
        "and nothing to probe costs no round-trip"
    );

    // A correction closes the row it replaces, and the gate stops seeing it —
    // otherwise re-stating a claim that was already corrected would come back
    // as a duplicate of the version that is no longer true.
    let mut correction = NewMemory::new(Kind::Fact, "the user prefers Rust over Go");
    correction.supersedes = Some(profile.clone());
    correction.embedding = Some(axis(3));
    repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: None,
            memories: vec![correction],
        },
    )
    .await
    .expect("correction");

    let after = repo::nearest_live(&db, &space(), &[axis(5)])
        .await
        .expect("probe");
    assert!(
        after[0].iter().all(|neighbour| neighbour.id != profile),
        "the gate compares against what is still true, not what once was"
    );
}

#[tokio::test]
async fn stats_count_what_a_space_holds() {
    let db = seeded().await;
    let stats = repo::stats(&db, &space()).await.expect("stats");

    assert_eq!(
        (stats.memories, stats.live, stats.episodes, stats.chunks),
        (5, 5, 1, 3)
    );
    assert_eq!(
        stats.live_by_kind,
        [(Kind::Fact, 3), (Kind::Instruction, 1), (Kind::Lesson, 1)],
        "one bucket per kind present, alphabetically"
    );

    let empty = repo::stats(&db, &"other".parse().expect("valid slug"))
        .await
        .expect("stats");
    assert_eq!((empty.memories, empty.episodes), (0, 0));
    assert!(empty.live_by_kind.is_empty());
}

#[tokio::test]
async fn an_episode_comes_back_with_its_slices_and_what_it_produced() {
    let db = seeded().await;
    let seeded_memories = repo::direct_lookup(&db, &Lookup::new(vec![space()]))
        .await
        .expect("lookup");
    let Source::Episode { episode: id } = &seeded_memories[0].source else {
        panic!("the fixture writes every memory alongside its episode");
    };

    let detail = repo::episode(&db, &space(), id).await.expect("episode");
    assert_eq!(
        detail.episode.content, "a long conversation",
        "the verbatim text, unedited — this is what makes a claim quotable"
    );
    assert_eq!(
        detail
            .chunks
            .iter()
            .map(|chunk| (chunk.position, chunk.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (0, "the borrow checker took an hour to explain"),
            (1, "then somebody made coffee"),
            (2, "and the meeting ended"),
        ],
        "slices in reading order, not the order the engine happened to store them"
    );
    assert_eq!(
        detail.derived.len(),
        5,
        "and every claim distilled from it, however many"
    );
    assert!(
        detail
            .derived
            .iter()
            .all(|memory| matches!(&memory.source, Source::Episode { episode } if episode == id)),
        "the link is `source.ref`, walked backwards"
    );

    let absent = EpisodeId::new("01M145SMNET1XRYA713EWAQTD3").expect("ulid");
    assert!(matches!(
        repo::episode(&db, &space(), &absent).await,
        Err(StoreError::UnknownEpisode { .. })
    ));
    let elsewhere: SpaceName = "other".parse().expect("valid slug");
    assert!(
        matches!(
            repo::episode(&db, &elsewhere, id).await,
            Err(StoreError::UnknownEpisode { .. })
        ),
        "an id is a capability inside a space, not across them"
    );
}
