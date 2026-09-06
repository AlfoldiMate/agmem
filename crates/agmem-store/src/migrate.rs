//! Versioned, idempotent schema migrations.
//!
//! `ensure` first applies [`BOOTSTRAP`] (the `meta` gate table — SurrealDB 3.x
//! errors on selecting from an undefined table, so the gate must exist before
//! the version can be read), then applies the pending `MIGRATIONS` suffix,
//! bumping `meta:main.schema_version` after each batch. Statements use
//! `IF NOT EXISTS` so a half-applied batch is safe to re-run.

use crate::StoreError;
use crate::db::Db;

/// The migration gate itself; always applied, idempotent, unversioned.
const BOOTSTRAP: &str = "DEFINE TABLE IF NOT EXISTS meta SCHEMAFULL;
     DEFINE FIELD IF NOT EXISTS schema_version ON meta TYPE int;
     DEFINE FIELD IF NOT EXISTS embedder_model ON meta TYPE option<string>;
     DEFINE FIELD IF NOT EXISTS embedder_dim ON meta TYPE option<int>;
     DEFINE FIELD IF NOT EXISTS embedder_revision ON meta TYPE option<string>;
     DEFINE FIELD IF NOT EXISTS created_at ON meta TYPE datetime DEFAULT time::now();";

/// Ordered migration batches; index + 1 is the schema version they produce.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/v1_schema.surql"),
    include_str!("migrations/v2_derived_from.surql"),
    include_str!("migrations/v3_supersedes_list.surql"),
    include_str!("migrations/v4_chunk_occurred_at.surql"),
    include_str!("migrations/v5_live_dedup.surql"),
    include_str!("migrations/v6_writer.surql"),
    include_str!("migrations/v7_novelty.surql"),
    include_str!("migrations/v8_summary_kind.surql"),
    include_str!("migrations/v9_documents.surql"),
];

/// The schema version this binary produces.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Embedding width the v1 schema defines its HNSW indexes with (design §2.2).
///
/// What a store starts at, not what it is stuck at: the dimension is baked
/// into the index definitions, so changing models means rebuilding them and
/// re-embedding every row — which the server does on open when the store's
/// recorded space differs from the configured model (issue #138), and
/// `agmem reindex` does on demand. Startup compares the configured backend
/// against what `meta:main` recorded, never against this constant.
pub const EMBEDDING_DIM: usize = 384;

/// The vector space a store's rows were built in, as `meta:main` records it.
///
/// `revision` is `None` for a store written before agmem recorded one
/// (pre-v0.3) or by a backend whose weights have no such identity (test
/// doubles); [`Self::matches`] says how that reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEmbedder {
    /// `meta.embedder_model`.
    pub model: String,
    /// `meta.embedder_dim`.
    pub dim: i64,
    /// `meta.embedder_revision`.
    pub revision: Option<String>,
}

impl StoredEmbedder {
    /// Whether a backend reporting `model_id`/`dim`/`revision` embeds into
    /// this space.
    ///
    /// The id and the width must agree. The revision is compared only when
    /// both sides carry one: a store that never recorded a revision is not a
    /// mismatch — it predates the field, and the first run that knows the
    /// revision backfills it — and a backend without one claims nothing.
    #[must_use]
    pub fn matches(&self, model_id: &str, dim: usize, revision: Option<&str>) -> bool {
        self.model == model_id
            && self.dim == width(dim)
            && match (self.revision.as_deref(), revision) {
                (Some(stored), Some(configured)) => stored == configured,
                _ => true,
            }
    }
}

/// A width as `meta` stores it.
fn width(dim: usize) -> i64 {
    i64::try_from(dim).unwrap_or(i64::MAX)
}

/// Read the applied schema version (0 = fresh store).
///
/// Requires [`ensure`] to have run at least once on this store — the gate
/// table must exist.
pub async fn current_version(db: &Db) -> Result<u32, StoreError> {
    let mut resp = db
        .query("SELECT VALUE schema_version FROM meta:main")
        .await?;
    let versions: Vec<u32> = resp.take(0)?;
    Ok(versions.first().copied().unwrap_or(0))
}

/// Record the embedder this store's vectors belong to, or refuse to run.
///
/// The HNSW indexes carry one dimension and the vectors one geometry, so two
/// models in one store means silently wrong neighbours. First run writes the
/// space into `meta`; later runs must match it ([`StoredEmbedder::matches`]).
/// This is the guard; moving a store that does not match is the server's
/// decision, made in its startup with [`set_embedder`] (issue #138).
///
/// A dimensionless backend (`--embedder none`) claims no vector space: it is
/// neither recorded nor checked, so BM25-only mode opens any store and only
/// the rows it writes lack vectors.
///
/// A first run whose width differs from the baked [`EMBEDDING_DIM`] redefines
/// the HNSW indexes to its own width before recording the pair: a store with
/// no pair has never held a vector (every run that could write one records
/// its pair here first), so the indexes are empty and the switch is free.
///
/// A store that matches on id and width but never recorded a revision gets
/// this run's revision written in: it predates the field, and the vectors it
/// holds are the ones this pin produces until a later pin says otherwise.
///
/// Two first runs on one shared store — `ws://`, where no advisory lock
/// serialises processes — can both find the pair absent (issue #72). The
/// write is conditional on it still being absent, so the engine lets exactly
/// one land, and the re-read after it turns the loser into an ordinary
/// mismatch refusal instead of a silent overwrite.
///
/// # Errors
/// [`StoreError::EmbedderMismatch`] when the store was embedded with another
/// model, width or revision, or when a concurrent first run recorded its
/// pair first.
pub async fn ensure_embedder(
    db: &Db,
    model_id: &str,
    dim: usize,
    revision: Option<&str>,
) -> Result<(), StoreError> {
    if dim == 0 {
        return Ok(());
    }
    let mismatch = |stored: StoredEmbedder| StoreError::EmbedderMismatch {
        stored_model: stored.model,
        stored_dim: stored.dim,
        stored_revision: stored.revision,
        configured_model: model_id.to_owned(),
        configured_dim: width(dim),
        configured_revision: revision.map(str::to_owned),
    };

    match stored_embedder(db).await? {
        Some(stored) if !stored.matches(model_id, dim, revision) => Err(mismatch(stored)),
        Some(stored) => {
            if let (None, Some(revision)) = (&stored.revision, revision) {
                tracing::info!(
                    model = model_id,
                    revision,
                    "recording store embedder revision"
                );
                db.query(
                    "UPDATE meta:main SET embedder_revision = $revision
                     WHERE embedder_revision IS NONE",
                )
                .bind(("revision", revision.to_owned()))
                .await?
                .check()?;
            }
            Ok(())
        }
        None => {
            // No pair recorded means no run could have written a vector yet,
            // so the HNSW indexes still stand empty at the width the schema
            // baked in — adopt this backend's width now, while it costs
            // nothing.
            if dim != EMBEDDING_DIM {
                crate::repo::reindex::reset_vectors(db, dim).await?;
            }
            tracing::info!(model = model_id, dim, revision, "recording store embedder");
            // Conditional on the pair still being absent: a check in Rust and
            // an unconditional write is two statements, and another first run
            // fits between them (issue #72). One guarded statement is the
            // only write the engine applies atomically, so exactly one pair
            // lands — the re-read below is what tells a loser it lost.
            db.query(
                "UPDATE meta:main SET embedder_model = $model, embedder_dim = $dim,
                     embedder_revision = $revision
                 WHERE embedder_model IS NONE AND embedder_dim IS NONE",
            )
            .bind(("model", model_id.to_owned()))
            .bind(("dim", width(dim)))
            .bind(("revision", revision.map(str::to_owned)))
            .await?
            .check()?;
            match stored_embedder(db).await? {
                Some(stored) if stored.matches(model_id, dim, revision) => Ok(()),
                Some(stored) => Err(mismatch(stored)),
                // `ensure` ran before this, so `meta:main` exists and one of
                // the writers above matched it; an absent pair here is the
                // engine misbehaving, not a caller error.
                None => Err(StoreError::UnexpectedResponse(
                    "no embedder pair recorded after writing one",
                )),
            }
        }
    }
}

/// The vector space this store's rows were built in, if a run has ever
/// recorded one.
///
/// `None` for a store only ever opened in BM25-only mode: a dimensionless
/// backend claims no vector space, so it writes nothing here.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects.
pub async fn stored_embedder(db: &Db) -> Result<Option<StoredEmbedder>, StoreError> {
    let mut resp = db
        .query(
            "SELECT VALUE embedder_model FROM meta:main;
             SELECT VALUE embedder_dim FROM meta:main;
             SELECT VALUE embedder_revision FROM meta:main;",
        )
        .await?
        .check()?;
    let model: Option<String> = resp
        .take::<Vec<Option<String>>>(0)?
        .into_iter()
        .flatten()
        .next();
    let dim: Option<i64> = resp
        .take::<Vec<Option<i64>>>(1)?
        .into_iter()
        .flatten()
        .next();
    let revision: Option<String> = resp
        .take::<Vec<Option<String>>>(2)?
        .into_iter()
        .flatten()
        .next();
    Ok(model.zip(dim).map(|(model, dim)| StoredEmbedder {
        model,
        dim,
        revision,
    }))
}

/// Record `model_id`/`dim`/`revision` as the store's vector space, replacing
/// whatever was there.
///
/// [`ensure_embedder`] only ever writes the space when it is absent — it is a
/// guard, and a guard that overwrites what it guards is not one. Moving a
/// store is a re-embed (the server's startup on a mismatch, or
/// `agmem reindex`), and this is where it says so. Writing it *before* the
/// re-embedding loop is deliberate: the rows without vectors are the resume
/// marker, so a run interrupted halfway must not come back to a store that
/// thinks it still belongs to the old model.
///
/// # Errors
/// [`StoreError::Db`] for anything the engine rejects.
pub async fn set_embedder(
    db: &Db,
    model_id: &str,
    dim: usize,
    revision: Option<&str>,
) -> Result<(), StoreError> {
    db.query(
        "UPSERT meta:main SET embedder_model = $model, embedder_dim = $dim,
             embedder_revision = $revision",
    )
    .bind(("model", model_id.to_owned()))
    .bind(("dim", width(dim)))
    .bind(("revision", revision.map(str::to_owned)))
    .await?
    .check()?;
    Ok(())
}

/// Apply the bootstrap plus any pending migrations; returns the version.
///
/// A store written by a newer agmem fails with [`StoreError::SchemaTooNew`]
/// instead of being touched.
pub async fn ensure(db: &Db) -> Result<u32, StoreError> {
    db.query(BOOTSTRAP).await?.check()?;
    let mut version = current_version(db).await?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }
    for (index, batch) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let next = u32::try_from(index).expect("tiny list") + 1;
        tracing::info!(from = version, to = next, "applying schema migration");
        db.query(*batch).await?.check()?;
        db.query("UPSERT meta:main SET schema_version = $version")
            .bind(("version", next))
            .await?
            .check()?;
        version = next;
    }
    Ok(version)
}
