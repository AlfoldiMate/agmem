//! Opening the store (design §5.1 steps 3–8): the one sequence every route
//! into it runs — the shared daemon, `--no-daemon`, and the one-shots.
//!
//! Lock, connect, migrate, load the embedder, settle the vector space, prune,
//! register the space. The vector-space step is the policy issue #138 asks
//! for: the configured model wins. A store recorded under another model,
//! width or pinned revision — every store from before v0.3, which holds bge
//! on ONNX Runtime under a retired id — is moved on open: vectors cleared,
//! HNSW indexes redefined at the new width, `meta` rewritten. That part is
//! sub-second. Re-embedding the rows is not, so it happens in the background
//! ([`crate::reindex::drain`]) while the store already serves, and every
//! tool result says how many rows are still to go until none are.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agmem_embed::Embedder;
use agmem_store::db::Db;
use agmem_store::migrate::{self, StoredEmbedder};
use agmem_store::{StoreError, repo};

use crate::config::Config;
use crate::lock::DataDirLock;
use crate::{embedder, lock};

/// How far the store's vectors are from the configured model, shared between
/// the drain that closes the gap and the sessions that report it.
#[derive(Debug, Clone, Default)]
pub struct VectorState {
    /// Rows still without a vector. Written by the drain after every batch;
    /// read by every tool call.
    pending: Arc<AtomicUsize>,
    /// The space the store was recorded in before this open moved it, if it
    /// did — what the log and `doctor` name as the "from".
    moved_from: Option<StoredEmbedder>,
}

impl VectorState {
    /// A state with `pending` rows to embed and nothing moved.
    #[must_use]
    pub fn with_pending(pending: usize) -> Self {
        Self {
            pending: Arc::new(AtomicUsize::new(pending)),
            moved_from: None,
        }
    }

    /// Rows a vector recall cannot reach yet.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }

    /// The drain's progress report.
    pub fn set_pending(&self, pending: usize) {
        self.pending.store(pending, Ordering::Relaxed);
    }

    /// The space this open moved the store out of, if it moved it.
    #[must_use]
    pub fn moved_from(&self) -> Option<&StoredEmbedder> {
        self.moved_from.as_ref()
    }
}

/// An open store, ready to serve.
pub struct Opened {
    pub db: Db,
    pub embedder: Arc<dyn Embedder>,
    pub vectors: VectorState,
    /// The schema version after migrating.
    pub schema: u32,
    /// Working-context memories the startup sweep closed.
    pub pruned: usize,
    /// The data-dir lock, for as long as the store is held; `None` behind a
    /// remote engine, where the DB server is the boundary.
    pub lock: Option<DataDirLock>,
}

/// Open the store the way every route does.
///
/// The embedder loads before the vector space is settled, on purpose: a
/// first run downloads the model there, and a download that fails must leave
/// an old store exactly as it was rather than cleared and waiting for a
/// model that never came.
///
/// # Errors
/// When the lock is held elsewhere, the store will not open or migrate, the
/// embedder will not load, or the engine rejects a statement.
pub async fn open(cfg: &Config) -> anyhow::Result<Opened> {
    // Embedded engines require the single-writer lock for the whole process
    // lifetime (design §5.1 step 3); remote engines skip it.
    let lock = if cfg.db_is_remote() {
        None
    } else {
        Some(lock::acquire(&cfg.data_dir)?)
    };
    let mut opened = open_locked(cfg).await?;
    opened.lock = lock;
    Ok(opened)
}

/// [`open`] for a caller that already holds the data-dir lock — the daemon,
/// which takes it before it touches the socket. `Opened::lock` is `None`.
///
/// # Errors
/// As [`open`], minus the lock.
pub async fn open_locked(cfg: &Config) -> anyhow::Result<Opened> {
    let db = agmem_store::db::connect_with(&cfg.db_url, cfg.db_credentials()).await?;
    let schema = migrate::ensure(&db).await?;
    let embedder = embedder::build(cfg)?;
    let vectors = resolve(&db, embedder.as_ref()).await?;
    let pruned = prune(&db).await;
    repo::ensure_space(&db, &cfg.space).await?;
    Ok(Opened {
        db,
        embedder,
        vectors,
        schema,
        pruned,
        lock: None,
    })
}

/// Settle the store's vector space against `embedder`: the configured model
/// wins (issue #138).
///
/// - No space recorded: record this one (a first run, or a store only ever
///   opened in BM25-only mode).
/// - The same model, width and revision: nothing to do, beyond backfilling
///   a revision the store predates.
/// - Anything else: move the store — clear every vector, redefine both HNSW
///   indexes at this width, record this space — and leave every row pending
///   for [`crate::reindex::drain`]. New writes embed with the new model at
///   once; old rows join the vector arm as the drain reaches them.
///
/// A dimensionless backend claims no space and moves nothing; whatever the
/// store recorded stays, and rows it writes simply carry no vector.
///
/// Interrupted moves need no bookkeeping: `meta` already names the new
/// model, so the next open matches and finds the pending rows by their
/// missing vectors — the invariant `agmem reindex` has always relied on.
///
/// # Errors
/// [`StoreError`] for anything the engine rejects. A mismatch is not an
/// error here; it is the case this function exists to resolve.
pub async fn resolve(db: &Db, embedder: &dyn Embedder) -> Result<VectorState, StoreError> {
    let dim = embedder.dim();
    if dim == 0 {
        return Ok(VectorState::default());
    }
    let model = embedder.model_id();
    let revision = embedder.revision();

    let moved_from = match migrate::stored_embedder(db).await? {
        Some(stored) if !stored.matches(model, dim, revision) => {
            repo::reindex::reset_vectors(db, dim).await?;
            migrate::set_embedder(db, model, dim, revision).await?;
            Some(stored)
        }
        _ => {
            migrate::ensure_embedder(db, model, dim, revision).await?;
            None
        }
    };
    let pending = repo::reindex::pending_count(db).await?;
    if let Some(from) = &moved_from {
        tracing::warn!(
            from = %from.model,
            from_dim = from.dim,
            to = model,
            to_dim = dim,
            pending,
            "moved the store to the configured model; its rows re-embed in the background"
        );
    } else if pending > 0 {
        tracing::info!(
            pending,
            "rows without a vector; re-embedding in the background"
        );
    }
    Ok(VectorState {
        pending: Arc::new(AtomicUsize::new(pending)),
        moved_from,
    })
}

/// Close working-context memories that decayed while nobody was running, and
/// report how many.
///
/// A failed sweep is logged and swallowed rather than propagated. By this
/// point the schema has migrated and the embedder has loaded, so the store can
/// answer questions; refusing to serve any memory at all because a maintenance
/// pass failed trades everything the agent needs for a little unbounded
/// growth. It is the rule `recall`'s reinforcement already follows (design
/// §5.3 step 6) — the sweep is not what the session came for.
pub async fn prune(db: &Db) -> usize {
    match repo::prune_expired(db).await {
        Ok(closed) => closed.len(),
        Err(error) => {
            tracing::warn!(%error, "the startup prune failed; serving anyway");
            0
        }
    }
}
