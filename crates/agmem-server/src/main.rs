//! agmem — agent memory over MCP.
//!
//! stdout is the MCP wire: nothing in this binary may write to stdout except
//! the protocol transport (enforced by `clippy::print_stdout = deny`).

use agmem_server::{config, lock, telemetry};
use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cfg = config::Cli::parse().resolve()?;
    telemetry::init(&cfg.log, cfg.log_file.as_deref())?;
    tracing::info!(space = %cfg.space, db = %cfg.db_url, "agmem starting");

    // Embedded engines require the single-writer lock for the whole process
    // lifetime (design §5.1 step 3); remote engines skip it.
    let _lock = if cfg.db_is_remote() {
        None
    } else {
        Some(lock::acquire(&cfg.data_dir)?)
    };

    if cfg.doctor {
        // Full checks land with the --doctor issue.
        tracing::warn!("--doctor checks are not wired yet");
        return Ok(());
    }

    // The MCP serve loop lands with the rmcp skeleton issue (design §5.1).
    Ok(())
}
