//! agmem — agent memory over MCP.
//!
//! stdout is the MCP wire: nothing in this binary may write to stdout except
//! the protocol transport (enforced by `clippy::print_stdout = deny`).

use agmem_server::{config, doctor, embedder, lock, telemetry};
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = config::Cli::parse().resolve()?;
    telemetry::init(&cfg.log, cfg.log_file.as_deref())?;
    tracing::info!(space = %cfg.space, db = %cfg.db_url, "agmem starting");

    // Embedded engines require the single-writer lock for the whole process
    // lifetime (design §5.1 step 3); remote engines skip it.
    let lock = if cfg.db_is_remote() {
        None
    } else {
        Some(lock::acquire(&cfg.data_dir)?)
    };

    if cfg.doctor {
        return doctor::run(&cfg, lock.is_some()).await;
    }

    let db = agmem_store::db::connect(&cfg.db_url).await?;
    let schema = agmem_store::migrate::ensure(&db).await?;
    let embedder = embedder::build(&cfg)?;
    agmem_store::migrate::ensure_embedder(&db, embedder.model_id(), embedder.dim()).await?;
    tracing::info!(
        schema,
        embedder = embedder.model_id(),
        dim = embedder.dim(),
        "store ready"
    );

    // The MCP serve loop lands with the rmcp skeleton issue (design §5.1).
    Ok(())
}
