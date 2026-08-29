//! The SurrealQL behind `--reindex` (design §5.5, issue #28).
//!
//! The order of the pass is engine-dictated, not stylistic. HNSW carries its
//! width in the index definition and the engine checks every write against
//! it, so with a `DIMENSION 384` index live, `UPDATE … SET embedding = <256
//! floats>` fails with "Incorrect vector dimension (256). Expected a vector of
//! 384 dimension." `DEFINE INDEX OVERWRITE` at the new width fails the same
//! way — it rebuilds against the rows as they stand — and the *old*
//! definition survives that failure, so the attempt does not even leave the
//! store half-moved. The one write both accept is `embedding = NONE`.
//!
//! So the pass is: clear every vector, redefine both indexes at the new
//! width, then embed. That order pays for the resume marker as well — a row
//! with no vector is a row still to do, so an interrupted pass needs no
//! bookkeeping column and no schema version of its own. (All of it verified
//! against `surreal sql --endpoint memory`, 3.2.3.)

use crate::types::{EPISODE_CHUNK, MEMORY};

/// A table that carries vectors: the text they are built from, and the HNSW
/// index over them.
pub(crate) struct Vectored {
    /// Table name.
    pub(crate) table: &'static str,
    /// Column the passage text comes from.
    pub(crate) text: &'static str,
    /// Name of the HNSW index over `embedding`.
    pub(crate) index: &'static str,
}

/// Every table `--reindex` walks, `memory` first so that the rows an agent
/// reads back soonest are the ones finished first.
pub(crate) const VECTORED: [Vectored; 2] = [
    Vectored {
        table: MEMORY,
        text: "content",
        index: "mem_vec",
    },
    Vectored {
        table: EPISODE_CHUNK,
        text: "text",
        index: "ec_vec",
    },
];

impl Vectored {
    /// Drop every vector in this table, whatever width it holds.
    pub(crate) fn clear(&self) -> String {
        format!("UPDATE {} SET embedding = NONE", self.table)
    }

    /// Rebuild the HNSW index at `dim`.
    ///
    /// `OVERWRITE` rather than `REMOVE` then `DEFINE`: it is one statement, so
    /// there is no window in which the table has no index at all. The
    /// dimension is formatted into the text because a `DEFINE` takes no bound
    /// parameters — the same reason the KNN operator's `K` is.
    pub(crate) fn redefine(&self, dim: usize) -> String {
        format!(
            "DEFINE INDEX OVERWRITE {} ON {} FIELDS embedding HNSW DIMENSION {dim} DIST COSINE",
            self.index, self.table
        )
    }

    /// The next rows still waiting for a vector.
    pub(crate) fn pending(&self) -> String {
        format!(
            "SELECT id, {} AS text FROM {} WHERE embedding IS NONE LIMIT $limit",
            self.text, self.table
        )
    }

    /// How many rows in this table have no vector.
    pub(crate) fn pending_count(&self) -> String {
        format!(
            "SELECT VALUE count() FROM {} WHERE embedding IS NONE GROUP ALL",
            self.table
        )
    }
}

/// Attach a batch of vectors to the rows they were built from.
///
/// `UPDATE` over an id that names nothing is a silent no-op, which is why the
/// caller re-counts what is left rather than trusting this to have landed.
pub(crate) const WRITE_VECTORS: &str = "FOR $row IN $rows {
    UPDATE $row.id SET embedding = $row.vector
}";
