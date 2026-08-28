//! Migration gate against the real (in-memory) engine.

use agmem_store::{db, migrate};

#[tokio::test]
async fn fresh_store_migrates_then_reruns_as_noop() {
    let conn = db::connect("mem://").await.expect("connect mem://");

    let v = migrate::ensure(&conn).await.expect("first ensure");
    assert_eq!(v, migrate::SCHEMA_VERSION);
    assert_eq!(migrate::current_version(&conn).await.unwrap(), v);

    let again = migrate::ensure(&conn).await.expect("second ensure");
    assert_eq!(again, v, "re-run must be a no-op");
}
