//! agmem domain model: record types, scoring, dedup, and chunking.
//!
//! This crate is pure: no I/O, no async, no database. Everything here is
//! unit-testable in isolation. See `docs/design.md` §4.

pub mod chunk;
pub mod dedup;
pub mod error;
pub mod model;
pub mod scoring;

pub use error::CoreError;
pub use model::{
    ChunkId, DecayClass, Episode, EpisodeChunk, EpisodeId, InvalidReason, Kind, MemoryId,
    MemoryRecord, Source, SpaceName,
};
