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
    ErrorData, Json, ServerHandler, ServiceExt,
    handler::server::wrapper::Parameters,
    model::{Implementation, ServerCapabilities, ServerInfo},
    service::ServerInitializeError,
    tool, tool_handler, tool_router,
    transport::stdio,
};

use crate::config::Config;
use crate::tools::recall::{self, RecallParams, RecallResult};
use crate::tools::remember::{self, RememberParams, RememberResult};

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
/// macro generates, which [`ServerHandler`] below dispatches through. Each body
/// is one call into [`crate::tools`]; the `description` beside it is not a
/// comment but the tool's text on the wire — the extraction contract the model
/// reads before deciding to call. `inspect` follows in phase 1, `context` and
/// `forget` in phase 2 (design §3.1).
#[tool_router]
impl AgmemService {
    /// The write verb (design §5.2). `description` below is the extraction
    /// contract on the wire — a doc comment would work too, but the macro
    /// joins its lines with a single `\n`, which flattens the paragraphs into
    /// one wall the model has to re-segment.
    #[tool(
        name = "remember",
        description = "Store distilled memories, and optionally the verbatim text they came \
from, so a future session does not have to rediscover them.\n\n\
Distil before you call: one atomic, self-contained claim per entry, in the third person, \
understandable with no conversation around it — \"the user prefers Rust over Python for CLI \
tools\", not \"he said he likes it better\". Store what a later session would otherwise have to \
work out again: a preference, a decision and its reason, a convention, a lesson from something \
that failed. Do not store what the code or the ticket already records, or what only matters to \
this turn.\n\n\
Nothing here is ever rewritten. When a stored claim turns out to be wrong, send the correction \
with `supersedes` set to the old id instead of storing a contradiction: the old claim stays \
readable and dated, and only one of them is live.\n\n\
Returns a diff rather than an acknowledgement — what was created, what was already stored (with \
how close a match, so you can decide between a no-op and a correction), what was closed, and the \
episode's id.",
        annotations(destructive_hint = false, idempotent_hint = true)
    )]
    async fn remember(
        &self,
        Parameters(params): Parameters<RememberParams>,
    ) -> Result<Json<RememberResult>, ErrorData> {
        remember::run(self, params).await.map(Json)
    }

    /// The read verb (design §5.3).
    #[tool(
        name = "recall",
        description = "Search everything past sessions stored — distilled claims and the verbatim \
text behind them — before assuming, guessing, or asking the user something they may already have \
said.\n\n\
Call this at the start of a session, when a new topic comes up, and before any answer that \
depends on what the user prefers, decided earlier, or has already been told. Ask in words: the \
wording is matched literally and the meaning semantically, so a question works better than \
keywords. Drop `query` entirely to list what `entities`, `tags` or `kinds` select on their own.\n\n\
Hits come back ranked by how well they matched, how well they have held up since they were last \
used, and how important the storing agent said they were — each of those is in `signals`, so a \
claim that only surfaced because it never decays is visible as such. Nothing is hidden: a claim \
that was corrected is absent unless you ask with `as_of` or `include_invalidated`, which is how \
you find out what was believed at some earlier point.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn recall(
        &self,
        Parameters(params): Parameters<RecallParams>,
    ) -> Result<Json<RecallResult>, ErrorData> {
        recall::run(self, params).await.map(Json)
    }
}

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
