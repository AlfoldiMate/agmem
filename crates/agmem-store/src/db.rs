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
