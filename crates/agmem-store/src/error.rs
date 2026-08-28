//! Store error taxonomy.

/// Errors from the repository layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// Any SurrealDB engine/query failure.
    #[error("database error: {0}")]
    Db(#[from] surrealdb::Error),

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
