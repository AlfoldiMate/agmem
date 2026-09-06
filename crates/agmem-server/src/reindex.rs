//! Re-embedding a store into the configured model's vector space (design
//! §5.5, issues #28 and #138).
//!
//! Two doors onto one loop. [`drain`] is the background one: the server's
//! startup moved a mismatched store (or found rows without a vector) and
//! the process that holds the store finishes the job while it serves.
//! [`run`] is `agmem reindex`, the deliberate one: with no session attached,
//! re-embed everything now — optionally under another `--model` — and exit.
//! Both write the new space into `meta` before the first vector, so a run
//! interrupted halfway resumes rather than starts over: the rows without a
//! vector are the only record that it is unfinished.
//!
//! `agmem reindex` never runs through the daemon. It needs the store to
//! itself, which a live daemon already owns, so it refuses while one answers
//! on the socket — naming the pid — rather than become the second writer.

use std::sync::Arc;
use std::time::Duration;

use agmem_embed::Embedder;
use agmem_store::db::Db;
use agmem_store::{migrate, repo};

use crate::config::{Config, ReindexArgs};
use crate::startup::VectorState;

/// Passages per embed call in the offline pass.
///
/// Not tuned: large enough that the round trips disappear against inference,
/// small enough that the progress line moves on any store worth watching.
const BATCH: usize = 128;

/// Passages per embed call in the background drain.
///
/// Small on purpose: the model serialises on one worker thread, so a live
/// query arriving mid-batch waits for the batch ahead of it. Sixteen rows
/// is ~0.2 s on Metal and a second or two on a CPU-only host.
pub const DRAIN_BATCH: usize = 16;

/// How long the drain waits after a failed batch before trying again.
const DRAIN_RETRY: Duration = Duration::from_secs(5);

/// Consecutive failed batches after which the drain stops and leaves the
/// rest to `agmem reindex` — a model that answers nothing is not going to
/// start answering by being asked again.
const DRAIN_GIVE_UP: u32 = 5;

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

/// `agmem reindex`: take the store, re-embed it, report on stderr.
///
/// `--model` on the subcommand names the space to move into for this run
/// only; without it the configured model is the target, which makes a bare
/// `agmem reindex` the way to finish what a background drain left.
///
/// # Errors
/// When a daemon is serving the store, the lock is held elsewhere, the store
/// will not open, the embedder will not load, the backend is dimensionless,
/// or the engine rejects a statement.
pub async fn run(cfg: &Config, args: ReindexArgs) -> anyhow::Result<()> {
    let cfg = match args.model {
        Some(model) => Config {
            model,
            ..cfg.clone()
        },
        None => cfg.clone(),
    };

    // The socket answers before the lock does: a daemon that is up holds the
    // lock, and the lock's own refusal only says "another process". This one
    // says which, and what to do about it.
    #[cfg(unix)]
    if let Some(socket) = live_daemon(&cfg).await {
        let owner = crate::lock::owner(&cfg.data_dir).unwrap_or_else(|| "unknown".to_owned());
        anyhow::bail!(
            "a daemon is serving {socket} (pid {owner}); reindex needs the store to itself. \
             End the agmem sessions — the daemon exits after AGMEM_IDLE_TIMEOUT — or stop pid \
             {owner}; the next session restarts it and finishes any rows this pass leaves"
        );
    }

    // Same rule as serving: an embedded engine is single-writer, a remote one
    // is the DB server's problem.
    let lock = if cfg.db_is_remote() {
        None
    } else {
        Some(crate::lock::acquire(&cfg.data_dir)?)
    };

    eprintln!("agmem reindex");
    let db = agmem_store::db::connect_with(&cfg.db_url, cfg.db_credentials()).await?;
    let schema = migrate::ensure(&db).await?;
    eprintln!("  ok    schema               v{schema}");

    // Loading the model is where a first run downloads it; do it before
    // anything is cleared, so a missing model costs nothing.
    let embedder = crate::embedder::build(&cfg)?;
    eprintln!(
        "  ok    embedder             {} ({}d, {}{})",
        embedder.model_id(),
        embedder.dim(),
        embedder.accelerator(),
        embedder
            .revision()
            .map(|rev| format!(", rev {}", agmem_store::error::short_revision(rev)))
            .unwrap_or_default()
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

/// The socket of a daemon that answers, if there is one.
#[cfg(unix)]
async fn live_daemon(cfg: &Config) -> Option<String> {
    let path = crate::daemon::socket_path(&cfg.data_dir).ok()?;
    tokio::net::UnixStream::connect(&path).await.ok()?;
    Some(path.display().to_string())
}

/// The offline pass itself, against an open store and a loaded backend.
///
/// Separate from [`run`] because this is the part worth testing: a stub
/// embedder of another width makes the whole migration runnable without a
/// model download.
///
/// Three phases, in this order for reasons the store's `queries::reindex`
/// documents: clear every vector and redefine both HNSW indexes at the new
/// width, record the new space in `meta`, then embed until nothing is
/// pending. The space is written *before* the loop on purpose — the rows
/// without vectors are the only record that the loop is unfinished, so a run
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
    let revision = embedder.revision();

    let stored = migrate::stored_embedder(db).await?;
    let moved = !stored.is_some_and(|stored| stored.matches(&model, dim, revision));
    if moved {
        repo::reindex::reset_vectors(db, dim).await?;
        migrate::set_embedder(db, &model, dim, revision).await?;
    }

    let total = repo::reindex::pending_count(db).await?;
    let mut remaining = total;
    let mut embedded = 0usize;
    while remaining > 0 {
        let left = embed_batch(db, &embedder, BATCH).await?;
        // The pending count is the progress *and* the check: an `UPDATE` over
        // a row that has gone is a silent no-op, so a batch that changed
        // nothing would otherwise come back forever.
        anyhow::ensure!(
            left < remaining,
            "a batch left {left} of {remaining} row(s) still pending; refusing to loop"
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

/// Give the next `limit` pending rows their vectors; returns how many rows
/// are still pending afterwards.
///
/// # Errors
/// When no row comes back for a nonzero count, when the backend answers
/// with the wrong number of vectors, or when the engine rejects a statement.
async fn embed_batch(db: &Db, embedder: &Arc<dyn Embedder>, limit: usize) -> anyhow::Result<usize> {
    let batch = repo::reindex::pending(db, limit).await?;
    if batch.is_empty() {
        return Ok(0);
    }
    let texts: Vec<String> = batch.iter().map(|row| row.text().to_owned()).collect();
    let vectors = agmem_embed::embed_passages(Arc::clone(embedder), texts).await?;
    repo::reindex::write_vectors(db, batch, vectors).await?;
    Ok(repo::reindex::pending_count(db).await?)
}

/// Re-embed every pending row in the background, [`DRAIN_BATCH`] at a time,
/// reporting progress through `vectors` until nothing is pending.
///
/// Spawned by the process that holds the store — the daemon, or the
/// `--no-daemon` session — right after [`crate::startup::open`] found rows
/// to do. Yields between batches so the sessions it shares a runtime with
/// keep answering; a failed batch is retried after [`DRAIN_RETRY`], and
/// [`DRAIN_GIVE_UP`] failures in a row stop the drain with the count left
/// where it is, so the notice stays up and `agmem reindex` can finish it.
///
/// Rows written while the drain runs already carry a vector and are never
/// touched; rows another writer removes mid-batch are a silent no-op the
/// recount absorbs.
pub async fn drain(db: Db, embedder: Arc<dyn Embedder>, vectors: VectorState) {
    let mut failures = 0u32;
    let mut previous = vectors.pending();
    loop {
        match embed_batch(&db, &embedder, DRAIN_BATCH).await {
            Ok(left) if left < previous || left == 0 => {
                failures = 0;
                previous = left;
                vectors.set_pending(left);
                if left == 0 {
                    tracing::info!(model = embedder.model_id(), "every row carries a vector");
                    return;
                }
                tracing::debug!(pending = left, "re-embedding in the background");
                tokio::task::yield_now().await;
            }
            Ok(left) => {
                failures += 1;
                tracing::warn!(
                    pending = left,
                    failures,
                    "a re-embed batch changed nothing; retrying"
                );
                tokio::time::sleep(DRAIN_RETRY).await;
            }
            Err(error) => {
                failures += 1;
                tracing::warn!(%error, failures, "a re-embed batch failed; retrying");
                tokio::time::sleep(DRAIN_RETRY).await;
            }
        }
        if failures >= DRAIN_GIVE_UP {
            tracing::error!(
                pending = vectors.pending(),
                "re-embedding gave up after {DRAIN_GIVE_UP} failed batches; run `agmem reindex` \
                 once the cause is fixed"
            );
            return;
        }
    }
}
