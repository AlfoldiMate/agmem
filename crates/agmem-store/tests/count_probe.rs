//! Issue #71: `count_matching` is an unindexed scan of `memory`, paid by
//! every recall whose page fills. The issue's own gate is "measure before
//! optimising" — this probe is the measurement, kept so the number can be
//! re-taken whenever the schema or the engine moves.
//!
//! Ignored by default: it seeds tens of thousands of rows, which has no place
//! in the suite. It runs on `mem://`, so what it times is the scan's own
//! per-row cost, not disk; a real store pays at least this much.
//!
//! Run with `cargo test -p agmem-store --test count_probe -- --ignored --nocapture`.

use std::time::Instant;

use agmem_core::{Kind, SpaceName};
use agmem_store::db::Db;
use agmem_store::repo::{self, Batch, Lookup, NewMemory};
use agmem_store::{db, migrate};

fn space() -> SpaceName {
    "probe".parse().expect("valid slug")
}

/// A migrated store of `rows` live claims, no embeddings — the count's
/// predicate reads `space` and `invalid_at` only, so vectors would just make
/// seeding slow.
async fn seeded(rows: usize) -> Db {
    let db = db::connect("mem://").await.expect("connect mem://");
    migrate::ensure(&db).await.expect("migrate");
    for start in (0..rows).step_by(1_000) {
        let memories = (start..(start + 1_000).min(rows))
            .map(|n| NewMemory::new(Kind::Fact, format!("claim number {n}")))
            .collect();
        repo::insert_batch(
            &db,
            Batch {
                space: space(),
                episode: None,
                memories,
            },
        )
        .await
        .expect("seed batch");
    }
    db
}

#[tokio::test]
#[ignore = "a measurement, not a test — seeds tens of thousands of rows"]
async fn count_matching_cost_by_store_size() {
    for rows in [1_000_usize, 10_000, 50_000] {
        let db = seeded(rows).await;
        let lookup = Lookup::new(vec![space()]);
        // One warm pass, so first-query setup is not what gets measured.
        repo::count_matching(&db, &lookup)
            .await
            .expect("warm count");

        let started = Instant::now();
        let counted = repo::count_matching(&db, &lookup).await.expect("count");
        let elapsed = started.elapsed();

        assert_eq!(counted, rows as u64, "the probe count must be exact");
        println!("count_matching over {rows} rows: {elapsed:?}");
    }
}
