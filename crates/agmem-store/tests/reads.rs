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

use agmem_core::{DecayClass, EpisodeId, Kind, MemoryId, Source, SpaceName, dedup};
use agmem_store::db::Db;
use agmem_store::repo::{
    self, Batch, Candidate, Filters, Forget, Hit, Liveness, Lookup, NewChunk, NewEpisode,
    NewMemory, Search, Written,
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
async fn a_count_answers_for_the_whole_selection_a_limit_only_pages() {
    let db = seeded().await;
    let count_of = async |filters: Filters, liveness: Liveness| {
        let mut lookup = Lookup::new(vec![space()]);
        lookup.filters = filters;
        lookup.liveness = liveness;
        // Deliberately smaller than the answer: the limit is what makes a page
        // look like a store, and the count exists to be immune to it.
        lookup.limit = 1;
        repo::count_matching(&db, &lookup).await.expect("count")
    };

    assert_eq!(
        count_of(Filters::default(), Liveness::Live).await,
        5,
        "the limit pages the rows and never the count"
    );
    assert_eq!(
        count_of(
            Filters {
                kinds: vec![Kind::Instruction],
                ..Filters::default()
            },
            Liveness::Live
        )
        .await,
        1,
        "and it narrows exactly like the lookup it sits beside"
    );
    assert_eq!(
        count_of(
            Filters {
                tags: vec!["nobody-uses-this".to_owned()],
                ..Filters::default()
            },
            Liveness::Live
        )
        .await,
        0,
        "a selection matching nothing groups into no rows, which is still zero"
    );

    let old = live_id(&db, "the user prefers Rust").await;
    let mut correction = NewMemory::new(Kind::Fact, "the user now prefers Rust over Go");
    correction.supersedes = Some(old);
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

    assert_eq!(
        count_of(Filters::default(), Liveness::Live).await,
        5,
        "a correction closes one row and opens another"
    );
    assert_eq!(
        count_of(Filters::default(), Liveness::Any).await,
        6,
        "and the closed one is still there to be counted"
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

/// Backdate a row and set the counters the sweep and `stale_contexts` read.
///
/// `last_accessed` and `access_count` are the engine's to default, so nothing
/// in the write API can produce a row that has been idle for a year — which is
/// the only interesting state either query has.
async fn age(db: &Db, id: &MemoryId, days: i64, strength: f64, accesses: i64) {
    db.query(
        "UPDATE type::record('memory', $id)
         SET last_accessed = time::now() - duration::from_secs($idle),
             strength = $strength, access_count = $accesses",
    )
    .bind(("id", id.to_string()))
    .bind(("idle", days * 86_400))
    .bind(("strength", strength))
    .bind(("accesses", accesses))
    .await
    .expect("age the row")
    .check()
    .expect("statements");
}

#[tokio::test]
async fn the_all_pairs_scan_pairs_every_live_row_with_its_own_vector() {
    let db = seeded().await;
    // A BM25-only write leaves no vector behind, and there is nothing to
    // compare such a row by — it is absent rather than present and empty.
    repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: None,
            memories: vec![NewMemory::new(
                Kind::Fact,
                "the parking barrier needs a fob",
            )],
        },
    )
    .await
    .expect("write");

    let rows = repo::live_vectors(&db, &space(), repo::MAX_POOL)
        .await
        .expect("scan");
    assert_eq!(
        rows.len(),
        5,
        "the five seeded memories carry a vector; the sixth does not"
    );

    let profile = rows
        .iter()
        .find(|row| row.memory.content.starts_with("the user prefers Rust"))
        .expect("the seeded profile");
    assert_eq!(
        profile.embedding,
        axis(5),
        "the vector comes back as it was written"
    );
    assert!(
        rows.iter().all(|row| row.memory.embedding.is_none()),
        "the vector rides beside the record, never inside it"
    );

    let closed = profile.memory.id.clone();
    repo::forget(
        &db,
        &Forget {
            spaces: vec![space()],
            memories: vec![closed.clone()],
            episodes: Vec::new(),
            purge: false,
        },
    )
    .await
    .expect("forget");

    let after = repo::live_vectors(&db, &space(), repo::MAX_POOL)
        .await
        .expect("rescan");
    assert_eq!(after.len(), 4);
    assert!(
        after.iter().all(|row| row.memory.id != closed),
        "consolidation offers merges among what is still true"
    );

    let elsewhere = "other".parse().expect("valid slug");
    assert!(
        repo::live_vectors(&db, &elsewhere, repo::MAX_POOL)
            .await
            .expect("scan")
            .is_empty(),
        "an empty space is an empty answer, not an error"
    );
}

#[tokio::test]
async fn the_all_pairs_similarity_is_the_one_the_engine_reports() {
    // The whole point of the arm: `consolidate` compares vectors in this
    // process against thresholds the write gate states in the engine's units.
    // If the two spellings of cosine ever disagreed, 0.90 would mean two
    // different things in one tool.
    let db = seeded().await;
    let rows = repo::live_vectors(&db, &space(), repo::MAX_POOL)
        .await
        .expect("scan");

    // Halfway between two fixture axes, so the probe lands mid-band rather
    // than at the 1.0 and 0.0 the one-hot fixture otherwise produces.
    let mut probe = vec![0.0; agmem_store::migrate::EMBEDDING_DIM];
    // 0.894 against axis 5: inside [0.75, 0.90), which an equal mix of two
    // axes is not — that is 0.707, below the floor entirely.
    probe[5] = 1.0;
    probe[0] = 0.5;
    let unit = dedup::Unit::new(&probe).expect("a direction");

    let probed = repo::nearest_live(&db, &space(), &[probe])
        .await
        .expect("probe");
    let neighbours = &probed[0];
    assert!(
        neighbours
            .iter()
            .any(|neighbour| dedup::is_contradiction_candidate(neighbour.similarity)),
        "the probe is meant to land in the band the contradiction arm reads"
    );

    for neighbour in neighbours {
        let row = rows
            .iter()
            .find(|row| row.memory.id == neighbour.id)
            .expect("the scan sees what the probe sees");
        let ours = unit.similarity(&dedup::Unit::new(&row.embedding).expect("a direction"));
        assert!(
            (ours - neighbour.similarity).abs() < 1e-5,
            "{}: engine {} vs all-pairs {ours}",
            row.memory.content,
            neighbour.similarity
        );
    }
}

#[tokio::test]
async fn stale_contexts_are_the_rows_reinforcement_carried_past_the_prune() {
    let db = seeded().await;
    let working = |content: &str| {
        let mut memory = NewMemory::new(Kind::Fact, content);
        memory.decay_class = Some(DecayClass::Fast);
        memory
    };
    let ids: Vec<MemoryId> = repo::insert_batch(
        &db,
        Batch {
            space: space(),
            episode: None,
            memories: vec![
                working("deploys go out from the release branch"),
                working("the branch under review is called spike"),
                working("the failing test is called roundtrip"),
                NewMemory::new(Kind::Fact, "the office is on the third floor"),
            ],
        },
    )
    .await
    .expect("write")
    .memories
    .into_iter()
    .map(Written::into_id)
    .collect();
    let (carried, untouched, current, durable) = (
        ids[0].clone(),
        ids[1].clone(),
        ids[2].clone(),
        ids[3].clone(),
    );

    // Recalled thirty times, so `strength` bought it roughly 620 days against
    // a class whose unreinforced horizon is twenty — the sweep will not reach
    // it, which is exactly why it needs a decision instead.
    age(&db, &carried, 200, 31.0, 30).await;
    // Equally idle, but nothing ever used it — the prune's own backlog, and
    // it will close on the next start rather than needing a decision.
    age(&db, &untouched, 200, 1.0, 1).await;
    // Heavily used and still in use; nothing to reconsider yet.
    age(&db, &current, 1, 6.0, 20).await;
    // Ancient and heavily used, but never filed as short-lived: a `normal`
    // fact has no TTL to outlive (design §5.5).
    age(&db, &durable, 400, 8.0, 30).await;

    let found: Vec<MemoryId> = repo::stale_contexts(&db, &space(), repo::StaleContexts::new())
        .await
        .expect("scan")
        .into_iter()
        .map(|memory| memory.id)
        .collect();
    assert_eq!(found, vec![carried.clone()]);

    // The sweep agrees it cannot reach it: that is what makes it a candidate
    // rather than something already handled.
    assert!(
        !repo::prune_expired(&db)
            .await
            .expect("prune")
            .contains(&carried),
        "the row consolidation reports is exactly the one the prune leaves"
    );

    let elsewhere = "other".parse().expect("valid slug");
    assert!(
        repo::stale_contexts(&db, &elsewhere, repo::StaleContexts::new())
            .await
            .expect("scan")
            .is_empty()
    );
}
