//! agmem — agent memory over MCP.
//!
//! stdout is the MCP wire: nothing in this binary may write to stdout except
//! the protocol transport (enforced by `clippy::print_stdout = deny`).

mod config;
mod telemetry;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cfg = config::Cli::parse().resolve()?;
    telemetry::init(&cfg.log, cfg.log_file.as_deref())?;
    tracing::info!(space = %cfg.space, db = %cfg.db_url, "agmem starting");

    if cfg.doctor {
        // Full checks land with the --doctor issue.
        tracing::warn!("--doctor checks are not wired yet");
        return Ok(());
    }

    // The MCP serve loop lands with the rmcp skeleton issue (design §5.1).
    Ok(())
}
