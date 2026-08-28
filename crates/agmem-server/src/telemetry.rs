//! Logging setup. stdout is the MCP wire, so logs go to stderr or a file —
//! never stdout.

use std::path::Path;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

/// Initialise the global tracing subscriber.
pub fn init(filter: &str, log_file: Option<&Path>) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(filter)
        .with_context(|| format!("invalid log filter {filter:?} (AGMEM_LOG)"))?;
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(true);
    match log_file {
        Some(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("cannot open log file {}", path.display()))?;
            builder.with_writer(std::sync::Mutex::new(file)).init();
        }
        None => builder.with_writer(std::io::stderr).init(),
    }
    Ok(())
}
