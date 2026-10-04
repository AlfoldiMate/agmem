//! Connection handling: one function, any engine.
//!
//! The connection string decides the engine (`surrealkv://<path>` embedded,
//! `mem://` for tests, `ws://host` for a shared server); repository code
//! never knows which one it runs on (design §1).

use surrealdb::Surreal;
use surrealdb::engine::any::{self, Any};
use surrealdb::opt::auth::Root;

use crate::StoreError;

/// The connection handle callers pass around; engine-agnostic.
pub type Db = Surreal<Any>;

/// SurrealDB namespace holding all agmem data.
pub const NAMESPACE: &str = "agmem";
/// SurrealDB database holding all agmem data (spaces are a field, not a DB).
pub const DATABASE: &str = "main";

/// Root signin for a remote server; embedded engines have no users to be.
#[derive(Debug, Clone, Copy)]
pub struct Credentials<'a> {
    pub user: &'a str,
    pub pass: &'a str,
}

/// What the embedded engine may hold in memory (issue #196).
///
/// Upstream sizes surrealkv from total RAM — on 16 GiB a 1 GiB memtable and
/// a 7 GiB block cache — and the WAL only rotates when the memtable fills, so
/// a daemon up for two weeks held a 721 MB WAL, replayed it into memory at
/// every open (1060 MB footprint before the first session) and grew to 3.4
/// GB. A memory store is a few thousand rows: 64 MiB of memtable is days of
/// writes, and it bounds both the WAL and its replay. The block cache only
/// spares re-reading pages the OS page cache holds anyway. The HNSW cache
/// holds the vector graph — a few thousand 768-wide vectors, about 20 MB.
pub const ENGINE_CONFIG: [(&str, &str); 3] = [
    ("surrealkv_max_memtable_size", "67108864"),
    ("surrealkv_block_cache_capacity", "67108864"),
    ("hnsw_cache_size", "67108864"),
];

/// Connect to the engine named by `url` and select the agmem namespace/db.
pub async fn connect(url: &str) -> Result<Db, StoreError> {
    connect_with(url, None).await
}

/// [`connect`], signing in first when the deployment set credentials — what
/// a remote server with authentication enabled requires before `use_ns`.
pub async fn connect_with(
    url: &str,
    credentials: Option<Credentials<'_>>,
) -> Result<Db, StoreError> {
    // Process-wide and first-set-wins, through the vendored SDK patch: every
    // embedded datastore agmem opens wants the same bounds, and a remote
    // engine ignores them.
    surrealdb::engine::local::set_datastore_config(ENGINE_CONFIG);
    let db = any::connect(url).await?;
    if let Some(Credentials { user, pass }) = credentials {
        db.signin(Root {
            username: user.to_owned(),
            password: pass.to_owned(),
        })
        .await?;
    }
    db.use_ns(NAMESPACE).use_db(DATABASE).await?;
    Ok(db)
}
