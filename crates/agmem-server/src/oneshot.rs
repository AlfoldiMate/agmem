//! `agmem context` — the context block as a one-shot print (issue #46) —
//! and, since #150, `agmem consolidate` and `agmem forget`: the maintenance
//! pair off the default MCP list, served from the shell instead. `agmem doc`
//! (`doc.rs`) rides the same `call`.
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

use agmem_core::Writer;
use anyhow::{Context as _, bail};
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Value, json};

use crate::config::{Config, ConsolidateArgs, ContextArgs, ForgetArgs, ToolGroup};
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
/// The session asks for every tool, whatever `cfg` says: a one-shot is the
/// shell door to the gated pair (#150), and `agmem forget` through a daemon
/// that was started by a core session would otherwise be refused by it.
///
/// # Errors
/// When no daemon can be reached or started, or the handshake fails.
#[cfg(unix)]
pub(crate) async fn daemon_session(
    cfg: &Config,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
    use rmcp::ServiceExt as _;

    let cfg = Config {
        tools: ToolGroup::All,
        ..cfg.clone()
    };
    let (read, write) = crate::daemon::client::attach(&cfg).await?.into_split();
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

/// Print the `consolidate` tool's answer, pretty-printed — the lists carry
/// the ids and text the caller acts on, so nothing is summarised away.
///
/// # Errors
/// When no route to the store works, or the tool refuses the space.
#[allow(clippy::print_stdout)]
pub async fn consolidate(cfg: Config, args: ConsolidateArgs) -> anyhow::Result<()> {
    let answer = call(&cfg, "consolidate", json!({ "space": args.space })).await?;
    print!("{}", pretty(&answer)?);
    Ok(())
}

/// Forget by id from the shell. A dry run prints what the ids select, as
/// JSON; a real one prints one line saying what closed or what was purged.
///
/// # Errors
/// When an id names nothing, a purge is refused because live claims cite a
/// document and `--cascade` was not given, or no route to the store works.
#[allow(clippy::print_stdout)]
pub async fn forget(cfg: Config, args: ForgetArgs) -> anyhow::Result<()> {
    let ForgetArgs {
        ids,
        purge,
        cascade,
        dry_run,
        space,
    } = args;
    let arguments = json!({
        "ids": ids,
        "space": space,
        "purge": purge,
        "cascade": cascade,
        "dry_run": dry_run,
    });
    let answer = call(&cfg, "forget", arguments).await?;
    if dry_run {
        print!("{}", pretty(&answer["matched"])?);
        return Ok(());
    }
    let names = |key: &str| -> Vec<String> {
        answer[key]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    if purge {
        let purged = names("purged");
        println!(
            "purged {count} record(s), {chunks} chunk(s): {ids}",
            count = purged.len(),
            chunks = answer["chunks_purged"].as_u64().unwrap_or(0),
            ids = purged.join(", ")
        );
    } else {
        let invalidated = names("invalidated");
        println!(
            "forgot {count} record(s): {ids}",
            count = invalidated.len(),
            ids = invalidated.join(", ")
        );
    }
    Ok(())
}

/// A tool's JSON answer, by whichever route this configuration serves —
/// what every one-shot but `context` (which wants markdown) goes through.
///
/// # Errors
/// When no route to the store works, or the tool refuses.
pub(crate) async fn call(
    cfg: &Config,
    tool: &'static str,
    arguments: Value,
) -> anyhow::Result<Value> {
    #[cfg(unix)]
    if crate::daemon::wanted(cfg) {
        return through_daemon_json(cfg, tool, arguments).await;
    }
    direct_json(cfg, tool, arguments).await
}

/// Ask a shared daemon, as a real MCP client on the session socket, for a
/// tool's structured answer.
#[cfg(unix)]
async fn through_daemon_json(
    cfg: &Config,
    tool: &'static str,
    arguments: Value,
) -> anyhow::Result<Value> {
    use rmcp::model::CallToolRequestParams;

    let session = daemon_session(cfg).await?;
    let arguments = arguments
        .as_object()
        .cloned()
        .context("tool arguments are an object")?;
    let result = session
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .map_err(|error| anyhow::anyhow!("the {tool} tool refused: {error}"))?;
    // Detach politely so the daemon logs a session that ended, not one that
    // broke; the answer is already in hand either way.
    let _ = session.cancel().await;

    let text = || {
        result
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if result.is_error == Some(true) {
        bail!("the {tool} tool answered with an error: {}", text());
    }
    if let Some(value) = result.structured_content {
        return Ok(value);
    }
    serde_json::from_str(&text())
        .context("the tool answered with no JSON; agmem versions disagree?")
}

/// Open the store in this process and call the tool with no wire at all,
/// serialising its typed answer the way the wire would.
async fn direct_json(cfg: &Config, tool: &'static str, arguments: Value) -> anyhow::Result<Value> {
    use crate::tools::{consolidate, forget, inspect, remember};

    let (service, lock) = open_direct(cfg).await?;
    let refused = |error: rmcp::ErrorData| anyhow::anyhow!("the {tool} tool refused: {error}");
    let answer = match tool {
        "remember" => {
            // Who wrote it (issue #75): no MCP handshake here, so the CLI
            // introduces itself, and the session is the one this process
            // minted for itself.
            let writer = Writer {
                client: "agmem-cli".to_owned(),
                client_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                session: service.session().to_owned(),
                tool: tool.to_owned(),
            };
            let params = serde_json::from_value(arguments)?;
            serde_json::to_value(
                remember::run(&service, params, writer)
                    .await
                    .map_err(refused)?,
            )?
        }
        "inspect" => {
            let params = serde_json::from_value(arguments)?;
            serde_json::to_value(inspect::run(&service, params).await.map_err(refused)?)?
        }
        "forget" => {
            let params = serde_json::from_value(arguments)?;
            serde_json::to_value(forget::run(&service, params).await.map_err(refused)?)?
        }
        "consolidate" => {
            let params = serde_json::from_value(arguments)?;
            serde_json::to_value(consolidate::run(&service, params).await.map_err(refused)?)?
        }
        other => bail!("no direct route for the {other} tool"),
    };
    drop(lock);
    Ok(answer)
}

/// JSON the way a shell wants it: indented, newline-terminated.
///
/// # Errors
/// When `answer` cannot be serialised, which it always can.
pub(crate) fn pretty(answer: &Value) -> anyhow::Result<String> {
    let mut text = serde_json::to_string_pretty(answer)?;
    text.push('\n');
    Ok(text)
}
