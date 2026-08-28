//! Versioned, idempotent schema migrations.
//!
//! `ensure` first applies [`BOOTSTRAP`] (the `meta` gate table — SurrealDB 3.x
//! errors on selecting from an undefined table, so the gate must exist before
//! the version can be read), then applies the pending `MIGRATIONS` suffix,
//! bumping `meta:main.schema_version` after each batch. Statements use
//! `IF NOT EXISTS` so a half-applied batch is safe to re-run.

use surrealdb::Surreal;
use surrealdb::engine::any::Any;

use crate::StoreError;

/// The migration gate itself; always applied, idempotent, unversioned.
const BOOTSTRAP: &str = "DEFINE TABLE IF NOT EXISTS meta SCHEMAFULL;
     DEFINE FIELD IF NOT EXISTS schema_version ON meta TYPE int;
     DEFINE FIELD IF NOT EXISTS embedder_model ON meta TYPE option<string>;
     DEFINE FIELD IF NOT EXISTS embedder_dim ON meta TYPE option<int>;
     DEFINE FIELD IF NOT EXISTS created_at ON meta TYPE datetime DEFAULT time::now();";

/// Ordered migration batches; index + 1 is the schema version they produce.
/// The full data-model DDL (design §2.2) lands with the schema issue.
const MIGRATIONS: &[&str] = &[];

/// The schema version this binary produces.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Read the applied schema version (0 = fresh store).
///
/// Requires [`ensure`] to have run at least once on this store — the gate
/// table must exist.
pub async fn current_version(db: &Surreal<Any>) -> Result<u32, StoreError> {
    let mut resp = db
        .query("SELECT VALUE schema_version FROM meta:main")
        .await?;
    let versions: Vec<u32> = resp.take(0)?;
    Ok(versions.first().copied().unwrap_or(0))
}

/// Apply the bootstrap plus any pending migrations; returns the version.
///
/// A store written by a newer agmem fails with [`StoreError::SchemaTooNew`]
/// instead of being touched.
pub async fn ensure(db: &Surreal<Any>) -> Result<u32, StoreError> {
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
