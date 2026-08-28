//! Domain types: the vocabulary every other crate speaks.
//!
//! These mirror the v1 tables (`docs/design.md` §2.2) field for field, and
//! their serde shapes match the rows — the store still owns the SurrealDB
//! representation (record links, datetimes), so nothing here knows about a
//! database.

use jiff::Timestamp;

use crate::CoreError;

/// A validated space (project scope) name.
///
/// Slug rules: `[a-z0-9-_]`, 1–64 chars. Spaces isolate projects inside one
/// store; the reserved [`SpaceName::user`] space holds cross-project personal
/// memory (`docs/design.md` §2.1).
///
/// ```
/// use agmem_core::SpaceName;
/// let s: SpaceName = "my-project_1".parse()?;
/// assert_eq!(s.as_str(), "my-project_1");
/// assert!("Bad Name!".parse::<SpaceName>().is_err());
/// # Ok::<(), agmem_core::CoreError>(())
/// ```
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub struct SpaceName(String);

impl SpaceName {
    /// Maximum length of a space name in bytes.
    pub const MAX_LEN: usize = 64;

    /// Create a validated space name.
    ///
    /// # Errors
    /// [`CoreError::InvalidSpaceName`] when the slug rules are violated.
    pub fn new(name: impl Into<String>) -> Result<Self, CoreError> {
        let name = name.into();
        let valid = !name.is_empty()
            && name.len() <= Self::MAX_LEN
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if valid {
            Ok(Self(name))
        } else {
            Err(CoreError::InvalidSpaceName(name))
        }
    }

    /// The reserved global space for cross-project personal memory.
    pub fn user() -> Self {
        Self("user".to_owned())
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SpaceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for SpaceName {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for SpaceName {
    type Error = CoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SpaceName> for String {
    fn from(name: SpaceName) -> Self {
        name.0
    }
}

/// Defines a record-id newtype over the ULID SurrealQL minted for a row.
///
/// The table is implied by the type, so the store rebuilds the full record id
/// on the way out and strips it on the way in.
macro_rules! record_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        ///
        /// Holds the 26-character ULID half of the record id, without the
        /// table prefix. Validated on construction, including on deserialize.
        #[derive(
            Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Create a validated id.
            ///
            /// # Errors
            /// [`CoreError::InvalidRecordId`] unless `id` is a ULID.
            pub fn new(id: impl Into<String>) -> Result<Self, CoreError> {
                let id = id.into();
                if is_ulid(&id) {
                    Ok(Self(id))
                } else {
                    Err(CoreError::InvalidRecordId(id))
                }
            }

            /// The ULID as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = CoreError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $name {
            type Error = CoreError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

record_id!(
    /// Id of a [`MemoryRecord`].
    MemoryId
);
record_id!(
    /// Id of an [`Episode`].
    EpisodeId
);
record_id!(
    /// Id of an [`EpisodeChunk`].
    ChunkId
);

/// Length of a ULID in its canonical text form.
const ULID_LEN: usize = 26;

/// ULIDs are 26 characters of uppercase Crockford base32.
fn is_ulid(candidate: &str) -> bool {
    candidate.len() == ULID_LEN
        && candidate
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

/// What a memory *is*, which decides how it is used and how fast it fades.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A distilled statement about the world, the user, or the project.
    Fact,
    /// A procedural insight: "X fails when Y; do Z".
    Lesson,
    /// A standing behavioral rule, always part of assembled context.
    Instruction,
}

impl Kind {
    /// The decay class a memory of this kind gets unless told otherwise
    /// (`docs/design.md` §2.3): instructions stand until superseded, lessons
    /// are meant to be few and long-lived, facts fade at the normal rate.
    pub fn default_decay_class(self) -> DecayClass {
        match self {
            Self::Fact => DecayClass::Normal,
            Self::Lesson => DecayClass::Slow,
            Self::Instruction => DecayClass::Pinned,
        }
    }

    /// The wire/row spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Lesson => "lesson",
            Self::Instruction => "instruction",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How fast a memory's retention falls off between accesses.
///
/// The rates themselves live with the scoring functions; this is the label
/// stored on the row.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum DecayClass {
    /// Never decays.
    Pinned,
    /// Fades over months.
    Slow,
    /// The default: fades over weeks.
    #[default]
    Normal,
    /// Working context: fades over days, then gets pruned at startup.
    Fast,
}

impl DecayClass {
    /// The wire/row spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pinned => "pinned",
            Self::Slow => "slow",
            Self::Normal => "normal",
            Self::Fast => "fast",
        }
    }
}

impl std::fmt::Display for DecayClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a memory stopped being live. Memories are closed, never deleted, so
/// the reason is part of the history the agent can walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InvalidReason {
    /// A newer memory replaced it; `superseded_by` points at the successor.
    Superseded,
    /// The agent or user asked for it to be forgotten.
    Forgotten,
    /// A `fast` memory decayed past the pruning threshold.
    Expired,
}

impl InvalidReason {
    /// The wire/row spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Superseded => "superseded",
            Self::Forgotten => "forgotten",
            Self::Expired => "expired",
        }
    }
}

impl std::fmt::Display for InvalidReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a memory came from — provenance, so a distilled claim can always be
/// traced back to the verbatim text it was distilled from.
///
/// Serializes as the row shape: `{ kind, ref }`, with `ref` absent for
/// agent-authored memories.
///
/// ```
/// use agmem_core::{EpisodeId, Source};
/// let source = Source::Episode { episode: "01M145SMNET1XRYA713EWAQTD3".parse::<EpisodeId>()? };
/// assert_eq!(
///     serde_json::to_value(&source)?,
///     serde_json::json!({ "kind": "episode", "ref": "01M145SMNET1XRYA713EWAQTD3" }),
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    /// Distilled from an episode stored here; `ref` links to it.
    Episode {
        /// The episode this was distilled from.
        #[serde(rename = "ref")]
        episode: EpisodeId,
    },
    /// Asserted by the calling agent with no stored episode behind it.
    Agent,
    /// Imported from outside; `ref` names the origin (a URL, a file, a ticket).
    External {
        /// Free-form origin identifier.
        #[serde(rename = "ref")]
        origin: String,
    },
}

/// A distilled, supersedable memory as stored.
///
/// Fields mirror the `memory` table; see `docs/design.md` §2.2. Ranking
/// combines `strength`, `last_accessed` and `decay_class` at read time —
/// nothing here is maintained by a background job.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryRecord {
    /// Record id.
    pub id: MemoryId,
    /// Scope this memory belongs to.
    pub space: SpaceName,
    /// What the memory is.
    pub kind: Kind,
    /// The distilled statement itself.
    pub content: String,
    /// blake3 of the normalized content; the exact-duplicate gate.
    pub content_hash: String,
    /// Denormalized subjects, for direct lookup and filtering.
    pub entities: Vec<String>,
    /// Agent-chosen labels.
    pub tags: Vec<String>,
    /// Absent in BM25-only mode, or until the embedder catches up.
    pub embedding: Option<Vec<f32>>,
    /// How fast this one fades.
    pub decay_class: DecayClass,
    /// Ebbinghaus stability: raised by every recall that returns this memory.
    pub strength: f64,
    /// When recall last returned this memory.
    pub last_accessed: Timestamp,
    /// How often recall has returned it.
    pub access_count: u32,
    /// When the claim started being true.
    pub valid_from: Timestamp,
    /// When it stopped being live; `None` means live.
    pub invalid_at: Option<Timestamp>,
    /// Why it stopped being live.
    pub invalid_reason: Option<InvalidReason>,
    /// The memory this one replaced.
    pub supersedes: Option<MemoryId>,
    /// The memory that replaced this one.
    pub superseded_by: Option<MemoryId>,
    /// Provenance.
    pub source: Source,
    /// When the row was written.
    pub created_at: Timestamp,
}

impl MemoryRecord {
    /// Whether this memory is still live (not superseded, forgotten, expired).
    pub fn is_live(&self) -> bool {
        self.invalid_at.is_none()
    }
}

/// Verbatim ground truth: what was actually said or written, unedited.
///
/// Episodes are append-only and never superseded — distillation is lossy, so
/// the original stays retrievable (`docs/design.md` §2.1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Episode {
    /// Record id.
    pub id: EpisodeId,
    /// Scope this episode belongs to.
    pub space: SpaceName,
    /// The verbatim text.
    pub content: String,
    /// blake3 of the normalized content; the exact-duplicate gate.
    pub content_hash: String,
    /// When the events described happened (defaults to write time).
    pub occurred_at: Timestamp,
    /// Optional grouping key for one conversation or working session.
    pub session: Option<String>,
    /// When the row was written.
    pub created_at: Timestamp,
}

/// A retrieval-sized slice of an [`Episode`]: what search actually matches.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EpisodeChunk {
    /// Record id.
    pub id: ChunkId,
    /// The episode this slice came from.
    pub episode: EpisodeId,
    /// Scope, denormalized from the episode so search filters on one table.
    pub space: SpaceName,
    /// The slice.
    pub text: String,
    /// Zero-based position within the episode.
    pub position: u32,
    /// Absent in BM25-only mode, or until the embedder catches up.
    pub embedding: Option<Vec<f32>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_slugs() {
        for name in ["default", "user", "a", "my-project_1", &"x".repeat(64)] {
            assert!(SpaceName::new(name).is_ok(), "should accept {name:?}");
        }
    }

    #[test]
    fn rejects_invalid_slugs() {
        for name in [
            "",
            "Upper",
            "has space",
            "dot.dot",
            "../etc",
            "é",
            &"x".repeat(65),
        ] {
            assert!(SpaceName::new(name).is_err(), "should reject {name:?}");
        }
    }

    #[test]
    fn kind_picks_its_default_decay_class() {
        assert_eq!(Kind::Fact.default_decay_class(), DecayClass::Normal);
        assert_eq!(Kind::Lesson.default_decay_class(), DecayClass::Slow);
        assert_eq!(Kind::Instruction.default_decay_class(), DecayClass::Pinned);
        assert_eq!(DecayClass::default(), Kind::Fact.default_decay_class());
    }

    #[test]
    fn accepts_ulids_and_rejects_anything_else() {
        assert!(MemoryId::new("01M145SMNET1XRYA713EWAQTD3").is_ok());
        for bad in [
            "",
            "memory:01M145SMNET1XRYA713EWAQTD3",
            "01m145smnet1xrya713ewaqtd3",
            "01M145SMNET1XRYA713EWAQTD",
            "01M145SMNET1XRYA713EWAQTD3X",
        ] {
            assert!(EpisodeId::new(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn enums_use_the_row_spellings() {
        assert_eq!(Kind::Instruction.to_string(), "instruction");
        assert_eq!(DecayClass::Fast.to_string(), "fast");
        assert_eq!(InvalidReason::Superseded.to_string(), "superseded");
        assert_eq!(
            serde_json::to_value(Kind::Lesson).unwrap(),
            serde_json::json!("lesson")
        );
    }

    #[test]
    fn source_round_trips_through_the_row_shape() {
        for (source, json) in [
            (Source::Agent, serde_json::json!({ "kind": "agent" })),
            (
                Source::External {
                    origin: "https://example.com".to_owned(),
                },
                serde_json::json!({ "kind": "external", "ref": "https://example.com" }),
            ),
        ] {
            assert_eq!(serde_json::to_value(&source).unwrap(), json);
            assert_eq!(serde_json::from_value::<Source>(json).unwrap(), source);
        }
    }

    #[test]
    fn ids_and_spaces_validate_while_deserializing() {
        assert!(serde_json::from_value::<MemoryId>(serde_json::json!("nope")).is_err());
        assert!(serde_json::from_value::<SpaceName>(serde_json::json!("Nope!")).is_err());
        assert_eq!(
            serde_json::from_value::<SpaceName>(serde_json::json!("user")).unwrap(),
            SpaceName::user()
        );
    }

    #[test]
    fn memory_record_round_trips() {
        let record = MemoryRecord {
            id: MemoryId::new("01M145SMNH1V44GYMHB5KG5MXJ").unwrap(),
            space: SpaceName::user(),
            kind: Kind::Fact,
            content: "the user prefers Rust over Python".to_owned(),
            content_hash: "deadbeef".to_owned(),
            entities: vec!["user".to_owned()],
            tags: vec![],
            embedding: None,
            decay_class: Kind::Fact.default_decay_class(),
            strength: 1.0,
            last_accessed: Timestamp::UNIX_EPOCH,
            access_count: 0,
            valid_from: Timestamp::UNIX_EPOCH,
            invalid_at: None,
            invalid_reason: None,
            supersedes: None,
            superseded_by: None,
            source: Source::Episode {
                episode: EpisodeId::new("01M145SMNET1XRYA713EWAQTD3").unwrap(),
            },
            created_at: Timestamp::UNIX_EPOCH,
        };

        assert!(record.is_live());
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(
            json["source"],
            serde_json::json!({
                "kind": "episode",
                "ref": "01M145SMNET1XRYA713EWAQTD3",
            })
        );
        assert_eq!(
            serde_json::from_value::<MemoryRecord>(json).unwrap(),
            record
        );
    }
}
