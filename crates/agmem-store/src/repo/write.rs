//! The write half (design §5.2).
//!
//! A `remember` call lands as one transaction — episode, chunks, memories and
//! supersessions commit together or not at all — and reports each row as
//! either created or an exact duplicate, never as an error. The agent decides
//! what a duplicate means.

use agmem_core::{DecayClass, EpisodeId, Kind, MemoryId, Source, SpaceName, dedup};
use jiff::Timestamp;
use surrealdb::types::RecordId;

use super::{checked, ensure_memories_exist};
use crate::StoreError;
use crate::db::Db;
use crate::queries::write::{self as queries, MemoryShape};
use crate::types::{
    self, BatchRow, ChunkRow, EpisodeRow, MemoryRow, PurgedRow, SourceRow, WriteRow,
};

/// A memory to write.
///
/// The content hash is derived from `content` here, so the exact-duplicate
/// gate can never disagree with what is stored.
#[derive(Debug, Clone)]
pub struct NewMemory {
    /// What the memory is.
    pub kind: Kind,
    /// The distilled statement.
    pub content: String,
    /// Denormalized subjects.
    pub entities: Vec<String>,
    /// Agent-chosen labels.
    pub tags: Vec<String>,
    /// The content vector; `None` in BM25-only mode.
    pub embedding: Option<Vec<f32>>,
    /// Overrides [`Kind::default_decay_class`].
    pub decay_class: Option<DecayClass>,
    /// When the claim started being true; defaults to write time.
    pub valid_from: Option<Timestamp>,
    /// The live memory this one closes, if any.
    pub supersedes: Option<MemoryId>,
    /// Provenance; `None` means the batch's episode, or `agent` without one.
    pub source: Option<Source>,
}

impl NewMemory {
    /// A memory with only what it cannot do without; set the rest directly.
    pub fn new(kind: Kind, content: impl Into<String>) -> Self {
        Self {
            kind,
            content: content.into(),
            entities: Vec::new(),
            tags: Vec::new(),
            embedding: None,
            decay_class: None,
            valid_from: None,
            supersedes: None,
            source: None,
        }
    }
}

/// One retrieval-sized slice of an episode; position is its place in the list.
#[derive(Debug, Clone)]
pub struct NewChunk {
    /// The slice, verbatim.
    pub text: String,
    /// The slice's vector; `None` in BM25-only mode.
    pub embedding: Option<Vec<f32>>,
}

/// Verbatim ground truth, written alongside what was distilled from it.
///
/// Splitting the content into `chunks` is the caller's job (`core::chunk`), so
/// the same pass can embed them.
#[derive(Debug, Clone)]
pub struct NewEpisode {
    /// The verbatim text.
    pub content: String,
    /// When the events happened; defaults to write time.
    pub occurred_at: Option<Timestamp>,
    /// Grouping key for one conversation or working session.
    pub session: Option<String>,
    /// Retrieval slices of `content`, in order.
    pub chunks: Vec<NewChunk>,
}

impl NewEpisode {
    /// An episode with only what it cannot do without; set the rest directly.
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            occurred_at: None,
            session: None,
            chunks: Vec::new(),
        }
    }
}

/// One `remember` call's worth of writes.
#[derive(Debug, Clone)]
pub struct Batch {
    /// The scope everything in the batch belongs to.
    pub space: SpaceName,
    /// Optional verbatim episode; memories without an explicit `source` are
    /// linked to it.
    pub episode: Option<NewEpisode>,
    /// The distilled memories.
    pub memories: Vec<NewMemory>,
}

/// What became of one row a batch asked for.
///
/// An exact duplicate is a result, not a failure: `remember` reports the id of
/// the row that already holds the content and leaves the decision — accept the
/// NOOP, or re-send with `supersedes` — to the calling agent (design §3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written<I> {
    /// A new row, with the id it was minted with.
    Created(I),
    /// The content was already stored in this space, under this id.
    Duplicate(I),
}

impl<I> Written<I> {
    /// The id, whether it was just minted or already existed.
    pub fn id(&self) -> &I {
        match self {
            Self::Created(id) | Self::Duplicate(id) => id,
        }
    }

    /// Whether a row was actually written.
    pub fn is_created(&self) -> bool {
        matches!(self, Self::Created(_))
    }

    /// Take the id, whether it was just minted or already existed.
    pub fn into_id(self) -> I {
        match self {
            Self::Created(id) | Self::Duplicate(id) => id,
        }
    }
}

/// What one [`insert_batch`] wrote, positionally aligned with its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOutcome {
    /// The episode, if the batch carried one.
    pub episode: Option<Written<EpisodeId>>,
    /// One entry per [`Batch::memories`] entry, in the same order.
    pub memories: Vec<Written<MemoryId>>,
    /// The memories this batch closed — the `supersedes` targets of the
    /// memories that were actually created.
    pub superseded: Vec<MemoryId>,
}

/// Write a whole `remember` call in one transaction.
///
/// Exact duplicates (`(space, content_hash)`) are reported, not inserted:
/// letting one reach the unique index would abort the entire transaction, so
/// each insert is guarded by a lookup in the same transaction. Two identical
/// memories *within* one batch collapse the same way.
///
/// # Errors
/// [`StoreError::UnknownMemory`] when a `supersedes` target is not in this
/// space, and [`StoreError::Db`] for anything the engine rejects — in which
/// case nothing at all was written.
pub async fn insert_batch(db: &Db, batch: Batch) -> Result<BatchOutcome, StoreError> {
    let Batch {
        space,
        episode,
        memories,
    } = batch;

    let targets: Vec<&MemoryId> = memories
        .iter()
        .filter_map(|memory| memory.supersedes.as_ref())
        .collect();
    ensure_memories_exist(db, &space, &targets).await?;

    let shapes: Vec<MemoryShape<'_>> = memories
        .iter()
        .map(|memory| MemoryShape {
            source_is_batch_episode: memory.source.is_none() && episode.is_some(),
            supersedes: memory.supersedes.as_ref(),
        })
        .collect();
    let script = queries::insert_batch(&shapes, episode.is_some());

    let mut query = db
        .query(&script.text)
        .bind(("space", types::space_str(&space)));
    if let Some(episode) = &episode {
        let hash = dedup::content_hash(&episode.content);
        let chunks: Vec<ChunkRow> = episode
            .chunks
            .iter()
            .enumerate()
            .map(|(position, chunk)| ChunkRow {
                text: chunk.text.clone(),
                position: i64::try_from(position).unwrap_or(i64::MAX),
                embedding: chunk.embedding.clone(),
            })
            .collect();
        query = query
            .bind(("ep_hash", hash.clone()))
            .bind((
                "ep_row",
                EpisodeRow {
                    space: types::space_str(&space),
                    content: episode.content.clone(),
                    content_hash: hash,
                    occurred_at: types::to_datetime(episode.occurred_at.unwrap_or_else(now)),
                    session: episode.session.clone(),
                },
            ))
            .bind(("ep_chunks", chunks));
    }
    for (index, memory) in memories.iter().enumerate() {
        let hash = dedup::content_hash(&memory.content);
        query = query.bind((format!("hash{index}"), hash.clone())).bind((
            format!("row{index}"),
            MemoryRow {
                space: types::space_str(&space),
                kind: types::kind_str(memory.kind),
                content: memory.content.clone(),
                content_hash: hash,
                entities: memory.entities.clone(),
                tags: memory.tags.clone(),
                embedding: memory.embedding.clone(),
                decay_class: types::decay_class_str(
                    memory
                        .decay_class
                        .unwrap_or_else(|| memory.kind.default_decay_class()),
                ),
                valid_from: types::to_datetime(memory.valid_from.unwrap_or_else(now)),
                supersedes: memory.supersedes.as_ref().map(types::memory_ref),
            },
        ));
        if !shapes[index].source_is_batch_episode {
            let source = memory.source.clone().unwrap_or(Source::Agent);
            query = query.bind((format!("src{index}"), SourceRow::new(&source)));
        }
        if let Some(old) = &memory.supersedes {
            query = query.bind((format!("old{index}"), types::memory_ref(old)));
        }
    }

    let mut resp = checked(query.await?)?;
    let row: BatchRow = resp
        .take::<Option<BatchRow>>(script.result_index)?
        .ok_or(StoreError::UnexpectedResponse("the batch reported nothing"))?;
    if row.memories.len() != memories.len() {
        return Err(StoreError::UnexpectedResponse(
            "the batch reported a different number of memories than it was given",
        ));
    }

    let outcomes: Vec<Written<MemoryId>> = row
        .memories
        .into_iter()
        .map(|row| written(row, MemoryId::new))
        .collect::<Result<_, StoreError>>()?;
    let superseded = memories
        .iter()
        .zip(&outcomes)
        .filter(|(_, outcome)| outcome.is_created())
        .filter_map(|(memory, _)| memory.supersedes.clone())
        .collect();
    Ok(BatchOutcome {
        episode: row
            .episode
            .map(|row| written(row, EpisodeId::new))
            .transpose()?,
        memories: outcomes,
        superseded,
    })
}

/// Register `space` in the space registry unless it is already there.
///
/// Isolation is a field on every row, not a database, so this table is a
/// listing rather than a gate — nothing reads it to decide whether a write is
/// allowed. Startup calls this every run (design §5.1 step 8), so it has to be
/// idempotent.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects.
pub async fn ensure_space(db: &Db, space: &SpaceName) -> Result<(), StoreError> {
    checked(
        db.query(queries::ENSURE_SPACE)
            .bind(("name", types::space_str(space)))
            .await?,
    )?;
    Ok(())
}

/// Close `old` in favour of `new`, atomically.
///
/// The boundary comes from the successor's `valid_from`, so walking the chain
/// backwards from a timestamp lands on exactly one live claim.
///
/// # Errors
/// [`StoreError::UnknownMemory`] when either id is not in `space`.
pub async fn supersede(
    db: &Db,
    space: &SpaceName,
    old: &MemoryId,
    new: &MemoryId,
) -> Result<(), StoreError> {
    ensure_memories_exist(db, space, &[old, new]).await?;
    checked(
        db.query(queries::SUPERSEDE)
            .bind(("old", types::memory_ref(old)))
            .bind(("new", types::memory_ref(new)))
            .await?,
    )?;
    Ok(())
}

/// What one `forget` call acts on (design §5.4).
///
/// The ids are exactly the rows to touch. Resolving a reference, expanding a
/// supersession chain and confirming the scope of a query all happen above
/// this, so the only judgement left here is the space guard — which is not
/// judgement so much as the same rule every read follows: an id is a
/// capability inside a space, not across the store.
#[derive(Debug, Clone)]
pub struct Forget {
    /// The scopes a row must belong to before it may be touched.
    pub spaces: Vec<SpaceName>,
    /// The memories to close, or to delete under `purge`.
    pub memories: Vec<MemoryId>,
    /// The episodes to delete; a soft forget ignores them, because verbatim
    /// text has no validity window to close.
    pub episodes: Vec<EpisodeId>,
    /// Delete outright instead of closing.
    pub purge: bool,
}

/// What a forget actually changed.
///
/// Ids that named nothing in `spaces`, and memories a correction had already
/// closed, are simply absent: the caller asked for a state, and this reports
/// the rows that moved into it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forgotten {
    /// The memories closed, or deleted under `purge`.
    pub memories: Vec<MemoryId>,
    /// The episodes deleted; always empty for a soft forget.
    pub episodes: Vec<EpisodeId>,
    /// How many retrieval slices went with those episodes.
    pub chunks: usize,
}

/// Close or delete what a `forget` call resolved to (design §5.4).
///
/// Soft is the default and the whole point: a closed memory stops answering
/// `recall` while staying readable through `inspect`, dated and labelled
/// `forgotten`, so "we decided not to keep this" and "this was never said"
/// stay distinguishable. `purge` is the escape hatch for text that must not
/// remain on disk, and it takes the episode's slices with it.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects, and
/// [`StoreError::MalformedRow`] for an id the engine reported that is not a
/// ULID. A purge is one transaction: it lands whole or not at all.
pub async fn forget(db: &Db, request: &Forget) -> Result<Forgotten, StoreError> {
    let spaces: Vec<String> = request.spaces.iter().map(types::space_str).collect();
    let memories: Vec<RecordId> = request.memories.iter().map(types::memory_ref).collect();

    if !request.purge {
        let mut resp = checked(
            db.query(queries::FORGET_SOFT)
                .bind(("spaces", spaces))
                .bind(("memories", memories))
                .await?,
        )?;
        let closed: Vec<String> = resp.take(0)?;
        return Ok(Forgotten {
            memories: ids(closed, MemoryId::new)?,
            episodes: Vec::new(),
            chunks: 0,
        });
    }

    let episodes: Vec<RecordId> = request.episodes.iter().map(types::episode_ref).collect();
    let script = queries::forget_purge();
    let mut resp = checked(
        db.query(&script.text)
            .bind(("spaces", spaces))
            .bind(("memories", memories))
            .bind(("episodes", episodes))
            .await?,
    )?;
    let row: PurgedRow = resp
        .take::<Option<PurgedRow>>(script.result_index)?
        .ok_or(StoreError::UnexpectedResponse("the purge reported nothing"))?;
    Ok(Forgotten {
        memories: ids(row.memories, MemoryId::new)?,
        episodes: ids(row.episodes, EpisodeId::new)?,
        chunks: usize::try_from(row.chunks).unwrap_or(0),
    })
}

/// Parse a list of reported ids, failing on the first the schema cannot have
/// minted.
fn ids<I, E>(raw: Vec<String>, id: impl Fn(String) -> Result<I, E>) -> Result<Vec<I>, StoreError>
where
    StoreError: From<E>,
{
    raw.into_iter().map(|one| Ok(id(one)?)).collect()
}

/// Turn one reported row into a typed outcome.
fn written<I, E>(
    row: WriteRow,
    id: impl Fn(String) -> Result<I, E>,
) -> Result<Written<I>, StoreError>
where
    StoreError: From<E>,
{
    let parsed = id(row.id)?;
    Ok(if row.created {
        Written::Created(parsed)
    } else {
        Written::Duplicate(parsed)
    })
}

/// Write time, for the fields a caller left open.
fn now() -> Timestamp {
    Timestamp::now()
}
