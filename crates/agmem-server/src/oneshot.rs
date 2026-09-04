//! `agmem context` — the context block as a one-shot print (issue #46).
//!
//! The MCP `context` tool needs a live session, which a shell hook does not
//! have: hand-rolling the JSON-RPC over the server's stdin means faking the
//! `initialize` handshake and holding stdin open for the reply. This is that
//! exchange done properly, once, from inside the binary — so a SessionStart
//! hook can *inject* the briefing instead of nudging the model to ask for it.
//!
//! Two routes to the same block, chosen exactly like a serving run chooses:
//!
//! - Where a session would share the daemon, so does this — attaching to a
//!   running one, or starting one. Never the store directly: a hook fires at
//!   the same moment the session's own agmem comes up, and a second writer
//!   grabbing the single-writer lock for even a moment could be the reason
//!   that daemon fails to start. Attaching also leaves a warm daemon behind
//!   for the session about to want one.
//! - `--no-daemon`, remote engines, `mem://`, and non-Unix platforms open the
//!   store here, the way `main::in_process` does, and call the tool directly.

use std::sync::Arc;

use anyhow::bail;
use rmcp::model::{CallToolResult, ContentBlock};

use crate::config::{Config, ContextArgs};
use crate::service::AgmemService;
use crate::tools::context::{self, ContextParams};
use crate::{embedder, lock};

/// Print the context block for `cfg` and exit — the whole subcommand.
///
/// stdout is the answer here, not the MCP wire: one-shot mode is the single
/// place outside the transport where the crate-wide deny is lifted.
///
/// # Errors
/// When no route to the store works, or the tool refuses the parameters.
#[allow(clippy::print_stdout)]
pub async fn context(cfg: Config, args: ContextArgs) -> anyhow::Result<()> {
    let block = fetch(&cfg, args).await?;
    println!("{block}");
    Ok(())
}

/// The assembled block, by whichever route this configuration serves.
///
/// # Errors
/// When the daemon cannot be reached or started, the store will not open, or
/// the tool refuses the parameters.
pub async fn fetch(cfg: &Config, args: ContextArgs) -> anyhow::Result<String> {
    #[cfg(unix)]
    if crate::daemon::wanted(cfg) {
        return through_daemon(cfg, args).await;
    }
    direct(cfg, args).await
}

/// A live MCP session on the shared daemon, attached or freshly started.
///
/// # Errors
/// When no daemon can be reached or started, or the handshake fails.
#[cfg(unix)]
pub(crate) async fn daemon_session(
    cfg: &Config,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
    use anyhow::Context as _;
    use rmcp::ServiceExt as _;

    let (read, write) = crate::daemon::client::attach(cfg).await?.into_split();
    ().serve((read, write))
        .await
        .context("initializing MCP with the shared store")
}

/// The store, opened in this process the way `main::in_process` opens it —
/// lock, open, migrate, embedder check, prune, register — so a one-shot
/// sees exactly the state a served session would.
///
/// The lock rides along: dropping the pair releases it, and a caller that
/// wants the store for longer keeps the pair for longer.
///
/// # Errors
/// When the lock is held elsewhere, the store will not open or migrate, or
/// the embedder disagrees with what the store was written with.
pub(crate) async fn open_direct(
    cfg: &Config,
) -> anyhow::Result<(AgmemService, Option<lock::DataDirLock>)> {
    let lock = if cfg.db_is_remote() {
        None
    } else {
        Some(lock::acquire(&cfg.data_dir)?)
    };
    let db = agmem_store::db::connect_with(&cfg.db_url, cfg.db_credentials()).await?;
    agmem_store::migrate::ensure(&db).await?;
    let embedder = embedder::build(cfg)?;
    agmem_store::migrate::ensure_embedder(&db, embedder.model_id(), embedder.dim()).await?;
    let pruned = crate::startup::prune(&db).await;
    agmem_store::repo::ensure_space(&db, &cfg.space).await?;
    tracing::debug!(pruned, "store opened for a one-shot");
    Ok((AgmemService::new(db, embedder, Arc::new(cfg.clone())), lock))
}

/// Ask a shared daemon, as a real MCP client on the session socket.
#[cfg(unix)]
async fn through_daemon(cfg: &Config, args: ContextArgs) -> anyhow::Result<String> {
    use rmcp::model::CallToolRequestParams;

    let session = daemon_session(cfg).await?;

    let ContextArgs {
        query,
        space,
        budget_chars,
    } = args;
    let mut arguments = serde_json::Map::new();
    if let Some(query) = query {
        arguments.insert("query".to_owned(), query.into());
    }
    if let Some(space) = space {
        arguments.insert("space".to_owned(), space.into());
    }
    if let Some(budget) = budget_chars {
        arguments.insert("budget_chars".to_owned(), budget.into());
    }

    let result = session
        .call_tool(CallToolRequestParams::new("context").with_arguments(arguments))
        .await
        .map_err(|error| anyhow::anyhow!("the context tool refused: {error}"))?;
    // Detach politely so the daemon logs a session that ended, not one that
    // broke; the block is already in hand either way.
    let _ = session.cancel().await;
    markdown(&result)
}

/// Open the store in this process and call the tool with no wire at all.
async fn direct(cfg: &Config, args: ContextArgs) -> anyhow::Result<String> {
    let (service, lock) = open_direct(cfg).await?;
    let params = ContextParams {
        query: args.query,
        space: args.space,
        budget_chars: args.budget_chars,
    };
    let result = context::run(&service, params)
        .await
        .map_err(|error| anyhow::anyhow!("the context tool refused: {error}"))?;
    drop(lock);
    markdown(&result)
}

/// The block out of the tool's answer.
///
/// `context` answers with one text block and nothing else (`service.rs` says
/// why), so anything different here is a changed contract, not a case.
fn markdown(result: &CallToolResult) -> anyhow::Result<String> {
    let text: Vec<&str> = result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect();
    if result.is_error == Some(true) {
        bail!(
            "the context tool answered with an error: {}",
            text.join("\n")
        );
    }
    if text.is_empty() {
        bail!("the context tool answered with no text; agmem versions disagree?");
    }
    Ok(text.join("\n"))
}
