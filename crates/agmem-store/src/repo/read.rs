//! The read half (design §5.3).
//!
//! A hybrid recall is one request: BM25 and HNSW over `memory` and
//! `episode_chunk`, fused by `search::rrf` into a single candidate order.
//! Ranking stops there. What comes back is the retrieval signal and the rows;
//! turning that into a final order is `core::scoring`'s job, because decay is
//! computed at read time from the clock, not stored.
//!
//! Retrieved records never carry their vector — reads do not project
//! `embedding` — so a [`MemoryRecord`] or [`EpisodeChunk`] from this module
//! always has `None` there, whatever the store holds. [`live_vectors`] is the
//! one read that needs them, and it does not break that: the vector rides
//! beside the record in an [`Embedded`], never inside it.

use std::collections::{HashMap, HashSet};

use agmem_core::{
    ChunkId, Derivation, DocKind, Episode, EpisodeChunk, EpisodeId, Kind, MemoryId, MemoryRecord,
    SpaceName, dedup, scoring,
};
use jiff::Timestamp;
use surrealdb::engine::any::Any;
use surrealdb::method::Query;
use surrealdb::types::RecordId;

use super::write::PRUNE_CLASS;
use super::{checked, ensure_memories_exist};
use crate::StoreError;
use crate::db::Db;
use crate::queries::read as queries;
use crate::types::{
    self, ChainRow, ChunkReadRow, ChurnRow, DocumentHeaderRow, DocumentsRow, EpisodeDetailRow,
    EpisodeReadRow, LiveVectorsRow, LocatedRow, MemoryReadRow, NeighbourRow, SearchRow, StatsRow,
};

/// The candidate pool one recall considers, before `k` truncates it
/// (design §5.3, `AGMEM_POOL`).
pub const DEFAULT_POOL: usize = 64;

/// The largest pool or lookup limit the store will build a query for; larger
/// values are clamped to it.
pub const MAX_POOL: usize = queries::MAX_POOL;

/// RRF's rank-smoothing constant: the `60` in every arm's `1 / (60 + rank)`.
/// Exported so a caller composing an extra arm against the fused pool — the
/// server's hop — shares the engine's arithmetic instead of re-deriving it.
pub const RRF_K: usize = queries::RRF_K;

/// The indexed narrowing a read applies before anything is scored.
///
/// Within a field the values are alternatives (`entities CONTAINSANY`), across
/// fields they compound: `kinds` and `entities` and `tags` must all match. An
/// empty field is not a filter at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    /// Keep only these kinds.
    pub kinds: Vec<Kind>,
    /// Keep only memories naming one of these subjects.
    pub entities: Vec<String>,
    /// Keep only memories carrying one of these labels.
    pub tags: Vec<String>,
}

/// Which memories a read may return.
///
/// Supersession closes a record rather than deleting it, so "what is true"
/// and "what was true" are the same query with a different window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Liveness {
    /// Only what is live now: `invalid_at IS NONE`.
    #[default]
    Live,
    /// What was live at an instant: `valid_from ≤ t < invalid_at`.
    AsOf(Timestamp),
    /// Everything, closed rows included.
    Any,
}

/// One hybrid recall.
#[derive(Debug, Clone)]
pub struct Search {
    /// The scopes to search; a memory belongs to exactly one.
    pub spaces: Vec<SpaceName>,
    /// The query text for the BM25 arms; `None` drops them.
    pub text: Option<String>,
    /// The query vector for the HNSW arms; `None` drops them.
    pub vector: Option<Vec<f32>>,
    /// Indexed narrowing, applied to memories only.
    pub filters: Filters,
    /// The validity window.
    pub liveness: Liveness,
    /// How many candidates each arm contributes, and the fused result keeps.
    pub pool: usize,
    /// Whether verbatim episode chunks compete alongside distilled memories.
    pub episodes: bool,
}

impl Search {
    /// A recall over `spaces` with only what it cannot do without; set the
    /// rest directly. Both arms start empty, so a search with neither `text`
    /// nor `vector` set matches nothing.
    pub fn new(spaces: Vec<SpaceName>) -> Self {
        Self {
            spaces,
            text: None,
            vector: None,
            filters: Filters::default(),
            liveness: Liveness::Live,
            pool: DEFAULT_POOL,
            episodes: true,
        }
    }
}

/// One tier-1 lookup: indexed filters, no query and no embedding.
#[derive(Debug, Clone)]
pub struct Lookup {
    /// The scopes to look in.
    pub spaces: Vec<SpaceName>,
    /// Indexed narrowing; an empty [`Filters`] means "everything in `spaces`".
    pub filters: Filters,
    /// The validity window.
    pub liveness: Liveness,
    /// How many rows to return, strongest first.
    pub limit: usize,
}

impl Lookup {
    /// A lookup over `spaces` with only what it cannot do without; set the
    /// rest directly.
    pub fn new(spaces: Vec<SpaceName>) -> Self {
        Self {
            spaces,
            filters: Filters::default(),
            liveness: Liveness::Live,
            limit: DEFAULT_POOL,
        }
    }
}

/// What retrieval matched: a distilled memory, or the verbatim text one was
/// distilled from.
#[derive(Debug, Clone, PartialEq)]
pub enum Hit {
    /// A memory. Boxed because it dwarfs the other variant.
    Memory(Box<MemoryRecord>),
    /// A slice of an episode.
    Chunk(EpisodeChunk),
}

/// One retrieval candidate, with the score that surfaced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// SurrealDB's `search::rrf` score: the sum of `1 / (60 + rank)` over
    /// every arm this row placed in. It has no fixed scale — `core::scoring`
    /// normalises it against the pool before weighing it.
    pub rrf: f64,
    /// Cosine similarity between the query and this row, when a vector arm
    /// measured it — the one absolute relevance signal a recall carries
    /// (issue #77), in the same unit as [`Neighbour::similarity`]. `None`
    /// when no vector arm ran, or when only a text arm returned the row:
    /// the absence of a measurement is not evidence of irrelevance.
    pub similarity: Option<f64>,
    /// The row itself.
    pub hit: Hit,
}

/// The nearest live memory to a candidate, and how close it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    /// The memory already in the store.
    pub id: MemoryId,
    /// What that memory says — the gate's caller hands this to an agent
    /// deciding whether the new claim corrects it, and an id is not something
    /// to decide on.
    pub content: String,
    /// Cosine similarity: 1.0 for an identical vector, 0.0 for an orthogonal
    /// one. Converted from the engine's distance by [`dedup`], so the gate and
    /// its threshold speak the same units.
    pub similarity: f64,
}

/// An episode with everything that came out of it.
#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeDetail {
    /// The verbatim text, as stored.
    pub episode: Episode,
    /// Its retrieval slices, in reading order.
    pub chunks: Vec<EpisodeChunk>,
    /// The claims distilled from it, oldest first.
    pub derived: Vec<MemoryRecord>,
}

/// Which documents to list (#134, `inspect` on `docs:<space>`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentFilter {
    /// Keep only these kinds; empty keeps every kind.
    pub kinds: Vec<DocKind>,
    /// Keep only documents carrying one of these tags; empty keeps all.
    pub tags: Vec<String>,
    /// How many to return, newest first.
    pub limit: usize,
}

/// One document on a listing page: the row, and how many live memories cite
/// it through `source.ref` or `derived_from`.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentSummary {
    /// The document itself, content included.
    pub episode: Episode,
    /// Live memories citing it, each counted once.
    pub cited: u64,
}

/// Per-space counts, for `inspect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceStats {
    /// The space these counts describe.
    pub space: SpaceName,
    /// Every memory, live or closed.
    pub memories: u64,
    /// Memories that are still live.
    pub live: u64,
    /// Live memories per kind, alphabetically; kinds with none are absent.
    pub live_by_kind: Vec<(Kind, u64)>,
    /// Verbatim episodes.
    pub episodes: u64,
    /// Retrieval slices of those episodes.
    pub chunks: u64,
    /// Those episodes that are documents: named and typed (#132).
    pub documents: u64,
}

/// Hybrid search over memories and episode chunks, best candidate first.
///
/// One round-trip: every arm, the fusion, and the projection of the survivors
/// travel as a single request. Candidates come back in `search::rrf` order
/// with their fused score; they are *not* decayed or reranked here.
///
/// A search with neither `text` nor `vector` has nothing to retrieve on and
/// returns empty without touching the engine — use [`direct_lookup`] for a
/// filters-only read.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn search_hybrid(db: &Db, search: &Search) -> Result<Vec<Candidate>, StoreError> {
    // Terms rather than the raw text: `@N@` ANDs whatever one reference holds,
    // so the fulltext arms are built from one reference per word (issue #39).
    // Text that yields no terms — punctuation, an empty string — leaves the
    // request with whatever vector arm it has, or nothing at all.
    let terms = queries::terms(search.text.as_deref().unwrap_or_default());
    if terms.is_empty() && search.vector.is_none() {
        return Ok(Vec::new());
    }
    let script = queries::search(search, &terms);
    let mut query = db
        .query(&script.text)
        .bind(("spaces", space_strs(&search.spaces)));
    for (index, term) in terms.iter().enumerate() {
        query = query.bind((format!("t{index}"), term.clone()));
    }
    if let Some(vector) = &search.vector {
        query = query.bind(("vector", vector.clone()));
    }
    query = bind_filters(query, &search.filters, search.liveness);

    let mut resp = checked(query.await?)?;
    let row: SearchRow = resp.take::<Option<SearchRow>>(script.result_index)?.ok_or(
        StoreError::UnexpectedResponse("the search reported nothing"),
    )?;

    let mut memories: HashMap<String, MemoryReadRow> = row
        .memories
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    let mut chunks: HashMap<String, ChunkReadRow> = row
        .chunks
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();

    let similarities: HashMap<(String, String), f64> = row
        .nearest
        .into_iter()
        .map(|near| {
            (
                (near.table, near.id),
                dedup::similarity_from_distance(near.d),
            )
        })
        .collect();

    let mut candidates = Vec::with_capacity(row.scored.len());
    for scored in row.scored {
        // The fused ids and the rows come from the same request, so every
        // scored id has a row; a miss means the query text and the row
        // structs have drifted apart.
        let missing = StoreError::UnexpectedResponse("the search scored a row it did not return");
        let similarity = similarities
            .get(&(scored.table.clone(), scored.id.clone()))
            .copied();
        let hit = match scored.table.as_str() {
            types::MEMORY => Hit::Memory(Box::new(
                memories.remove(&scored.id).ok_or(missing)?.into_record()?,
            )),
            types::EPISODE_CHUNK => {
                Hit::Chunk(chunks.remove(&scored.id).ok_or(missing)?.into_chunk()?)
            }
            _ => return Err(missing),
        };
        candidates.push(Candidate {
            rrf: scored.rrf,
            similarity,
            hit,
        });
    }
    Ok(candidates)
}

/// Tier-1 retrieval: indexed filters only, strongest first.
///
/// This is the path `recall` takes when the request is entity- or tag-shaped
/// rather than a question, and what `context` assembles its fixed sections
/// from — no query text, no embedding, no fusion.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn direct_lookup(db: &Db, lookup: &Lookup) -> Result<Vec<MemoryRecord>, StoreError> {
    let text = queries::direct_lookup(lookup);
    let query = db.query(&text).bind(("spaces", space_strs(&lookup.spaces)));
    let mut resp = checked(bind_filters(query, &lookup.filters, lookup.liveness).await?)?;
    let rows: Vec<MemoryReadRow> = resp.take(0)?;
    rows.into_iter().map(MemoryReadRow::into_record).collect()
}

/// How many live claims the filters select, ignoring `lookup.limit`.
///
/// A page carries no evidence of what it left behind: fifty hits out of fifty
/// rows and fifty out of five hundred look identical to the agent reading
/// them. This is the second number. It counts memories and not episode
/// chunks, because the filters narrow memories and the question it answers is
/// how many claims the ranking chose between.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects.
pub async fn count_matching(db: &Db, lookup: &Lookup) -> Result<u64, StoreError> {
    let text = queries::count_matching(lookup);
    let query = db.query(&text).bind(("spaces", space_strs(&lookup.spaces)));
    let mut resp = checked(bind_filters(query, &lookup.filters, lookup.liveness).await?)?;
    let total: Option<i64> = resp.take(0)?;
    Ok(count(total.unwrap_or_default()))
}

/// The nearest live memories in `space` to each of `vectors`, closest first,
/// in input order.
///
/// The near-dup gate of the write path (design §5.2 step 4): a memory whose
/// nearest live neighbour is close enough is the same claim in different
/// words, and `remember` reports it instead of storing it again. The
/// neighbours behind it are the correction band (issue #38) — same subject,
/// different statement — reported so the agent has the id of what it may be
/// contradicting. Deciding *how* close either is belongs to
/// [`dedup::is_near_duplicate`] and [`dedup::is_correction_candidate`]; this
/// only measures.
///
/// An empty slot means the space holds no vector to compare against yet.
/// Every vector must be the width the schema's HNSW indexes were defined at —
/// the engine rejects any other at query time — so callers running without an
/// embedder pass nothing here rather than passing empty vectors.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for an id that is not a ULID.
pub async fn nearest_live(
    db: &Db,
    space: &SpaceName,
    vectors: &[Vec<f32>],
) -> Result<Vec<Vec<Neighbour>>, StoreError> {
    if vectors.is_empty() {
        return Ok(Vec::new());
    }
    let script = queries::nearest_live(vectors.len());
    let mut query = db
        .query(&script.text)
        .bind(("space", types::space_str(space)));
    for (index, vector) in vectors.iter().enumerate() {
        query = query.bind((format!("vec{index}"), vector.clone()));
    }

    let mut resp = checked(query.await?)?;
    let probes: Vec<Vec<NeighbourRow>> = resp.take(script.result_index)?;
    if probes.len() != vectors.len() {
        return Err(StoreError::UnexpectedResponse(
            "the gate probed a different number of vectors than it was given",
        ));
    }
    probes
        .into_iter()
        .map(|rows| {
            rows.into_iter()
                .map(|row| {
                    Ok(Neighbour {
                        id: MemoryId::new(row.id)?,
                        content: row.content,
                        similarity: dedup::similarity_from_distance(row.distance),
                    })
                })
                .collect()
        })
        .collect()
}

/// A live memory with the vector the store holds for it.
///
/// The pair exists because consolidation is the one read that needs both, and
/// because [`MemoryRecord::embedding`] staying `None` on every read is an
/// invariant worth keeping — a record that sometimes carries a vector is one
/// every caller has to check.
#[derive(Debug, Clone, PartialEq)]
pub struct Embedded {
    /// The memory itself, with `embedding: None` like every other read.
    pub memory: MemoryRecord,
    /// Its stored vector, exactly as written.
    pub embedding: Vec<f32>,
}

/// Which rows [`stale_contexts`] selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleContexts {
    /// How often a memory must have been recalled to count as one
    /// reinforcement kept alive rather than one merely waiting to expire.
    pub min_access_count: u32,
    /// How many to return, most overdue first.
    pub limit: usize,
}

impl StaleContexts {
    /// The defaults design §5.5 is asking for.
    ///
    /// Five recalls is the line between "this is in use" and "this has not
    /// come up yet": a `fast` note nobody has touched expires on its own at
    /// the next start, and offering it as a candidate would be reporting the
    /// prune's own backlog as a finding.
    #[must_use]
    pub fn new() -> Self {
        Self {
            min_access_count: 5,
            limit: DEFAULT_POOL,
        }
    }
}

impl Default for StaleContexts {
    fn default() -> Self {
        Self::new()
    }
}

/// Every live memory in `space` that has a vector, with it (design §5.5).
///
/// Consolidation's only source of similarity. It asks which stored claims are
/// near *each other*, which is a different question from the one HNSW answers
/// — a KNN probe needs a query vector, so asking it per row means one scan per
/// row, each with its own recall loss. One flat read and an all-pairs pass in
/// `core::dedup` is exact and bounded by [`MAX_POOL`].
///
/// Strongest first, then newest, so a space past the cap keeps what has been
/// reinforced most. Memories written without a vector — BM25-only mode — are
/// absent rather than empty: there is nothing to compare them by.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects,
/// [`StoreError::MalformedRow`] for a row this schema cannot have written, and
/// [`StoreError::UnexpectedResponse`] when the two projections disagree about
/// which rows were selected.
pub async fn live_vectors(
    db: &Db,
    space: &SpaceName,
    limit: usize,
) -> Result<Vec<Embedded>, StoreError> {
    let script = queries::live_vectors(limit);
    let mut resp = checked(
        db.query(&script.text)
            .bind(("space", types::space_str(space)))
            .await?,
    )?;
    let row: LiveVectorsRow = resp
        .take::<Option<LiveVectorsRow>>(script.result_index)?
        .ok_or(StoreError::UnexpectedResponse(
            "the vector scan reported nothing",
        ))?;

    let mut vectors: HashMap<String, Vec<f32>> = row
        .vectors
        .into_iter()
        .map(|vector| (vector.id, vector.embedding))
        .collect();
    row.memories
        .into_iter()
        .map(|memory| {
            let embedding = vectors
                .remove(&memory.id)
                .ok_or(StoreError::UnexpectedResponse(
                    "a selected memory came back without its vector",
                ))?;
            Ok(Embedded {
                memory: memory.into_record()?,
                embedding,
            })
        })
        .collect()
}

/// Live `fast` memories the startup prune can no longer reach (design §5.5).
///
/// The sweep scales each row's horizon by its own `strength`, so every recall
/// buys a working note more time — deliberately, and it is the same
/// reinforcement that flattens the decay curve at read time. Capped at
/// `scoring::MAX_STABILITY` (issue #52), that still leaves a reinforced note
/// alive up to five horizons past its class with nothing revisiting it. This
/// finds those rows so an agent can: re-`remember` the claim at a slower
/// class if it turned out to be durable, or `forget` it if it did not.
///
/// Selection is the prune's own, with the `strength` factor removed — see
/// `queries::read::stale_contexts`. Most overdue first.
///
/// An empty answer is the ordinary one: the class only ever holds what an
/// agent chose to file as short-lived.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row this schema cannot have written.
pub async fn stale_contexts(
    db: &Db,
    space: &SpaceName,
    params: StaleContexts,
) -> Result<Vec<MemoryRecord>, StoreError> {
    // A class that never decays has no horizon to be past, so nothing can be
    // overdue against it — an empty answer, not an error.
    let Some(horizon) = prune_horizon_secs() else {
        return Ok(Vec::new());
    };
    let text = queries::stale_contexts(params.limit);
    let mut resp = checked(
        db.query(&text)
            .bind(("space", types::space_str(space)))
            .bind(("class", types::decay_class_str(PRUNE_CLASS)))
            .bind(("horizon", horizon))
            .bind(("min_count", i64::from(params.min_access_count)))
            .await?,
    )?;
    let rows: Vec<MemoryReadRow> = resp.take(0)?;
    rows.into_iter().map(MemoryReadRow::into_record).collect()
}

/// How long an unreinforced memory of the prune's class survives idle: the
/// point at which unit strength reaches `scoring::PRUNE_RETENTION`.
///
/// Public because `consolidate` reports how far past it a row has been carried
/// and must not re-derive the number the sweep uses. `None` for a class that
/// never decays, which [`PRUNE_CLASS`] is not.
#[must_use]
pub fn prune_horizon_secs() -> Option<f64> {
    scoring::decay_horizon_secs(PRUNE_CLASS, scoring::PRUNE_RETENTION)
}

/// Reinforce every memory a recall returned, in one statement.
///
/// Raising `strength` flattens that memory's decay curve, which is the whole
/// mechanism by which use keeps a memory alive (design §2.3) — up to
/// `scoring::MAX_STABILITY`, past which a recall refreshes `last_accessed`
/// but buys no more (issue #52). Ids that name nothing are skipped rather
/// than refused — this runs after the hits are already on their way to the
/// agent, so it must not be able to fail a recall. Returns how many rows were
/// actually touched.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects.
pub async fn reinforce(db: &Db, ids: &[MemoryId]) -> Result<usize, StoreError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let refs: Vec<RecordId> = ids.iter().map(types::memory_ref).collect();
    let mut resp = checked(
        db.query(queries::REINFORCE)
            .bind(("ids", refs))
            .bind(("cap", scoring::MAX_STABILITY))
            .await?,
    )?;
    let touched: Vec<String> = resp.take(0)?;
    Ok(touched.len())
}

/// The whole supersession chain `id` belongs to, oldest claim first.
///
/// Walks `supersedes` backwards and `superseded_by` forwards, so it returns
/// the same list whichever link of the chain it is given, with `id` somewhere
/// in the middle. A memory that has never been corrected is a chain of one.
///
/// # Errors
/// [`StoreError::UnknownMemory`] when `id` is not in `space`,
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn history_chain(
    db: &Db,
    space: &SpaceName,
    id: &MemoryId,
) -> Result<Vec<MemoryRecord>, StoreError> {
    ensure_memories_exist(db, space, &[id]).await?;
    let script = queries::history_chain();
    let mut resp = checked(
        db.query(&script.text)
            .bind(("target", types::memory_ref(id)))
            .await?,
    )?;
    let row: ChainRow = resp
        .take::<Option<ChainRow>>(script.result_index)?
        .ok_or(StoreError::UnexpectedResponse("the walk reported nothing"))?;

    // `SELECT … FROM <list of ids>` gives no ordering promise, so the walk
    // returns the order it found and the rows are put back into it here.
    let mut rows: HashMap<String, MemoryReadRow> = row
        .rows
        .into_iter()
        .map(|row| (row.id.clone(), row))
        .collect();
    row.ids
        .into_iter()
        .map(|id| {
            rows.remove(&id)
                .ok_or(StoreError::UnexpectedResponse(
                    "the walk named a link it did not return",
                ))?
                .into_record()
        })
        .collect()
}

/// One episode, its retrieval slices, and the claims distilled from it.
///
/// The provenance half of `inspect` (design §3.1): given a distilled claim's
/// `source`, this is the verbatim text it came from, unedited and quotable,
/// alongside everything else that came out of the same text. Chunks are in
/// reading order and derived memories in the order they were written.
///
/// # Errors
/// [`StoreError::UnknownEpisode`] when `id` is not in `space`,
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn episode(
    db: &Db,
    space: &SpaceName,
    id: &EpisodeId,
) -> Result<EpisodeDetail, StoreError> {
    let script = queries::episode();
    let mut resp = checked(
        db.query(&script.text)
            .bind(("target", types::episode_ref(id)))
            .bind(("space", types::space_str(space)))
            .await?,
    )?;
    let row: EpisodeDetailRow = resp
        .take::<Option<EpisodeDetailRow>>(script.result_index)?
        .ok_or_else(|| StoreError::UnknownEpisode {
            space: space.clone(),
            id: id.clone(),
        })?;
    Ok(EpisodeDetail {
        episode: row.episode.into_episode()?,
        chunks: row
            .chunks
            .into_iter()
            .map(ChunkReadRow::into_chunk)
            .collect::<Result<_, StoreError>>()?,
        derived: row
            .derived
            .into_iter()
            .map(MemoryReadRow::into_record)
            .collect::<Result<_, StoreError>>()?,
    })
}

/// The episode a retrieval slice belongs to, if `space` holds that slice.
///
/// `recall` hands out chunk ids for verbatim hits, so this is what lets one be
/// followed back to the text it came from (issue #36) rather than dead-ending
/// in an id no other tool accepts.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects and
/// [`StoreError::MalformedRow`] for a link that is not a ULID. A slice that is
/// not in `space` is `Ok(None)` rather than an error: the caller asks each
/// searched space in turn.
pub async fn episode_of_chunk(
    db: &Db,
    space: &SpaceName,
    id: &ChunkId,
) -> Result<Option<EpisodeId>, StoreError> {
    let script = queries::chunk_episode();
    let mut resp = checked(
        db.query(&script.text)
            .bind(("target", types::chunk_ref(id)))
            .bind(("space", types::space_str(space)))
            .await?,
    )?;
    Ok(resp
        .take::<Option<String>>(script.result_index)?
        .map(EpisodeId::new)
        .transpose()?)
}

/// Every episode filed under `title` in `space`, newest first (#134).
///
/// The first is the current version; the rest are what it replaced. A title
/// nobody has used is an empty list, not an error — the caller decides what
/// a miss means.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn documents_by_title(
    db: &Db,
    space: &SpaceName,
    title: &str,
) -> Result<Vec<Episode>, StoreError> {
    let script = queries::documents_by_title();
    let mut resp = checked(
        db.query(&script.text)
            .bind(("space", types::space_str(space)))
            .bind(("title", title.to_owned()))
            .await?,
    )?;
    resp.take::<Vec<EpisodeReadRow>>(script.result_index)?
        .into_iter()
        .map(EpisodeReadRow::into_episode)
        .collect()
}

/// The documents in `space` that pass `filter`, newest first, each with how
/// many live memories cite it (#134).
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn documents(
    db: &Db,
    space: &SpaceName,
    filter: &DocumentFilter,
) -> Result<Vec<DocumentSummary>, StoreError> {
    let script = queries::documents(filter);
    let mut query = db
        .query(&script.text)
        .bind(("space", types::space_str(space)));
    if !filter.kinds.is_empty() {
        let kinds: Vec<String> = filter
            .kinds
            .iter()
            .copied()
            .map(types::doc_kind_str)
            .collect();
        query = query.bind(("kinds", kinds));
    }
    if !filter.tags.is_empty() {
        query = query.bind(("tags", filter.tags.clone()));
    }
    let mut resp = checked(query.await?)?;
    let row: DocumentsRow = resp
        .take::<Option<DocumentsRow>>(script.result_index)?
        .ok_or(StoreError::UnexpectedResponse(
            "the documents listing reported nothing",
        ))?;

    // A memory citing one document through both columns is one citer.
    let mut citers: HashSet<(String, String)> = row
        .by_source
        .into_iter()
        .map(|cite| (cite.memory, cite.document))
        .collect();
    for cite in row.by_derivation {
        for document in cite.documents {
            citers.insert((cite.memory.clone(), document));
        }
    }
    let mut counts: HashMap<String, u64> = HashMap::new();
    for (_, document) in citers {
        *counts.entry(document).or_default() += 1;
    }

    row.docs
        .into_iter()
        .map(|doc| {
            let cited = counts.get(&doc.id).copied().unwrap_or(0);
            Ok(DocumentSummary {
                episode: doc.into_episode()?,
                cited,
            })
        })
        .collect()
}

/// The live memories that cite one document, through `source.ref` or
/// `derived_from`, oldest first (#134).
///
/// This is what a purge has to answer for: each of these would be left
/// naming text the store no longer holds.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn document_citers(
    db: &Db,
    space: &SpaceName,
    id: &EpisodeId,
) -> Result<Vec<MemoryRecord>, StoreError> {
    let script = queries::document_citers();
    let mut resp = checked(
        db.query(&script.text)
            .bind(("target", types::episode_ref(id)))
            .bind(("space", types::space_str(space)))
            .await?,
    )?;
    resp.take::<Vec<MemoryReadRow>>(script.result_index)?
        .into_iter()
        .map(MemoryReadRow::into_record)
        .collect()
}

/// The name and kind of a document, for a hit over one of its chunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentHeader {
    /// The document.
    pub id: EpisodeId,
    /// Its title.
    pub title: String,
    /// Its kind.
    pub doc_kind: DocKind,
}

/// Which of `ids` in `space` are documents, and what they are called (#134).
///
/// Anonymous episodes and ids the space does not hold are simply absent.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn document_headers(
    db: &Db,
    space: &SpaceName,
    ids: &[EpisodeId],
) -> Result<Vec<DocumentHeader>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let script = queries::document_headers();
    let refs: Vec<RecordId> = ids.iter().map(types::episode_ref).collect();
    let mut resp = checked(
        db.query(&script.text)
            .bind(("space", types::space_str(space)))
            .bind(("ids", refs))
            .await?,
    )?;
    resp.take::<Vec<DocumentHeaderRow>>(script.result_index)?
        .into_iter()
        .map(|row| {
            Ok(DocumentHeader {
                id: EpisodeId::new(row.id)?,
                // The query keeps only rows with `doc_kind`, and the write
                // path requires a title beside it; a row without one is
                // malformed rather than anonymous.
                title: row
                    .title
                    .ok_or(StoreError::UnexpectedResponse("a document without a title"))?,
                doc_kind: row
                    .doc_kind
                    .ok_or(StoreError::UnexpectedResponse("a document without a kind"))?
                    .parse()?,
            })
        })
        .collect()
}

/// The documents in `space` no live memory cites and that were stored more
/// than `grace_days` ago, newest first (#134, #137).
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn orphan_documents(
    db: &Db,
    space: &SpaceName,
    grace_days: u32,
) -> Result<Vec<Episode>, StoreError> {
    let script = queries::orphan_documents();
    let mut resp = checked(
        db.query(&script.text)
            .bind(("space", types::space_str(space)))
            .bind(("grace_days", i64::from(grace_days)))
            .await?,
    )?;
    resp.take::<Vec<EpisodeReadRow>>(script.result_index)?
        .into_iter()
        .map(EpisodeReadRow::into_episode)
        .collect()
}

/// A title rewritten more often than a plan should be (#137).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentChurn {
    /// The title every version shares.
    pub title: String,
    /// The current version's kind.
    pub doc_kind: DocKind,
    /// How many documents are filed under the title.
    pub versions: u32,
    /// The current version.
    pub newest: EpisodeId,
    /// When the first version was stored.
    pub first_at: Timestamp,
    /// When the current version was stored.
    pub latest_at: Timestamp,
}

/// The titles in `space` with more than `max_versions` documents under
/// them, most rewritten first (#137).
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a row the schema cannot have written.
pub async fn churning_documents(
    db: &Db,
    space: &SpaceName,
    max_versions: u32,
) -> Result<Vec<DocumentChurn>, StoreError> {
    let script = queries::churning_documents();
    let mut resp = checked(
        db.query(&script.text)
            .bind(("space", types::space_str(space)))
            .bind(("max_versions", i64::from(max_versions)))
            .await?,
    )?;
    let rows = resp.take::<Vec<ChurnRow>>(script.result_index)?;
    let mut churn = Vec::with_capacity(rows.len());
    for row in rows {
        let versions = documents_by_title(db, space, &row.title).await?;
        let Some(newest) = versions.into_iter().next() else {
            // Grouped a moment ago and gone now: a purge raced the read.
            continue;
        };
        churn.push(DocumentChurn {
            title: row.title,
            doc_kind: newest
                .doc_kind
                .ok_or(StoreError::UnexpectedResponse("a document without a kind"))?,
            versions: u32::try_from(row.versions)
                .map_err(|_| StoreError::UnexpectedResponse("a negative version count"))?,
            newest: newest.id,
            first_at: types::to_timestamp(&row.first_at),
            latest_at: types::to_timestamp(&row.latest_at),
        });
    }
    Ok(churn)
}

/// What each of `ids` names in `spaces`, in the order they were asked about.
///
/// `reflect` is handed citations an agent copied out of an earlier answer, and
/// those are bare ULIDs: `remember` returns one, a recall hit carries one, and
/// nothing about a ULID says which table it belongs to. This resolves the lot
/// in one round-trip, preferring a memory when a ULID somehow names a row in
/// both tables — the same order `inspect` tries them in.
///
/// `None` in a slot means no row in any of `spaces` answers to that id, which is the
/// caller's error to report rather than this one's: the ids came from an
/// agent, and naming which one missed is the whole of the useful message.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for an id the engine reported that is not a
/// ULID.
pub async fn locate(
    db: &Db,
    spaces: &[SpaceName],
    ids: &[String],
) -> Result<Vec<Option<Derivation>>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let memories: Vec<RecordId> = ids
        .iter()
        .map(|id| RecordId::new(types::MEMORY, id.as_str()))
        .collect();
    let episodes: Vec<RecordId> = ids
        .iter()
        .map(|id| RecordId::new(types::EPISODE, id.as_str()))
        .collect();
    let searched: Vec<String> = spaces.iter().map(types::space_str).collect();
    let mut resp = checked(
        db.query(queries::LOCATE)
            .bind(("spaces", searched))
            .bind(("mids", memories))
            .bind(("eids", episodes))
            .await?,
    )?;
    let found: LocatedRow =
        resp.take::<Option<LocatedRow>>(0)?
            .ok_or(StoreError::UnexpectedResponse(
                "the id lookup reported nothing",
            ))?;

    ids.iter()
        .map(|id| {
            if found.memories.iter().any(|hit| hit == id) {
                Ok(Some(Derivation::Memory(MemoryId::new(id.clone())?)))
            } else if found.episodes.iter().any(|hit| hit == id) {
                Ok(Some(Derivation::Episode(EpisodeId::new(id.clone())?)))
            } else {
                Ok(None)
            }
        })
        .collect()
}

/// Every space this store knows about, alphabetically.
///
/// `recall`'s `space: "all"` expands through this, and `inspect` walks it for
/// per-space counts. The registry is written by [`ensure_space`] — at startup
/// for the configured space, and on the first write to any other — so a space
/// with no rows left in it still appears here.
///
/// [`ensure_space`]: super::ensure_space
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for a name that is not a valid slug.
pub async fn spaces(db: &Db) -> Result<Vec<SpaceName>, StoreError> {
    let mut resp = checked(db.query(queries::SPACES).await?)?;
    let names: Vec<String> = resp.take(0)?;
    names
        .into_iter()
        .map(|name| Ok(SpaceName::new(name)?))
        .collect()
}

/// Count what `space` holds.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] when a `kind` column holds a spelling this
/// agmem does not know.
pub async fn stats(db: &Db, space: &SpaceName) -> Result<SpaceStats, StoreError> {
    let mut resp = checked(
        db.query(queries::STATS)
            .bind(("space", types::space_str(space)))
            .await?,
    )?;
    let row: StatsRow = resp
        .take::<Option<StatsRow>>(0)?
        .ok_or(StoreError::UnexpectedResponse("stats reported nothing"))?;
    Ok(SpaceStats {
        space: space.clone(),
        memories: count(row.memories),
        live: count(row.live),
        live_by_kind: row
            .live_by_kind
            .into_iter()
            .map(|bucket| Ok((bucket.kind.parse::<Kind>()?, count(bucket.count))))
            .collect::<Result<_, StoreError>>()?,
        episodes: count(row.episodes),
        chunks: count(row.chunks),
        documents: count(row.documents),
    })
}

/// Bind every parameter the filter and liveness clauses reference.
///
/// The query text leaves out the clauses whose filters are empty, so this
/// leaves out their bindings too — SurrealDB rejects a parameter the
/// statement never mentions.
fn bind_filters<'r>(
    mut query: Query<'r, Any>,
    filters: &Filters,
    liveness: Liveness,
) -> Query<'r, Any> {
    if let Liveness::AsOf(instant) = liveness {
        query = query.bind(("as_of", types::to_datetime(instant)));
    }
    if !filters.kinds.is_empty() {
        let kinds: Vec<String> = filters.kinds.iter().copied().map(types::kind_str).collect();
        query = query.bind(("kinds", kinds));
    }
    if !filters.entities.is_empty() {
        query = query.bind(("entities", filters.entities.clone()));
    }
    if !filters.tags.is_empty() {
        query = query.bind(("tags", filters.tags.clone()));
    }
    query
}

/// The row spelling of a list of space names.
fn space_strs(spaces: &[SpaceName]) -> Vec<String> {
    spaces.iter().map(types::space_str).collect()
}

/// A count column as a count; the engine's `count()` cannot be negative.
fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}
