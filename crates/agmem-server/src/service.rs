//! The MCP service: one struct, and every tool hangs off it.
//!
//! [`AgmemService`] owns the three things a tool can need — the store, the
//! embedder, and this run's configuration — so a tool body is only the flow
//! from `docs/design.md` §5.2/§5.3 and nothing else. The tools themselves live
//! in [`crate::tools`] and are attached by `#[tool]` inside the
//! `#[tool_router]` block below; this file is the seam they land on.
//!
//! stdout is the MCP wire. [`serve_stdio`] is the only thing in agmem allowed
//! to write there, and everything else logs to stderr.

use std::sync::Arc;

use agmem_embed::Embedder;
use agmem_store::db::Db;
use rmcp::{
    ServerHandler, ServiceExt,
    model::{Implementation, ServerCapabilities, ServerInfo},
    service::ServerInitializeError,
    tool_handler, tool_router,
    transport::stdio,
};

use crate::config::Config;

/// What the agent is told agmem is for, at `initialize`.
///
/// The MCP client puts this in front of the model once per session, so it says
/// what no tool description can: that agmem is worth reaching for at all, and
/// that distillation is the caller's job (design §1 — there is no server-side
/// LLM, so the tool contracts are the extractor).
const INSTRUCTIONS: &str = "\
Persistent memory across sessions. Recall before assuming; remember what you \
learned that a future session would have to rediscover.

You do the distilling: agmem stores what you give it and never rewrites it. \
Write one atomic, self-contained claim per memory, in the third person, so it \
still makes sense with no conversation around it. When something you stored \
turns out to be wrong, correct it with `supersedes` rather than storing a \
contradiction — the old claim stays readable and dated, and only one claim \
is live at a time.";

/// The MCP service: the store, the embedder, and the run's configuration.
pub struct AgmemService {
    db: Db,
    embedder: Arc<dyn Embedder>,
    config: Arc<Config>,
}

impl AgmemService {
    /// Assemble the service from an already-migrated store.
    pub fn new(db: Db, embedder: Arc<dyn Embedder>, config: Arc<Config>) -> Self {
        Self {
            db,
            embedder,
            config,
        }
    }

    /// The store every tool reads and writes through.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The embedder behind the vector half of retrieval.
    pub fn embedder(&self) -> &Arc<dyn Embedder> {
        &self.embedder
    }

    /// This run's configuration — the current space, the pool, the `k` ceiling.
    pub fn config(&self) -> &Config {
        &self.config
    }
}

/// The tools, and only the tools.
///
/// Every `#[tool]` in this block becomes a route on the `tool_router()` the
/// macro generates, which [`ServerHandler`] below dispatches through. It is
/// empty until the tool issues land: `remember`, `recall` and `inspect` in
/// phase 1, `context` and `forget` in phase 2 (design §3.1).
#[tool_router]
impl AgmemService {}

#[tool_handler]
impl ServerHandler for AgmemService {
    fn get_info(&self) -> ServerInfo {
        // Writing `get_info` by hand replaces the one `#[tool_handler]` would
        // generate, so `enable_tools` has to be repeated here or the server
        // advertises no tool capability at all. The server info likewise:
        // rmcp's `Implementation::from_build_env()` reports *rmcp's* name and
        // version, not ours.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("agmem", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

/// Serve MCP over stdio until the client hangs up (design §5.1 step 8).
///
/// A client that closes stdin before it ever initializes is not a failure —
/// it is a session that never started, which is exactly what a probe or a
/// bare `agmem` in a terminal does. That case exits 0 and says so on stderr;
/// anything else propagates.
///
/// # Errors
/// When the initialize handshake fails for any reason other than the client
/// hanging up, or the serve task panics.
pub async fn serve_stdio(service: AgmemService) -> anyhow::Result<()> {
    let running = match service.serve(stdio()).await {
        Ok(running) => running,
        Err(ServerInitializeError::ConnectionClosed(context)) => {
            tracing::info!(%context, "stdin closed before a session began");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let reason = running.waiting().await?;
    tracing::info!(?reason, "agmem stopped");
    Ok(())
}
