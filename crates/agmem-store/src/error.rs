//! Store error taxonomy.

use agmem_core::{CoreError, MemoryId, SpaceName};

/// Errors from the repository layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Any SurrealDB engine/query failure.
    #[error("database error: {0}")]
    Db(#[from] surrealdb::Error),

    /// A caller named a memory that this space does not hold — most often a
    /// stale `supersedes` id, or one belonging to another space.
    #[error("memory {id} does not exist in space {space}")]
    UnknownMemory { space: SpaceName, id: MemoryId },

    /// A record id came back in a shape the schema cannot have minted.
    #[error("malformed record id from the store: {0}")]
    MalformedId(#[from] CoreError),

    /// The engine answered with a shape the query cannot produce; a bug here,
    /// not a caller error.
    #[error("unexpected response from the store: {0}")]
    UnexpectedResponse(&'static str),

    /// The on-disk store was written by a newer agmem.
    #[error("store schema is v{found} but this agmem supports up to v{supported}; upgrade agmem")]
    SchemaTooNew { found: u32, supported: u32 },

    /// The store's vectors were built by a different embedder.
    #[error(
        "store was embedded with {stored_model} ({stored_dim}d) but this run is configured for \
         {configured_model} ({configured_dim}d); vectors from two models are not comparable — \
         switch back, or re-embed the store (`--reindex`, phase 4)"
    )]
    EmbedderMismatch {
        stored_model: String,
        stored_dim: i64,
        configured_model: String,
        configured_dim: i64,
    },
}
