//! agmem — agent memory over MCP.
//!
//! stdout is the MCP wire: nothing in this binary may write to stdout except
//! the protocol transport (enforced by `clippy::print_stdout = deny`).
//!
//! Three ways to run, decided here and nowhere else:
//!
//! - `--daemon-serve` — be the process that owns the store (issue #37).
//! - the default on Unix — attach to that daemon, starting it if needed, and
//!   pump stdio into it, so several sessions share one embedded store.
//! - `--no-daemon`, a remote `--db`, or a non-Unix platform — open the store
//!   in this process, which is what agmem always did.

use std::sync::Arc;

#[cfg(unix)]
use agmem_server::daemon;
use agmem_server::service::{self, AgmemService};
use agmem_server::{config, doctor, embedder, lock, telemetry};
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = config::Cli::parse().resolve()?;
    telemetry::init(&cfg.log, cfg.log_file.as_deref())?;
    tracing::info!(space = %cfg.space, db = %cfg.db_url, "agmem starting");

    #[cfg(unix)]
    if cfg.daemon_serve {
        return daemon::serve::run(cfg).await;
    }

    if cfg.doctor {
        return doctor::run(&cfg).await;
    }

    // A failure on the shared path exits non-zero rather than falling back to
    // opening the store: the fallback is a second writer on a single-writer
    // store, which is the bug this replaced.
    #[cfg(unix)]
    if daemon::wanted(&cfg) {
        return daemon::client::run(&cfg).await;
    }

    in_process(cfg).await
}

/// Own the store and serve one session over stdio — agmem's original shape,
/// still what runs behind `--no-daemon` and behind a remote engine.
async fn in_process(cfg: config::Config) -> anyhow::Result<()> {
    // Embedded engines require the single-writer lock for the whole process
    // lifetime (design §5.1 step 3); remote engines skip it.
    let lock = if cfg.db_is_remote() {
        None
    } else {
        Some(lock::acquire(&cfg.data_dir)?)
    };

    let db = agmem_store::db::connect(&cfg.db_url).await?;
    let schema = agmem_store::migrate::ensure(&db).await?;
    let embedder = embedder::build(&cfg)?;
    agmem_store::migrate::ensure_embedder(&db, embedder.model_id(), embedder.dim()).await?;
    agmem_store::repo::ensure_space(&db, &cfg.space).await?;
    tracing::info!(
        schema,
        embedder = embedder.model_id(),
        dim = embedder.dim(),
        "store ready"
    );

    // From here stdout belongs to the transport (design §5.1 step 8). The
    // lock is held until this returns, which is what keeps a second agmem off
    // an embedded store for as long as this one is serving.
    service::serve_stdio(AgmemService::new(db, embedder, Arc::new(cfg))).await?;
    drop(lock);
    Ok(())
}
