//! Connection handling: one function, any engine.
//!
//! The connection string decides the engine (`surrealkv://<path>` embedded,
//! `mem://` for tests, `ws://host` for a shared server); repository code
//! never knows which one it runs on (design §1).

use surrealdb::Surreal;
use surrealdb::engine::any::{self, Any};

use crate::StoreError;

/// The connection handle callers pass around; engine-agnostic.
pub type Db = Surreal<Any>;

/// SurrealDB namespace holding all agmem data.
pub const NAMESPACE: &str = "agmem";
/// SurrealDB database holding all agmem data (spaces are a field, not a DB).
pub const DATABASE: &str = "main";

/// Connect to the engine named by `url` and select the agmem namespace/db.
pub async fn connect(url: &str) -> Result<Db, StoreError> {
    let db = any::connect(url).await?;
    db.use_ns(NAMESPACE).use_db(DATABASE).await?;
    Ok(db)
}
