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
//! always has `None` there, whatever the store holds.

use std::collections::HashMap;

use agmem_core::{
    ChunkId, Episode, EpisodeChunk, EpisodeId, Kind, MemoryId, MemoryRecord, SpaceName, dedup,
};
use jiff::Timestamp;
use surrealdb::engine::any::Any;
use surrealdb::method::Query;
use surrealdb::types::RecordId;

use super::{checked, ensure_memories_exist};
use crate::StoreError;
use crate::db::Db;
use crate::queries::read as queries;
use crate::types::{
    self, ChainRow, ChunkReadRow, EpisodeDetailRow, MemoryReadRow, NeighbourRow, SearchRow,
    StatsRow,
};

/// The candidate pool one recall considers, before `k` truncates it
/// (design §5.3, `AGMEM_POOL`).
pub const DEFAULT_POOL: usize = 64;

/// The largest pool or lookup limit the store will build a query for; larger
/// values are clamped to it.
pub const MAX_POOL: usize = queries::MAX_POOL;

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
    /// The row itself.
    pub hit: Hit,
}

/// The nearest live memory to a candidate, and how close it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    /// The memory already in the store.
    pub id: MemoryId,
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
    if search.text.is_none() && search.vector.is_none() {
        return Ok(Vec::new());
    }
    let script = queries::search(search);
    let mut query = db
        .query(&script.text)
        .bind(("spaces", space_strs(&search.spaces)));
    if let Some(text) = &search.text {
        query = query.bind(("text", text.clone()));
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

    let mut candidates = Vec::with_capacity(row.scored.len());
    for scored in row.scored {
        // The fused ids and the rows come from the same request, so every
        // scored id has a row; a miss means the query text and the row
        // structs have drifted apart.
        let missing = StoreError::UnexpectedResponse("the search scored a row it did not return");
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

/// The nearest live memory in `space` to each of `vectors`, in input order.
///
/// The near-dup gate of the write path (design §5.2 step 4): a memory whose
/// nearest live neighbour is close enough is the same claim in different
/// words, and `remember` reports it instead of storing it again. Deciding
/// *how* close is [`dedup::is_near_duplicate`]'s; this only measures.
///
/// `None` in a slot means the space holds no vector to compare against yet.
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
) -> Result<Vec<Option<Neighbour>>, StoreError> {
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
    let rows: Vec<Option<NeighbourRow>> = resp.take(script.result_index)?;
    if rows.len() != vectors.len() {
        return Err(StoreError::UnexpectedResponse(
            "the gate probed a different number of vectors than it was given",
        ));
    }
    rows.into_iter()
        .map(|row| {
            row.map(|row| {
                Ok(Neighbour {
                    id: MemoryId::new(row.id)?,
                    similarity: dedup::similarity_from_distance(row.distance),
                })
            })
            .transpose()
        })
        .collect()
}

/// Reinforce every memory a recall returned, in one statement.
///
/// Raising `strength` flattens that memory's decay curve, which is the whole
/// mechanism by which use keeps a memory alive (design §2.3). Ids that name
/// nothing are skipped rather than refused — this runs after the hits are
/// already on their way to the agent, so it must not be able to fail a
/// recall. Returns how many rows were actually touched.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects.
pub async fn reinforce(db: &Db, ids: &[MemoryId]) -> Result<usize, StoreError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let refs: Vec<RecordId> = ids.iter().map(types::memory_ref).collect();
    let mut resp = checked(db.query(queries::REINFORCE).bind(("ids", refs)).await?)?;
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
