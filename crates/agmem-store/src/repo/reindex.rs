//! Re-embedding a store into another vector space (design §5.5, issue #28).
//!
//! Store-wide rather than space-scoped: vectors belong to the store's
//! embedder, `meta` records one model for all of it, and the HNSW indexes are
//! defined on the tables, not per space.
//!
//! The engine constraints that dictate the order of the three calls here are
//! in [`crate::queries::reindex`]. What matters to a caller is that
//! [`reset_vectors`] and the [`pending`]/[`write_vectors`] loop are two halves
//! of one operation: between them the store answers from BM25 alone, and the
//! only record that the second half is unfinished is the rows themselves.

use surrealdb::types::RecordId;

use super::checked;
use crate::db::Db;
use crate::types::{PassageRow, VectorWrite};
use crate::{StoreError, queries};

/// One row waiting for a vector: opaque except for the text to embed.
///
/// The id stays private so that a caller — which is the server, holding an
/// embedder — never has to name a table or spell a record id to hand the
/// vector back.
pub struct Passage {
    id: RecordId,
    text: String,
}

impl Passage {
    /// The text this row's vector is built from.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Drop every vector in the store and rebuild both HNSW indexes at `dim`.
///
/// One call because the two halves are only correct together: the clear is
/// what lets the redefine succeed, and a redefine that never ran would reject
/// the first vector of the new width. Afterwards every row is pending, which
/// is what the embed loop reads.
///
/// A `dim` of 0 is rejected by the engine rather than here — callers refuse a
/// dimensionless backend earlier, where the message can name `--embedder`.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects.
pub async fn reset_vectors(db: &Db, dim: usize) -> Result<(), StoreError> {
    let mut script = String::new();
    for table in &queries::reindex::VECTORED {
        script.push_str(&table.clear());
        script.push_str(";\n");
    }
    for table in &queries::reindex::VECTORED {
        script.push_str(&table.redefine(dim));
        script.push_str(";\n");
    }
    checked(db.query(script).await?)?;
    Ok(())
}

/// How many rows across the store still have no vector.
///
/// Also what a finished run reports as zero, and what an interrupted one
/// leaves behind for the next `--reindex` — or for `--doctor` to notice.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects.
pub async fn pending_count(db: &Db) -> Result<usize, StoreError> {
    let script = queries::reindex::VECTORED
        .iter()
        .map(|table| table.pending_count())
        .collect::<Vec<_>>()
        .join(";\n");
    let mut resp = checked(db.query(script).await?)?;
    let mut total = 0usize;
    for index in 0..queries::reindex::VECTORED.len() {
        let counts: Vec<i64> = resp.take(index)?;
        total += usize::try_from(counts.first().copied().unwrap_or(0)).unwrap_or(0);
    }
    Ok(total)
}

/// The next rows to embed, at most `limit` of them.
///
/// Both tables are asked in one round trip and the answer is truncated,
/// because the alternative — top up from the second table only once the first
/// is exhausted — is a second round trip on every batch to save fetching a
/// few hundred short strings on one of them.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects.
pub async fn pending(db: &Db, limit: usize) -> Result<Vec<Passage>, StoreError> {
    let script = queries::reindex::VECTORED
        .iter()
        .map(|table| table.pending())
        .collect::<Vec<_>>()
        .join(";\n");
    let mut resp = checked(
        db.query(script)
            .bind(("limit", i64::try_from(limit).unwrap_or(i64::MAX)))
            .await?,
    )?;

    let mut rows = Vec::new();
    for index in 0..queries::reindex::VECTORED.len() {
        let batch: Vec<PassageRow> = resp.take(index)?;
        rows.extend(batch.into_iter().map(|row| Passage {
            id: row.id,
            text: row.text,
        }));
        if rows.len() >= limit {
            break;
        }
    }
    rows.truncate(limit);
    Ok(rows)
}

/// Attach `vectors` to `passages`, positionally.
///
/// # Errors
/// [`StoreError::VectorCount`] when the backend returned a different number
/// of vectors than there were passages, and [`StoreError::Db`] for anything
/// the engine rejects — including a vector whose width is not the one the
/// indexes were just redefined with.
pub async fn write_vectors(
    db: &Db,
    passages: Vec<Passage>,
    vectors: Vec<Vec<f32>>,
) -> Result<(), StoreError> {
    if passages.len() != vectors.len() {
        return Err(StoreError::VectorCount {
            want: passages.len(),
            got: vectors.len(),
        });
    }
    if passages.is_empty() {
        return Ok(());
    }
    let rows: Vec<VectorWrite> = passages
        .into_iter()
        .zip(vectors)
        .map(|(passage, vector)| VectorWrite {
            id: passage.id,
            vector,
        })
        .collect();
    checked(
        db.query(queries::reindex::WRITE_VECTORS)
            .bind(("rows", rows))
            .await?,
    )?;
    Ok(())
}
