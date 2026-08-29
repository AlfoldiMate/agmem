//! `--reindex`: move a store into the configured embedder's vector space
//! (design §5.5, issue #28).
//!
//! The one sanctioned way to change embedding model or width. Everything else
//! refuses: `migrate::ensure_embedder` compares the configured backend
//! against the pair recorded in `meta` and errors rather than let two models'
//! vectors share one index.
//!
//! It runs instead of serving, and never through the daemon. It holds the
//! single-writer lock for the whole pass, which a live daemon already owns, so
//! an attempt made while sessions are attached fails naming that pid instead
//! of becoming the second writer.

use std::sync::Arc;

use agmem_embed::Embedder;
use agmem_store::db::Db;
use agmem_store::{migrate, repo};

use crate::config::Config;

/// Passages per embed call.
///
/// Not tuned: large enough that the round trips disappear against inference,
/// small enough that the progress line moves on any store worth watching.
const BATCH: usize = 128;

/// What one run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The model the store now belongs to.
    pub model: String,
    /// Its width.
    pub dim: usize,
    /// Rows this run gave a vector to.
    pub embedded: usize,
    /// Whether this run moved the store into another vector space, as opposed
    /// to finishing one an interrupted run left half-done.
    pub moved: bool,
}

/// Take the store, re-embed it, report on stderr.
///
/// # Errors
/// When the lock is held elsewhere, the store will not open, the embedder
/// will not load, the backend is dimensionless, or the engine rejects a
/// statement.
pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    // Same rule as serving: an embedded engine is single-writer, a remote one
    // is the DB server's problem.
    let lock = if cfg.db_is_remote() {
        None
    } else {
        Some(crate::lock::acquire(&cfg.data_dir)?)
    };

    eprintln!("agmem reindex");
    let db = agmem_store::db::connect(&cfg.db_url).await?;
    let schema = migrate::ensure(&db).await?;
    eprintln!("  ok    schema               v{schema}");

    // Loading the model is where a first run downloads it; do it before
    // anything is cleared, so a missing model costs nothing.
    let embedder = crate::embedder::build(cfg)?;
    eprintln!(
        "  ok    embedder             {} ({}d)",
        embedder.model_id(),
        embedder.dim()
    );

    let report = execute(&db, embedder).await?;
    if report.moved {
        eprintln!(
            "  ok    vector space         moved to {} ({}d)",
            report.model, report.dim
        );
    } else {
        eprintln!(
            "  ok    vector space         already {} ({}d)",
            report.model, report.dim
        );
    }
    eprintln!("reindex: {} row(s) re-embedded", report.embedded);
    drop(lock);
    Ok(())
}

/// The pass itself, against an open store and a loaded backend.
///
/// Separate from [`run`] because this is the part worth testing: a stub
/// embedder of another width makes the whole migration runnable without a
/// model download.
///
/// Three phases, in this order for reasons the store's `queries::reindex`
/// documents: clear every vector and redefine both HNSW indexes at the new
/// width, record the new pair in `meta`, then embed until nothing is pending.
/// The pair is written *before* the loop on purpose — the rows without
/// vectors are the only record that the loop is unfinished, so a run
/// interrupted halfway must come back to a store that already knows which
/// model it is being moved to, and resume rather than start over.
///
/// # Errors
/// When the backend is dimensionless, when it answers a batch with the wrong
/// number of vectors, or when the engine rejects a statement.
pub async fn execute(db: &Db, embedder: Arc<dyn Embedder>) -> anyhow::Result<Report> {
    let dim = embedder.dim();
    anyhow::ensure!(
        dim > 0,
        "--embedder none produces no vectors, so there is no space to reindex into; \
         BM25-only mode already opens any store, whatever embedded it"
    );
    let model = embedder.model_id().to_owned();

    let stored = migrate::stored_embedder(db).await?;
    let moved = stored.as_ref() != Some(&(model.clone(), i64::try_from(dim).unwrap_or(i64::MAX)));
    if moved {
        repo::reindex::reset_vectors(db, dim).await?;
        migrate::set_embedder(db, &model, dim).await?;
    }

    let total = repo::reindex::pending_count(db).await?;
    let mut remaining = total;
    let mut embedded = 0usize;
    while remaining > 0 {
        let batch = repo::reindex::pending(db, BATCH).await?;
        anyhow::ensure!(
            !batch.is_empty(),
            "{remaining} row(s) report as unembedded but none came back"
        );
        let sent = batch.len();
        let texts: Vec<String> = batch.iter().map(|row| row.text().to_owned()).collect();
        let vectors = agmem_embed::embed_passages(Arc::clone(&embedder), texts).await?;
        repo::reindex::write_vectors(db, batch, vectors).await?;

        // The pending count is the progress *and* the check: an `UPDATE` over
        // a row that has gone is a silent no-op, so a batch that changed
        // nothing would otherwise come back forever.
        let left = repo::reindex::pending_count(db).await?;
        anyhow::ensure!(
            left < remaining,
            "a batch of {sent} row(s) left {left} still pending; refusing to loop"
        );
        embedded += remaining - left;
        remaining = left;
        eprintln!("  ..    re-embedding         {embedded}/{total} rows");
    }

    Ok(Report {
        model,
        dim,
        embedded,
        moved,
    })
}
