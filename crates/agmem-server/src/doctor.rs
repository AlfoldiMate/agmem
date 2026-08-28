//! `--doctor`: prove an installation works. One check per line, report on
//! stderr (stdout stays reserved for the MCP wire even here), exit 0/1.

use std::path::Path;

use crate::config::{Config, EmbedderKind};

/// Run all checks; error (→ exit 1) when any failed.
pub async fn run(cfg: &Config, lock_held: bool) -> anyhow::Result<()> {
    let mut failures = 0u32;
    eprintln!("agmem doctor");

    match check_data_dir(&cfg.data_dir) {
        Ok(()) => eprintln!("  ok    data dir writable    {}", cfg.data_dir.display()),
        Err(err) => {
            failures += 1;
            eprintln!("  FAIL  data dir             {err:#}");
        }
    }

    if cfg.db_is_remote() {
        eprintln!("  skip  single-writer lock    remote engine, DB server is the boundary");
    } else if lock_held {
        eprintln!("  ok    single-writer lock    held by this process");
    } else {
        failures += 1;
        eprintln!("  FAIL  single-writer lock    not held");
    }

    match agmem_store::db::connect(&cfg.db_url).await {
        Ok(db) => {
            eprintln!("  ok    database open        {}", cfg.db_url);
            match agmem_store::migrate::ensure(&db).await {
                Ok(version) => eprintln!("  ok    schema               v{version}"),
                Err(err) => {
                    failures += 1;
                    eprintln!("  FAIL  schema               {err}");
                }
            }
            match roundtrip(&db).await {
                Ok(()) => {
                    eprintln!("  ok    write/read roundtrip scratch record created and removed")
                }
                Err(err) => {
                    failures += 1;
                    eprintln!("  FAIL  write/read roundtrip {err}");
                }
            }
        }
        Err(err) => {
            failures += 1;
            eprintln!("  FAIL  database open        {}: {err}", cfg.db_url);
        }
    }

    match cfg.embedder {
        EmbedderKind::None => eprintln!("  ok    embedder             none (BM25-only mode)"),
        other => eprintln!(
            "  skip  embedder             {other:?} configured; backend wiring lands with the embedder issue"
        ),
    }

    if failures == 0 {
        eprintln!("doctor: all checks passed");
        Ok(())
    } else {
        anyhow::bail!("doctor: {failures} check(s) failed")
    }
}

fn check_data_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".doctor-probe");
    std::fs::write(&probe, b"ok")?;
    std::fs::remove_file(&probe)?;
    Ok(())
}

async fn roundtrip(db: &agmem_store::db::Db) -> Result<(), agmem_store::StoreError> {
    db.query(
        "CREATE doctor_probe:check SET ok = true;
         SELECT VALUE ok FROM doctor_probe:check;
         DELETE doctor_probe;",
    )
    .await?
    .check()?;
    Ok(())
}
