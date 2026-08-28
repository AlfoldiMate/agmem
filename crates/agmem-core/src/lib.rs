//! agmem domain model: record types, scoring, dedup, and chunking.
//!
//! This crate is pure: no I/O, no async, no database. Everything here is
//! unit-testable in isolation. See `docs/design.md` §4.

pub mod error;
pub mod model;

pub use error::CoreError;
pub use model::{
    ChunkId, DecayClass, Episode, EpisodeChunk, EpisodeId, InvalidReason, Kind, MemoryId,
    MemoryRecord, Source, SpaceName,
};
