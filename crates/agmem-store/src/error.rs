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
}
