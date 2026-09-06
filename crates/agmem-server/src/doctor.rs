//! `--doctor`: prove an installation works. One check per line, report on
//! stderr (stdout stays reserved for the MCP wire even here), exit 0/1.
//!
//! What it can check depends on who owns the store. With a shared daemon
//! running, the checks that need the database are the daemon's — opening
//! surrealkv from here would be the second writer this whole design exists to
//! prevent — so they are reported as skipped rather than faked.

use std::path::Path;

use agmem_embed::Embedder;
use agmem_store::db::Db;

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
    // from the outside: the surface still lists seven tools, and the only way
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
            eprintln!(
                "  skip  database + schema     the daemon checked these when it started; \
                 its log has the result"
            );
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
            match document_counts(&db).await {
                Ok(detail) => eprintln!("  ok    documents            {detail}"),
                Err(err) => {
                    failures += 1;
                    eprintln!("  FAIL  documents            {err}");
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
            eprintln!("  ok    embedder             none (test-only, no vectors)");
        }
        Ok(embedder) => {
            eprintln!(
                "  ok    embedder             {}",
                describe(embedder.as_ref())
            );
            if let Some(db) = &opened {
                match embedder_vs_store(db, embedder.as_ref()).await {
                    Ok(detail) => eprintln!("  ok    embedder vs store    {detail}"),
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

/// `<id> (<dim>d, <accelerator>, rev <short>)` — the embedder line.
fn describe(embedder: &dyn Embedder) -> String {
    format!(
        "{} ({}d, {}{})",
        embedder.model_id(),
        embedder.dim(),
        embedder.accelerator(),
        embedder
            .revision()
            .map(|rev| format!(", rev {}", agmem_store::error::short_revision(rev)))
            .unwrap_or_default()
    )
}

/// Whether the store's recorded vector space is the configured model's, and
/// what the next start will do about it if not (issue #138).
///
/// Never a failure on its own: the configured model wins, so a store that
/// holds another model's vectors is not broken, it is scheduled — the next
/// start moves it and re-embeds in the background. Only the engine refusing
/// to answer is a FAIL.
async fn embedder_vs_store(db: &Db, embedder: &dyn Embedder) -> Result<String, String> {
    let stored = agmem_store::migrate::stored_embedder(db)
        .await
        .map_err(|err| err.to_string())?;
    Ok(match stored {
        None => format!(
            "nothing recorded yet; the first start records {}",
            embedder.model_id()
        ),
        Some(stored)
            if stored.matches(embedder.model_id(), embedder.dim(), embedder.revision()) =>
        {
            "agrees".to_owned()
        }
        Some(stored) => format!(
            "store holds {} ({}d{}); the next start moves it to {} ({}d) and re-embeds every \
             row in the background — or run `agmem reindex` now",
            stored.model,
            stored.dim,
            stored
                .revision
                .as_deref()
                .map(|rev| format!(", rev {}", agmem_store::error::short_revision(rev)))
                .unwrap_or_default(),
            embedder.model_id(),
            embedder.dim()
        ),
    })
}

/// Rows the vector half of retrieval cannot reach.
///
/// A re-embed interrupted between its reset and the end of its loop leaves
/// exactly this: rows with no vector, under a `meta` that already names the
/// new model — so the guard above is satisfied and a vector recall silently
/// misses them. Rows written in BM25-only mode look identical. Neither is a
/// failure since #138: the next start finishes them in the background, and
/// every tool result says so until then. The line still names the count,
/// because it is the one number that says whether recall is whole.
async fn check_vector_coverage(db: &Db) -> u32 {
    match vector_coverage(db).await {
        Ok(detail) => {
            eprintln!("  ok    vector coverage      {detail}");
            0
        }
        Err(err) => {
            eprintln!("  FAIL  vector coverage      {err}");
            1
        }
    }
}

/// The coverage check's verdict, worded for whoever reports it.
async fn vector_coverage(db: &Db) -> Result<String, String> {
    match agmem_store::repo::reindex::pending_count(db).await {
        Ok(0) => Ok("every row carries a vector".to_owned()),
        Ok(pending) => Ok(format!(
            "{pending} row(s) still to embed; the next start finishes them in the background, \
             or run `agmem reindex` now"
        )),
        Err(err) => Err(err.to_string()),
    }
}

/// How many documents each registered space holds (#135): the named, typed
/// episodes `agmem doc put` writes and the `@` picker lists. A count, not a
/// check — it cannot fail short of the store failing — but it is the one
/// number that says whether the document tier is in use at all.
async fn document_counts(db: &Db) -> Result<String, String> {
    let spaces = agmem_store::repo::spaces(db)
        .await
        .map_err(|err| err.to_string())?;
    let mut parts = Vec::with_capacity(spaces.len());
    for space in &spaces {
        let stats = agmem_store::repo::stats(db, space)
            .await
            .map_err(|err| err.to_string())?;
        parts.push(format!("{space}: {}", stats.documents));
    }
    if parts.is_empty() {
        return Ok("no space registered yet".to_owned());
    }
    Ok(parts.join(", "))
}

/// One check over an open store, as the daemon logs it at start (issue #112)
/// and `--doctor` prints it.
#[derive(Debug)]
pub struct Check {
    /// What was checked, in the words the doctor report uses.
    pub name: &'static str,
    /// What was found: a detail worth a log line, or why it failed.
    pub outcome: Result<String, String>,
}

/// The store checks that need the open handles: a scratch write and read,
/// and — when there is a vector side — that every row carries one.
///
/// The daemon runs this over the `Db` it already holds, because a second
/// connection to an embedded store from `--doctor` would be the second
/// writer the daemon exists to prevent; until #112 those checks were simply
/// skipped while a daemon was up. Nothing here decides what a failure means
/// — the daemon logs and serves anyway, the doctor counts it.
pub async fn selfcheck(db: &Db, embedder: &dyn Embedder) -> Vec<Check> {
    let mut checks = vec![Check {
        name: "write/read roundtrip",
        outcome: roundtrip(db)
            .await
            .map(|()| "scratch record created and removed".to_owned())
            .map_err(|err| err.to_string()),
    }];
    if embedder.dim() > 0 {
        checks.push(Check {
            name: "vector coverage",
            outcome: vector_coverage(db).await,
        });
    }
    checks.push(Check {
        name: "documents",
        outcome: document_counts(db).await,
    });
    checks
}

/// The embedder check on its own, for when the store belongs to the daemon.
///
/// Loading a model is process-local, so this one still means something: it is
/// where a fresh install downloads it, and where a broken ONNX runtime shows
/// up before first use.
fn embedder_only(cfg: &Config) -> u32 {
    match crate::embedder::build(cfg) {
        Ok(embedder) if embedder.dim() == 0 => {
            eprintln!("  ok    embedder             none (test-only, no vectors)");
            0
        }
        Ok(embedder) => {
            eprintln!(
                "  ok    embedder             {}",
                describe(embedder.as_ref())
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

/// Write a scratch record, read it back, remove it.
///
/// `UPSERT`, not `CREATE`, because of the run before this one: a daemon
/// killed between the write and the `DELETE` leaves the probe row behind,
/// and a `CREATE` would then fail on "already exists" at every later start
/// — a check that can only ever fail again is not a check. (A sweep ahead
/// of the write would do the same, but `DELETE` on a table nothing has
/// created yet is an error on a fresh store.)
async fn roundtrip(db: &Db) -> Result<(), agmem_store::StoreError> {
    db.query(
        "UPSERT doctor_probe:check SET ok = true;
         SELECT VALUE ok FROM doctor_probe:check;
         DELETE doctor_probe;",
    )
    .await?
    .check()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_probe_row_left_behind_does_not_fail_the_next_roundtrip() {
        let db = agmem_store::db::connect_with("mem://", None)
            .await
            .expect("an in-memory store");
        agmem_store::migrate::ensure(&db).await.expect("migrate");
        db.query("CREATE doctor_probe:check SET ok = false;")
            .await
            .expect("what an interrupted run leaves")
            .check()
            .expect("created");

        roundtrip(&db)
            .await
            .expect("the roundtrip sweeps what an interrupted run left behind");

        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = {
            use clap::Parser as _;
            crate::config::Cli::try_parse_from([
                "agmem",
                "--embedder",
                "none",
                "--db",
                "mem://",
                "--data",
                &dir.path().display().to_string(),
            ])
            .expect("parse")
            .resolve()
            .expect("resolve")
        };
        let embedder = crate::embedder::build(&cfg).expect("the none embedder");
        let checks = selfcheck(&db, embedder.as_ref()).await;
        assert!(
            checks.iter().all(|check| check.outcome.is_ok()),
            "{checks:?}"
        );
        assert_eq!(
            checks.len(),
            2,
            "with no vector side there is no coverage to check: {checks:?}"
        );
        assert_eq!(
            checks[1].outcome.as_deref(),
            Ok("no space registered yet"),
            "a store nothing registered in has nothing to count: {checks:?}"
        );
    }
}
