//! Row shapes and the core ↔ SurrealDB conversions.
//!
//! Query results map through [`SurrealValue`], not serde, so the row structs
//! here are a second spelling of the domain types in `agmem-core` — deliberate
//! duplication that keeps `core` free of any database dependency (design §4).
//!
//! Only the fields a *write* supplies appear below: everything the schema
//! defaults (`strength`, `last_accessed`, `access_count`, `created_at`) is left
//! to the engine, and everything a *read* projects arrives with issue #13.

use agmem_core::{DecayClass, EpisodeId, Kind, MemoryId, Source, SpaceName};
use jiff::Timestamp;
use surrealdb::types::{Datetime, RecordId, SurrealValue, Value};

/// Table holding distilled memories.
pub(crate) const MEMORY: &str = "memory";
/// Table holding verbatim episodes.
pub(crate) const EPISODE: &str = "episode";

/// A jiff instant as a SurrealDB datetime.
///
/// Infallible in practice: jiff's range (year 1..=9999) sits well inside
/// chrono's, which is what `Datetime` wraps.
pub(crate) fn to_datetime(stamp: Timestamp) -> Datetime {
    let nanos = u32::try_from(stamp.subsec_nanosecond()).unwrap_or(0);
    Datetime::from_timestamp(stamp.as_second(), nanos)
        .expect("jiff timestamps are inside chrono's range")
}

/// The full record id for a memory ULID.
pub(crate) fn memory_ref(id: &MemoryId) -> RecordId {
    RecordId::new(MEMORY, id.as_str())
}

/// The full record id for an episode ULID.
pub(crate) fn episode_ref(id: &EpisodeId) -> RecordId {
    RecordId::new(EPISODE, id.as_str())
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
    pub(crate) supersedes: Option<RecordId>,
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
