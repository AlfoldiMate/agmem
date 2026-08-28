//! agmem SurrealDB repository.
//!
//! Owns the connection (embedded or remote via a connection string), the
//! versioned schema migrations, and every SurrealQL query. Callers speak the
//! `agmem-core` domain types; nothing outside this crate writes SurrealQL.
//! See `docs/design.md` §4.

pub mod db;
pub mod error;
pub mod migrate;
mod queries;
pub mod repo;
mod types;

pub use error::StoreError;
pub use repo::{Batch, BatchOutcome, NewChunk, NewEpisode, NewMemory, Written};
