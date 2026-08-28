//! Error taxonomy for the domain layer.

/// Errors produced by domain-level validation and processing.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    /// A space name failed slug validation (`[a-z0-9-_]{1,64}`).
    #[error("invalid space name: {0:?}")]
    InvalidSpaceName(String),

    /// A record id was not the ULID half of a SurrealDB record id — most
    /// often a full `table:id` that should have been stripped first.
    #[error("invalid record id: {0:?}; expected a 26-character ULID")]
    InvalidRecordId(String),
}
