//! Step 8 of the startup sequence (design §5.1): the maintenance that has to
//! happen at a start, because agmem has no scheduler to happen on.
//!
//! Both routes into the store — the shared daemon and `--no-daemon` — run it
//! once, before anything is served.

use agmem_store::db::Db;

/// Close working-context memories that decayed while nobody was running, and
/// report how many.
///
/// A failed sweep is logged and swallowed rather than propagated. By this
/// point the schema has migrated and the embedder has loaded, so the store can
/// answer questions; refusing to serve any memory at all because a maintenance
/// pass failed trades everything the agent needs for a little unbounded
/// growth. It is the rule `recall`'s reinforcement already follows (design
/// §5.3 step 5) — the sweep is not what the session came for.
pub async fn prune(db: &Db) -> usize {
    match agmem_store::repo::prune_expired(db).await {
        Ok(closed) => closed.len(),
        Err(error) => {
            tracing::warn!(%error, "the startup prune failed; serving anyway");
            0
        }
    }
}
