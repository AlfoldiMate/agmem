//! `--doctor`: prove an installation works. One check per line, report on
//! stderr (stdout stays reserved for the MCP wire even here), exit 0/1.
//!
//! What it can check depends on who owns the store. With a shared daemon
//! running, the checks that need the database are the daemon's — opening
//! surrealkv from here would be the second writer this whole design exists to
//! prevent — so they are reported as skipped rather than faked.

use std::path::Path;

use crate::config::Config;

/// Run all checks; error (→ exit 1) when any failed.
///
/// # Errors
/// When any check failed, so the process exits non-zero.
pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut failures = 0u32;
    eprintln!("agmem doctor");

    match check_data_dir(&cfg.data_dir) {
        Ok(()) => eprintln!("  ok    data dir writable    {}", cfg.data_dir.display()),
        Err(err) => {
            failures += 1;
            eprintln!("  FAIL  data dir             {err:#}");
        }
    }

    // Not a check — nothing here can fail, `Config` refused an unknown tool
    // long before this. It is on the report because an override is invisible
    // from the outside: the surface still lists five tools, and the only way
    // to see which words an agent is being handed is to ask.
    if cfg.tool_desc.is_empty() {
        eprintln!("  ok    tool descriptions    agmem's own wording");
    } else {
        eprintln!(
            "  ok    tool descriptions    overridden: {}",
            cfg.tool_desc.tools().collect::<Vec<_>>().join(", ")
        );
    }

    #[cfg(unix)]
    if crate::daemon::wanted(cfg) {
        if let Some(socket) = live_daemon(cfg).await {
            eprintln!("  ok    shared daemon        serving {socket}");
            eprintln!("  skip  single-writer lock    the daemon holds it");
            eprintln!("  skip  database + schema     the daemon checked these when it started");
            failures += embedder_only(cfg);
            return finish(failures);
        }
        eprintln!("  ok    shared daemon        not running; the next session starts one");
    }

    if cfg.db_is_remote() {
        eprintln!("  skip  single-writer lock    remote engine, DB server is the boundary");
    } else {
        match crate::lock::acquire(&cfg.data_dir) {
            Ok(_held) => eprintln!("  ok    single-writer lock    held by this process"),
            Err(err) => {
                failures += 1;
                eprintln!("  FAIL  single-writer lock    {err:#}");
                // Everything below needs the store, and taking it while
                // somebody else holds the lock is the failure being reported.
                return finish(failures);
            }
        }
    }

    failures += check_store(cfg).await;
    finish(failures)
}

/// The database half of the report: open, migrate, write and read back.
async fn check_store(cfg: &Config) -> u32 {
    let mut failures = 0u32;
    let opened = match agmem_store::db::connect_with(&cfg.db_url, cfg.db_credentials()).await {
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
            Some(db)
        }
        Err(err) => {
            failures += 1;
            eprintln!("  FAIL  database open        {}: {err}", cfg.db_url);
            None
        }
    };

    // Loading the model is the check: on a fresh install this is where the
    // download happens, and a missing one should fail here, not at first use.
    match crate::embedder::build(cfg) {
        Ok(embedder) if embedder.dim() == 0 => {
            eprintln!("  ok    embedder             none (BM25-only mode)");
        }
        Ok(embedder) => {
            eprintln!(
                "  ok    embedder             {} ({}d)",
                embedder.model_id(),
                embedder.dim()
            );
            if let Some(db) = &opened {
                match agmem_store::migrate::ensure_embedder(db, embedder.model_id(), embedder.dim())
                    .await
                {
                    Ok(()) => eprintln!("  ok    embedder vs store    same model and width"),
                    Err(err) => {
                        failures += 1;
                        eprintln!("  FAIL  embedder vs store    {err}");
                    }
                }
                failures += check_vector_coverage(db).await;
            }
        }
        Err(err) => {
            failures += 1;
            eprintln!("  FAIL  embedder             {err:#}");
        }
    }
    failures
}

/// Rows the vector half of retrieval cannot reach.
///
/// A `--reindex` killed between its reset and the end of its embed loop
/// leaves exactly this: rows with no vector, under a `meta` that already
/// names the new model — so the guard above is satisfied and a vector recall
/// silently misses them. Nothing else notices, which is why this is a check
/// and not a log line. Rows written in BM25-only mode look identical and have
/// the same remedy, so the message names it rather than guessing which
/// happened.
async fn check_vector_coverage(db: &agmem_store::db::Db) -> u32 {
    match agmem_store::repo::reindex::pending_count(db).await {
        Ok(0) => {
            eprintln!("  ok    vector coverage      every row carries a vector");
            0
        }
        Ok(pending) => {
            eprintln!(
                "  FAIL  vector coverage      {pending} row(s) carry no vector, so a vector \
                 recall cannot reach them; run `agmem --reindex`"
            );
            1
        }
        Err(err) => {
            eprintln!("  FAIL  vector coverage      {err}");
            1
        }
    }
}

/// The embedder check on its own, for when the store belongs to the daemon.
///
/// Loading a model is process-local, so this one still means something: it is
/// where a fresh install downloads it, and where a broken ONNX runtime shows
/// up before first use.
fn embedder_only(cfg: &Config) -> u32 {
    match crate::embedder::build(cfg) {
        Ok(embedder) if embedder.dim() == 0 => {
            eprintln!("  ok    embedder             none (BM25-only mode)");
            0
        }
        Ok(embedder) => {
            eprintln!(
                "  ok    embedder             {} ({}d)",
                embedder.model_id(),
                embedder.dim()
            );
            0
        }
        Err(err) => {
            eprintln!("  FAIL  embedder             {err:#}");
            1
        }
    }
}

/// The socket of a daemon that answers, if there is one.
///
/// Connecting and leaving is the whole probe; the daemon treats a connection
/// that says nothing as exactly that.
#[cfg(unix)]
async fn live_daemon(cfg: &Config) -> Option<String> {
    let path = crate::daemon::socket_path(&cfg.data_dir).ok()?;
    tokio::net::UnixStream::connect(&path).await.ok()?;
    Some(path.display().to_string())
}

fn finish(failures: u32) -> anyhow::Result<()> {
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
