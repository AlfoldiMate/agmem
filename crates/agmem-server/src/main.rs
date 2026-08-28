//! agmem — agent memory over MCP.
//!
//! stdout is the MCP wire: nothing in this binary may write to stdout except
//! the protocol transport (enforced by `clippy::print_stdout = deny`).

fn main() -> anyhow::Result<()> {
    // Startup sequence lands with the config/telemetry, lockfile, and DB
    // issues (docs/design.md §5.1). The scaffold only proves the workspace.
    Ok(())
}
