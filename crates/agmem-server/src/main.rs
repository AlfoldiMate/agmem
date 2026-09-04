//! agmem — agent memory over MCP.
//!
//! stdout is the MCP wire: nothing in this binary may write to stdout except
//! the protocol transport (enforced by `clippy::print_stdout = deny`).
//!
//! Four ways to run, decided here and nowhere else:
//!
//! - `--daemon-serve` — be the process that owns the store (issue #37).
//! - the default on Unix — attach to that daemon, starting it if needed, and
//!   pump stdio into it, so several sessions share one embedded store.
//! - `--no-daemon`, a remote `--db`, or a non-Unix platform — open the store
//!   in this process, which is what agmem always did.
//! - a subcommand (`agmem context`, issue #46; `agmem doc`, issue #135) —
//!   answer once on stdout and exit, choosing between the two shapes above
//!   the same way.

use std::sync::Arc;

#[cfg(unix)]
use agmem_server::daemon;
use agmem_server::service::{self, AgmemService};
use agmem_server::{
    config, doc, doctor, embedder, hook, lock, oneshot, reindex, startup, telemetry,
};
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

    // Before the daemon branch: reindexing rewrites every vector in the
    // store, which is the one thing that must not be handed to a process
    // already serving sessions from it.
    if cfg.reindex {
        return reindex::run(&cfg).await;
    }

    // One-shot subcommands print their answer and exit. They route through
    // the daemon the way a session would (or open the store where a session
    // would), so they never contend with a running daemon for the store.
    match cfg.command.clone() {
        Some(config::CliCommand::Context(args)) => return oneshot::context(cfg, args).await,
        Some(config::CliCommand::Hook(args)) => return hook::run(cfg, args.event).await,
        Some(config::CliCommand::Doc(args)) => return doc::run(cfg, args).await,
        None => {}
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

    let db = agmem_store::db::connect_with(&cfg.db_url, cfg.db_credentials()).await?;
    let schema = agmem_store::migrate::ensure(&db).await?;
    let embedder = embedder::build(&cfg)?;
    agmem_store::migrate::ensure_embedder(&db, embedder.model_id(), embedder.dim()).await?;
    let pruned = startup::prune(&db).await;
    agmem_store::repo::ensure_space(&db, &cfg.space).await?;
    tracing::info!(
        schema,
        embedder = embedder.model_id(),
        dim = embedder.dim(),
        pruned,
        "store ready"
    );

    // From here stdout belongs to the transport (design §5.1 step 8). The
    // lock is held until this returns, which is what keeps a second agmem off
    // an embedded store for as long as this one is serving.
    service::serve_stdio(AgmemService::new(db, embedder, Arc::new(cfg))).await?;
    drop(lock);
    Ok(())
}
