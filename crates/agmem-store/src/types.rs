//! Row shapes and the core ↔ SurrealDB conversions.
//!
//! Query results map through [`SurrealValue`], not serde, so the row structs
//! here are a second spelling of the domain types in `agmem-core` — deliberate
//! duplication that keeps `core` free of any database dependency (design §4).
//!
//! Write rows carry only the fields a write supplies — everything the schema
//! defaults (`strength`, `last_accessed`, `access_count`, `created_at`) is left
//! to the engine. Read rows carry everything *except* `embedding`: vectors are
//! large and nothing downstream of retrieval looks at them, so reads never
//! project them and the [`MemoryRecord`] they rebuild always has `None` there.
//! [`LiveVectorsRow`] is the one read that needs the vectors too, and it keeps
//! that invariant by handing them back *alongside* the records rather than in
//! them.

use agmem_core::{
    ChunkId, DecayClass, Derivation, Episode, EpisodeChunk, EpisodeId, InvalidReason, Kind,
    MemoryId, MemoryRecord, Source, SpaceName, Writer,
};
use jiff::Timestamp;
use surrealdb::types::{Datetime, RecordId, SurrealValue, Value};

use crate::StoreError;

/// Table holding distilled memories.
pub(crate) const MEMORY: &str = "memory";
/// Table holding verbatim episodes.
pub(crate) const EPISODE: &str = "episode";
/// Table holding retrieval slices of episodes.
pub(crate) const EPISODE_CHUNK: &str = "episode_chunk";

/// A jiff instant as a SurrealDB datetime.
///
/// Infallible in practice: jiff's range (year 1..=9999) sits well inside
/// chrono's, which is what `Datetime` wraps.
pub(crate) fn to_datetime(stamp: Timestamp) -> Datetime {
    let nanos = u32::try_from(stamp.subsec_nanosecond()).unwrap_or(0);
    Datetime::from_timestamp(stamp.as_second(), nanos)
        .expect("jiff timestamps are inside chrono's range")
}

/// A SurrealDB datetime as a jiff instant.
///
/// The conversion the other way is the lossy one: chrono's range is the wider
/// of the two, so a datetime outside jiff's saturates rather than failing the
/// whole read. Nothing agmem writes can land there.
pub(crate) fn to_timestamp(value: &Datetime) -> Timestamp {
    let nanos = i32::try_from(value.timestamp_subsec_nanos()).unwrap_or(0);
    Timestamp::new(value.timestamp(), nanos).unwrap_or(if value.timestamp() < 0 {
        Timestamp::MIN
    } else {
        Timestamp::MAX
    })
}

/// The full record id for a memory ULID.
pub(crate) fn memory_ref(id: &MemoryId) -> RecordId {
    RecordId::new(MEMORY, id.as_str())
}

/// The full record id for an episode ULID.
pub(crate) fn episode_ref(id: &EpisodeId) -> RecordId {
    RecordId::new(EPISODE, id.as_str())
}

/// The full record id for an episode-chunk ULID.
pub(crate) fn chunk_ref(id: &ChunkId) -> RecordId {
    RecordId::new(EPISODE_CHUNK, id.as_str())
}

/// The full record id a citation points at, in whichever table it names.
pub(crate) fn derivation_ref(cited: &Derivation) -> RecordId {
    match cited {
        Derivation::Memory(id) => memory_ref(id),
        Derivation::Episode(id) => episode_ref(id),
    }
}

/// The `memory` columns a write supplies, minus `source`.
///
/// `source` is spelled by the query rather than carried here: a memory
/// distilled from an episode written in the *same* transaction can only name
/// it by SurrealQL variable, since the episode's ULID does not exist until the
/// transaction runs (see [`crate::queries`]).
#[derive(SurrealValue)]
pub(crate) struct MemoryRow {
    pub(crate) space: String,
    pub(crate) kind: String,
    pub(crate) content: String,
    pub(crate) content_hash: String,
    pub(crate) entities: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) embedding: Option<Vec<f32>>,
    pub(crate) decay_class: String,
    pub(crate) valid_from: Datetime,
    pub(crate) supersedes: Vec<RecordId>,
    pub(crate) derived_from: Vec<RecordId>,
    pub(crate) writer: WriterRow,
    pub(crate) novelty: Option<f64>,
}

/// The `writer` object: who performed the write (issue #75).
///
/// Required on every write row and optional on every read row: new rows
/// always record their writer, and rows from before v6 never can.
#[derive(SurrealValue)]
pub(crate) struct WriterRow {
    pub(crate) client: String,
    pub(crate) client_version: Option<String>,
    pub(crate) session: String,
    pub(crate) tool: String,
}

impl WriterRow {
    /// The row spelling of a writer.
    pub(crate) fn new(writer: &Writer) -> Self {
        Self {
            client: writer.client.clone(),
            client_version: writer.client_version.clone(),
            session: writer.session.clone(),
            tool: writer.tool.clone(),
        }
    }

    /// The domain writer this row spells.
    fn into_writer(self) -> Writer {
        Writer {
            client: self.client,
            client_version: self.client_version,
            session: self.session,
            tool: self.tool,
        }
    }
}

/// The `source` object: `{ kind, ref }`, with `ref` absent for `agent`.
#[derive(SurrealValue)]
pub(crate) struct SourceRow {
    pub(crate) kind: String,
    /// `ref` is a keyword-ish column name and not a legal Rust identifier.
    #[surreal(rename = "ref")]
    pub(crate) reference: Option<Value>,
}

impl SourceRow {
    /// The row spelling of a provenance that already knows what it points at.
    pub(crate) fn new(source: &Source) -> Self {
        match source {
            Source::Agent => Self {
                kind: "agent".to_owned(),
                reference: None,
            },
            Source::Episode { episode } => Self {
                kind: "episode".to_owned(),
                reference: Some(Value::RecordId(episode_ref(episode))),
            },
            Source::External { origin } => Self {
                kind: "external".to_owned(),
                reference: Some(Value::String(origin.clone())),
            },
        }
    }
}

/// The `episode` columns a write supplies.
#[derive(SurrealValue)]
pub(crate) struct EpisodeRow {
    pub(crate) space: String,
    pub(crate) content: String,
    pub(crate) content_hash: String,
    pub(crate) occurred_at: Datetime,
    pub(crate) session: Option<String>,
    pub(crate) writer: WriterRow,
}

/// The `episode_chunk` columns a write supplies, minus the ones the query
/// fills in from the episode it is looping over (`episode`, `space`).
#[derive(SurrealValue)]
pub(crate) struct ChunkRow {
    pub(crate) text: String,
    pub(crate) position: i64,
    pub(crate) embedding: Option<Vec<f32>>,
}

/// What one guarded insert reported: the id, and whether it is new.
#[derive(SurrealValue)]
pub(crate) struct WriteRow {
    pub(crate) id: String,
    pub(crate) created: bool,
}

/// The single object an `insert_batch` transaction returns.
#[derive(SurrealValue)]
pub(crate) struct BatchRow {
    pub(crate) episode: Option<WriteRow>,
    pub(crate) memories: Vec<WriteRow>,
}

/// One already-closed `supersedes` target (issue #62): what closed it, so the
/// caller can report the no-op instead of rewriting the close.
#[derive(SurrealValue)]
pub(crate) struct ClosedRow {
    pub(crate) id: String,
    pub(crate) invalid_reason: Option<String>,
    pub(crate) superseded_by: Option<String>,
}

/// Every `memory` column a read projects, minus `embedding`.
///
/// `source` arrives flattened into two scalars because its `ref` is a record
/// link for an episode and a plain string for an external origin; the query
/// unwraps the link so this side never has to guess at a [`Value`]'s shape.
#[derive(SurrealValue)]
pub(crate) struct MemoryReadRow {
    pub(crate) id: String,
    pub(crate) space: String,
    pub(crate) kind: String,
    pub(crate) content: String,
    pub(crate) content_hash: String,
    pub(crate) entities: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) decay_class: String,
    pub(crate) strength: f64,
    pub(crate) last_accessed: Datetime,
    pub(crate) access_count: i64,
    pub(crate) valid_from: Datetime,
    pub(crate) invalid_at: Option<Datetime>,
    pub(crate) invalid_reason: Option<String>,
    pub(crate) supersedes: Vec<String>,
    pub(crate) superseded_by: Option<String>,
    pub(crate) source_kind: String,
    pub(crate) source_ref: Option<String>,
    pub(crate) writer: Option<WriterRow>,
    pub(crate) novelty: Option<f64>,
    pub(crate) derived_from: Vec<DerivationRow>,
    pub(crate) created_at: Datetime,
}

/// One `derived_from` link, as the two halves a projection can name.
///
/// The table travels beside the id rather than as one rendered `table:id`
/// string, because how the engine escapes a record id in text is its business
/// and not something a read should have to parse back.
#[derive(SurrealValue)]
pub(crate) struct DerivationRow {
    pub(crate) table: String,
    pub(crate) id: String,
}

impl DerivationRow {
    /// The domain citation this link spells.
    ///
    /// # Errors
    /// [`StoreError::MalformedRow`] when the id is not a ULID, and
    /// [`StoreError::UnexpectedResponse`] for a table the schema's own type
    /// assertion cannot have allowed in.
    fn into_derivation(self) -> Result<Derivation, StoreError> {
        match self.table.as_str() {
            MEMORY => Ok(Derivation::Memory(MemoryId::new(self.id)?)),
            EPISODE => Ok(Derivation::Episode(EpisodeId::new(self.id)?)),
            _ => Err(StoreError::UnexpectedResponse(
                "a derivation link names neither a memory nor an episode",
            )),
        }
    }
}

impl MemoryReadRow {
    /// The domain record this row spells, with no vector (see the module doc).
    ///
    /// # Errors
    /// [`StoreError::MalformedRow`] when an id or an enum column holds
    /// something this agmem's schema cannot have written.
    pub(crate) fn into_record(self) -> Result<MemoryRecord, StoreError> {
        Ok(MemoryRecord {
            id: MemoryId::new(self.id)?,
            space: SpaceName::new(self.space)?,
            kind: self.kind.parse()?,
            content: self.content,
            content_hash: self.content_hash,
            entities: self.entities,
            tags: self.tags,
            embedding: None,
            decay_class: self.decay_class.parse()?,
            strength: self.strength,
            last_accessed: to_timestamp(&self.last_accessed),
            // The column is a non-negative counter; a negative one could only
            // come from a hand-edited store, and reads it as "never accessed".
            access_count: u32::try_from(self.access_count).unwrap_or(0),
            valid_from: to_timestamp(&self.valid_from),
            invalid_at: self.invalid_at.as_ref().map(to_timestamp),
            invalid_reason: self
                .invalid_reason
                .map(|reason| reason.parse::<InvalidReason>())
                .transpose()?,
            supersedes: self
                .supersedes
                .into_iter()
                .map(MemoryId::new)
                .collect::<Result<_, _>>()?,
            superseded_by: self.superseded_by.map(MemoryId::new).transpose()?,
            source: to_source(&self.source_kind, self.source_ref)?,
            writer: self.writer.map(WriterRow::into_writer),
            // The write path clamps; this guards a hand-edited row, whose
            // out-of-range value would otherwise skew every pool mean it
            // enters.
            novelty: self.novelty.map(|novelty| novelty.clamp(0.0, 1.0)),
            derived_from: self
                .derived_from
                .into_iter()
                .map(DerivationRow::into_derivation)
                .collect::<Result<_, StoreError>>()?,
            created_at: to_timestamp(&self.created_at),
        })
    }
}

/// One memory's vector, carrying the id that pairs it with a
/// [`MemoryReadRow`].
#[derive(SurrealValue)]
pub(crate) struct VectorRow {
    pub(crate) id: String,
    pub(crate) embedding: Vec<f32>,
}

/// A row `--reindex` still has to embed: whichever column that table keeps
/// its text in, under one name.
#[derive(SurrealValue)]
pub(crate) struct PassageRow {
    pub(crate) id: RecordId,
    pub(crate) text: String,
}

/// One freshly built vector on its way back to the row it came from.
#[derive(SurrealValue)]
pub(crate) struct VectorWrite {
    pub(crate) id: RecordId,
    pub(crate) vector: Vec<f32>,
}

/// What `queries::read::live_vectors` answers with: the same selection read
/// twice, once wide and once for the vectors.
///
/// The exception the module doc's "reads never project `embedding`" rule
/// admits — and it is still true of [`MemoryRecord`], because the vector
/// arrives beside the record rather than inside it. Consolidation is the only
/// caller, and it needs both.
#[derive(SurrealValue)]
pub(crate) struct LiveVectorsRow {
    pub(crate) memories: Vec<MemoryReadRow>,
    pub(crate) vectors: Vec<VectorRow>,
}

/// The domain provenance the flattened `source.kind`/`source.ref` pair spells.
fn to_source(kind: &str, reference: Option<String>) -> Result<Source, StoreError> {
    match (kind, reference) {
        ("agent", _) => Ok(Source::Agent),
        ("episode", Some(id)) => Ok(Source::Episode {
            episode: EpisodeId::new(id)?,
        }),
        ("external", Some(origin)) => Ok(Source::External { origin }),
        _ => Err(StoreError::UnexpectedResponse(
            "a memory's source names a kind with no matching ref",
        )),
    }
}

/// Every `episode_chunk` column a read projects, minus `embedding`.
#[derive(SurrealValue)]
pub(crate) struct ChunkReadRow {
    pub(crate) id: String,
    pub(crate) episode: String,
    pub(crate) space: String,
    pub(crate) text: String,
    pub(crate) position: i64,
    pub(crate) occurred_at: Option<Datetime>,
}

impl ChunkReadRow {
    /// The domain chunk this row spells, with no vector (see the module doc).
    ///
    /// # Errors
    /// [`StoreError::MalformedRow`] when an id column is not a ULID.
    pub(crate) fn into_chunk(self) -> Result<EpisodeChunk, StoreError> {
        Ok(EpisodeChunk {
            id: ChunkId::new(self.id)?,
            episode: EpisodeId::new(self.episode)?,
            space: SpaceName::new(self.space)?,
            text: self.text,
            occurred_at: self.occurred_at.as_ref().map(to_timestamp),
            embedding: None,
            // Positions are assigned from a `Vec` index, so this cannot be
            // negative unless the store was edited by hand.
            position: u32::try_from(self.position).unwrap_or(0),
        })
    }
}

/// Every `episode` column a read projects. Episodes carry no vector of their
/// own — retrieval matches their chunks — so there is nothing to leave out.
#[derive(SurrealValue)]
pub(crate) struct EpisodeReadRow {
    pub(crate) id: String,
    pub(crate) space: String,
    pub(crate) content: String,
    pub(crate) content_hash: String,
    pub(crate) occurred_at: Datetime,
    pub(crate) session: Option<String>,
    pub(crate) created_at: Datetime,
}

impl EpisodeReadRow {
    /// The domain episode this row spells.
    ///
    /// # Errors
    /// [`StoreError::MalformedRow`] when an id column is not a ULID.
    pub(crate) fn into_episode(self) -> Result<Episode, StoreError> {
        Ok(Episode {
            id: EpisodeId::new(self.id)?,
            space: SpaceName::new(self.space)?,
            content: self.content,
            content_hash: self.content_hash,
            occurred_at: to_timestamp(&self.occurred_at),
            session: self.session,
            created_at: to_timestamp(&self.created_at),
        })
    }
}

/// The single object an episode lookup returns: the verbatim row, the slices
/// retrieval matches, and the claims distilled from it.
#[derive(SurrealValue)]
pub(crate) struct EpisodeDetailRow {
    pub(crate) episode: EpisodeReadRow,
    pub(crate) chunks: Vec<ChunkReadRow>,
    pub(crate) derived: Vec<MemoryReadRow>,
}

/// A live memory near one probe vector: which row, what it says, and how far.
///
/// `content` rides along because the gate's answer is read by an agent
/// deciding whether the new claim corrects this one, and an id alone is not
/// something to decide on.
#[derive(SurrealValue)]
pub(crate) struct NeighbourRow {
    pub(crate) id: String,
    pub(crate) content: String,
    pub(crate) distance: f64,
}

/// One fused candidate: which row, and the score that surfaced it.
#[derive(SurrealValue)]
pub(crate) struct ScoreRow {
    pub(crate) id: String,
    pub(crate) table: String,
    pub(crate) rrf: f64,
}

/// One vector-arm candidate: which row, and its cosine distance to the query.
///
/// A row can appear in both vector arms' lists only by naming two tables at
/// once, which it cannot — but the join is keyed on `(table, id)` anyway, the
/// same pair `scored` resolves rows by.
#[derive(SurrealValue)]
pub(crate) struct NearestRow {
    pub(crate) id: String,
    pub(crate) table: String,
    pub(crate) d: f64,
}

/// The single object a hybrid search returns: the fused order, the vector
/// arms' distances, and the rows.
#[derive(SurrealValue)]
pub(crate) struct SearchRow {
    pub(crate) scored: Vec<ScoreRow>,
    pub(crate) nearest: Vec<NearestRow>,
    pub(crate) memories: Vec<MemoryReadRow>,
    pub(crate) chunks: Vec<ChunkReadRow>,
}

/// The single object a history walk returns: the chain order, and the rows.
#[derive(SurrealValue)]
pub(crate) struct ChainRow {
    pub(crate) ids: Vec<String>,
    pub(crate) rows: Vec<MemoryReadRow>,
}

/// One `GROUP BY kind` bucket.
#[derive(SurrealValue)]
pub(crate) struct KindCountRow {
    pub(crate) kind: String,
    pub(crate) count: i64,
}

/// The single object a stats query returns.
#[derive(SurrealValue)]
pub(crate) struct StatsRow {
    pub(crate) memories: i64,
    pub(crate) live: i64,
    pub(crate) episodes: i64,
    pub(crate) chunks: i64,
    pub(crate) live_by_kind: Vec<KindCountRow>,
}

/// The row spelling of a space name.
pub(crate) fn space_str(space: &SpaceName) -> String {
    space.as_str().to_owned()
}

/// The row spelling of a kind.
pub(crate) fn kind_str(kind: Kind) -> String {
    kind.as_str().to_owned()
}

/// The row spelling of a decay class.
pub(crate) fn decay_class_str(class: DecayClass) -> String {
    class.as_str().to_owned()
}

/// What a [`crate::queries::read::LOCATE`] found: which of the ids asked
/// about name a memory in the space, and which name an episode.
#[derive(SurrealValue)]
pub(crate) struct LocatedRow {
    pub(crate) memories: Vec<String>,
    pub(crate) episodes: Vec<String>,
}

/// The single object a purge returns: what it deleted, by id.
#[derive(SurrealValue)]
pub(crate) struct PurgedRow {
    pub(crate) chunks: i64,
    pub(crate) episodes: Vec<String>,
    pub(crate) memories: Vec<String>,
}
