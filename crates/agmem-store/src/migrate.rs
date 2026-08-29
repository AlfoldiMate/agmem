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
     DEFINE FIELD IF NOT EXISTS created_at ON meta TYPE datetime DEFAULT time::now();";

/// Ordered migration batches; index + 1 is the schema version they produce.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/v1_schema.surql"),
    include_str!("migrations/v2_derived_from.surql"),
];

/// The schema version this binary produces.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Embedding width the HNSW indexes are defined with (design §2.2).
///
/// The dimension is baked into the index definitions, so changing embedder
/// families needs a new migration plus a re-embed, never a silent swap —
/// startup compares this against `meta:main.embedder_dim`.
pub const EMBEDDING_DIM: usize = 384;

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
/// pair into `meta`; later runs must match it.
///
/// A dimensionless backend (`--embedder none`) claims no vector space: it is
/// neither recorded nor checked, so BM25-only mode opens any store and only
/// the rows it writes lack vectors.
///
/// # Errors
/// [`StoreError::EmbedderMismatch`] when the store was embedded with another
/// model or width.
pub async fn ensure_embedder(db: &Db, model_id: &str, dim: usize) -> Result<(), StoreError> {
    if dim == 0 {
        return Ok(());
    }
    let dim = i64::try_from(dim).unwrap_or(i64::MAX);

    let mut resp = db
        .query(
            "SELECT VALUE embedder_model FROM meta:main;
             SELECT VALUE embedder_dim FROM meta:main;",
        )
        .await?
        .check()?;
    let stored_model: Option<String> = resp
        .take::<Vec<Option<String>>>(0)?
        .into_iter()
        .flatten()
        .next();
    let stored_dim: Option<i64> = resp
        .take::<Vec<Option<i64>>>(1)?
        .into_iter()
        .flatten()
        .next();

    match (stored_model, stored_dim) {
        (Some(model), Some(width)) if model != model_id || width != dim => {
            Err(StoreError::EmbedderMismatch {
                stored_model: model,
                stored_dim: width,
                configured_model: model_id.to_owned(),
                configured_dim: dim,
            })
        }
        (Some(_), Some(_)) => Ok(()),
        _ => {
            tracing::info!(model = model_id, dim, "recording store embedder");
            db.query("UPSERT meta:main SET embedder_model = $model, embedder_dim = $dim")
                .bind(("model", model_id.to_owned()))
                .bind(("dim", dim))
                .await?
                .check()?;
            Ok(())
        }
    }
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
