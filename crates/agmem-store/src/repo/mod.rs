//! The repository: everything that reads or writes rows.
//!
//! Callers speak `agmem-core` domain types and nothing else; the SurrealQL
//! lives one module over, in [`crate::queries`]. The two halves are split by
//! direction — writes are design §5.2, reads §5.3 — and share only the
//! helpers below, which every statement needs to survive contact with the
//! engine's error reporting.

mod read;
pub mod reindex;
mod write;

pub use read::{
    Candidate, DEFAULT_POOL, DocumentChurn, DocumentFilter, DocumentHeader, DocumentSummary,
    Embedded, EpisodeDetail, Filters, Hit, Liveness, Lookup, MAX_POOL, Neighbour, RRF_K, Search,
    SpaceStats, StaleContexts, churning_documents, count_matching, direct_lookup, document_citers,
    document_headers, documents, documents_by_title, episode, episode_of_chunk, history_chain,
    live_vectors, locate, nearest_live, orphan_documents, prune_horizon_secs, reinforce,
    search_hybrid, spaces, stale_contexts, stats,
};
pub use write::{
    AlreadyClosed, Batch, BatchOutcome, Forget, Forgotten, NewChunk, NewEpisode, NewMemory,
    Written, ensure_space, forget, insert_batch, prune_expired, supersede,
};

use agmem_core::{MemoryId, SpaceName};
use surrealdb::IndexedResults;
use surrealdb::types::RecordId;

use crate::StoreError;
use crate::db::Db;
use crate::queries;
use crate::types;

/// Reject ids that name no memory in `space` before a statement depends on
/// them — a missing target would otherwise be a silent no-op `UPDATE`.
///
/// # Errors
/// [`StoreError::UnknownMemory`] for the first id that is not in `space`.
pub(super) async fn ensure_memories_exist(
    db: &Db,
    space: &SpaceName,
    ids: &[&MemoryId],
) -> Result<(), StoreError> {
    if ids.is_empty() {
        return Ok(());
    }
    let refs: Vec<RecordId> = ids.iter().copied().map(types::memory_ref).collect();
    let mut resp = checked(
        db.query(queries::write::EXISTING_MEMORIES)
            .bind(("space", types::space_str(space)))
            .bind(("ids", refs))
            .await?,
    )?;
    let found: Vec<String> = resp.take(0)?;
    for id in ids {
        if !found.iter().any(|hit| hit == id.as_str()) {
            return Err(StoreError::UnknownMemory {
                space: space.clone(),
                id: (*id).clone(),
            });
        }
    }
    Ok(())
}

/// Fail with the error that actually explains a failed transaction.
///
/// When one statement fails, every other statement reports a follow-on
/// ("not executed", "Cannot COMMIT"), and `IndexedResults::check` returns
/// whichever has the lowest index — which is nearly always one of those. This
/// prefers the first error that says something.
pub(super) fn checked(mut resp: IndexedResults) -> Result<IndexedResults, StoreError> {
    let mut errors: Vec<(usize, surrealdb::Error)> = resp.take_errors().into_iter().collect();
    errors.sort_by_key(|(index, _)| *index);
    let mut fallback = None;
    for (_, error) in errors {
        if !is_follow_on(&error) {
            return Err(error.into());
        }
        fallback.get_or_insert(error);
    }
    match fallback {
        Some(error) => Err(error.into()),
        None => Ok(resp),
    }
}

/// Whether an error only reports another statement's failure.
fn is_follow_on(error: &surrealdb::Error) -> bool {
    let text = error.to_string();
    text.contains("failed transaction") || text.contains("Cannot COMMIT")
}
