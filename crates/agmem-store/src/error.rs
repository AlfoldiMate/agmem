//! Store error taxonomy.

use agmem_core::{CoreError, EpisodeId, MemoryId, SpaceName};

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

    /// A caller named an episode that this space does not hold.
    #[error("episode {id} does not exist in space {space}")]
    UnknownEpisode { space: SpaceName, id: EpisodeId },

    /// A row came back in a shape the schema cannot have minted — a record id
    /// that is not a ULID, or an enum spelling this agmem does not know.
    #[error("malformed row from the store: {0}")]
    MalformedRow(#[from] CoreError),

    /// The engine answered with a shape the query cannot produce; a bug here,
    /// not a caller error.
    #[error("unexpected response from the store: {0}")]
    UnexpectedResponse(&'static str),

    /// The on-disk store was written by a newer agmem.
    #[error("store schema is v{found} but this agmem supports up to v{supported}; upgrade agmem")]
    SchemaTooNew { found: u32, supported: u32 },

    /// A backend answered a batch of passages with a different number of
    /// vectors, so nothing can be matched up and half a batch must not land.
    #[error("the embedder returned {got} vectors for {want} passages")]
    VectorCount { want: usize, got: usize },

    /// The store's vectors were built by a different embedder.
    #[error(
        "store was embedded with {stored_model} ({stored_dim}d) but this run is configured for \
         {configured_model} ({configured_dim}d); vectors from two models are not comparable — \
         switch back, or re-embed the store with `agmem --reindex`"
    )]
    EmbedderMismatch {
        stored_model: String,
        stored_dim: i64,
        configured_model: String,
        configured_dim: i64,
    },
}
