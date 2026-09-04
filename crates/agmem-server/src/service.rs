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

use std::borrow::Cow;
use std::sync::Arc;

use agmem_embed::Embedder;
use agmem_store::db::Db;
use rmcp::{
    ErrorData, Json, RoleServer, ServerHandler, ServiceExt,
    handler::server::router::prompt::PromptRouter,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, GetPromptResult, Implementation, ListResourceTemplatesResult,
        ListResourcesResult, PaginatedRequestParams, PromptMessage, ReadResourceRequestParams,
        ReadResourceResponse, Role, ServerCapabilities, ServerInfo,
    },
    prompt, prompt_handler, prompt_router,
    service::{RequestContext, ServerInitializeError},
    tool, tool_handler, tool_router,
    transport::stdio,
};

use crate::config::{Config, ToolGroup};
use crate::prompts::{self, Focus};
use crate::resources;
use crate::tools::GATED;
use crate::tools::consolidate::{self, ConsolidateParams, ConsolidateResult};
use crate::tools::context::{self, ContextParams};
use crate::tools::forget::{self, ForgetParams, ForgetResult, Pending};
use crate::tools::inspect::{self, InspectParams, InspectResult};
use crate::tools::recall::{self, RecallParams, RecallResult};
use crate::tools::reflect::{self, ReflectParams, ReflectResult};
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
    /// What this session's last `forget` dry run offered. It belongs to the
    /// service rather than to the store because it is a fact about *this*
    /// conversation: the daemon builds one service per connection, so a scope
    /// one agent confirmed can never authorise another agent's delete.
    pending_forget: Pending,
    /// The session id this connection's writes are attributed to (issue #75),
    /// unless a request carries its own in `_meta`. Minted here because the
    /// daemon builds one service per connection — which is exactly the
    /// granularity a session is.
    session: String,
    /// The routes this session serves, with `config.tool_desc` already
    /// applied. Held as a field rather than rebuilt per request — which is
    /// what a bare `#[tool_handler]` does — so the surface is decided once, at
    /// the same moment as the rest of the configuration.
    tool_router: ToolRouter<Self>,
    /// The rituals (design §3.3). A field for symmetry with `tool_router`
    /// rather than necessity — `#[prompt_handler]` would rebuild it per
    /// request quite happily — so that a prompt-side override has the same
    /// seam waiting for it that `AGMEM_TOOL_DESC_<TOOL>` uses.
    prompt_router: PromptRouter<Self>,
}

impl AgmemService {
    /// Assemble the service from an already-migrated store.
    pub fn new(db: Db, embedder: Arc<dyn Embedder>, config: Arc<Config>) -> Self {
        Self {
            db,
            embedder,
            pending_forget: Pending::default(),
            // Distinct per connection and sortable by start time; not a ULID
            // only because nothing in this crate mints those.
            session: format!(
                "{}-{}",
                std::process::id(),
                jiff::Timestamp::now().as_nanosecond()
            ),
            tool_router: gated(described(Self::tool_router(), &config), &config),
            prompt_router: Self::prompt_router(),
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

    /// The gate a `forget` by query has to pass (design §5.4).
    pub(crate) fn pending_forget(&self) -> &Pending {
        &self.pending_forget
    }

    /// The session id this connection's writes fall back to (issue #75).
    pub(crate) fn session(&self) -> &str {
        &self.session
    }
}

/// Serve this deployment's wording instead of agmem's, where it has any.
///
/// Descriptions are the steering lever design §3.1 keeps for the operator:
/// whether an agent reaches for memory at all is decided by this text and
/// nothing else (§9 risk 4), and a deployment that finds better words should
/// not have to wait for a release to use them. `Config` already refused any
/// name that is not a tool, so a miss here is unreachable rather than
/// tolerated — it is logged and skipped, because an assertion in the middle of
/// startup would trade a reworded tool for no tools at all.
fn described(mut router: ToolRouter<AgmemService>, config: &Config) -> ToolRouter<AgmemService> {
    for tool in config.tool_desc.tools() {
        let Some(text) = config.tool_desc.get(tool) else {
            continue;
        };
        match router.map.get_mut(tool) {
            Some(route) => {
                route.attr.description = Some(Cow::Owned(text.to_owned()));
                tracing::info!(
                    tool,
                    chars = text.len(),
                    "serving an overridden description"
                );
            }
            None => tracing::warn!(tool, "no such route; the override was dropped"),
        }
    }
    router
}

/// Serve the default list, or everything, as `config.tools` says (#150).
///
/// Runs after [`described`] on purpose: an override names a tool from
/// [`NAMES`], which still holds the gated pair, so `AGMEM_TOOL_DESC_FORGET`
/// under `core` is accepted and applied — and then has no route to be read
/// from. Removing the route is what makes a gated tool absent from both
/// `tools/list` and `tools/call`: `#[tool_handler]` lists and dispatches from
/// the same field, so there is no second list to keep in step.
fn gated(mut router: ToolRouter<AgmemService>, config: &Config) -> ToolRouter<AgmemService> {
    if config.tools == ToolGroup::Core {
        for name in GATED {
            router.remove_route(name);
        }
        tracing::debug!(gated = ?GATED, "serving the core tool list");
    }
    router
}

/// The tools, and only the tools.
///
/// Every `#[tool]` in this block becomes a route on the `tool_router()` the
/// macro generates, which [`ServerHandler`] below dispatches through. Each body
/// is one call into [`crate::tools`]; the `description` beside it is not a
/// comment but the tool's text on the wire — the extraction contract the model
/// reads before deciding to call, and what a deployment replaces through
/// `AGMEM_TOOL_DESC_<TOOL>` (see [`described`]). They are declared in the
/// order design §3.1 tables them; `list_tools` reports them sorted by name,
/// because that is what rmcp's `ToolRouter::list_all` does.
#[tool_router]
impl AgmemService {
    /// The write verb (design §5.2). `description` below is the extraction
    /// contract on the wire — a doc comment would work too, but the macro
    /// joins its lines with a single `\n`, which flattens the paragraphs into
    /// one wall the model has to re-segment.
    #[tool(
        name = "remember",
        description = "Store what this session learned — distilled claims, optionally with the verbatim \
text they came from — so the next session starts with it instead of working it out again.\n\n\
Call it as soon as something durable is said, unprompted: a preference or a standing instruction, \
a decision and the reason behind it, a convention, a constraint, a lesson from a failure. Nothing \
you say persists — an answer that ends \"noted\" without this call is a promise the next session \
cannot keep.\n\n\
Distil before you call: one atomic, self-contained claim per entry, in the third person, \
understandable with no conversation around it. Do not store what the code or the ticket already \
records, or what only matters to this turn.\n\n\
Nothing here is rewritten. When a stored claim turns out to be wrong, send the correction with \
`supersedes` set to its id rather than storing a contradiction: the old claim stays readable and \
dated, and only one is live.\n\n\
Returns a diff, not an acknowledgement: `created`, `duplicates` (**not** written) and `related`, \
the last two with the id and text of a claim already stored. Read both before you answer: a \
correction reads much like the claim it corrects, so it lands there rather than in `created`, and \
reporting it as remembered would leave the old, wrong claim live. When one of them contradicts \
yours, send yours again with `supersedes` set to its id.",
        annotations(destructive_hint = false, idempotent_hint = true)
    )]
    async fn remember(
        &self,
        Parameters(params): Parameters<RememberParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<RememberResult>, ErrorData> {
        let writer = crate::tools::writer(self, &context, "remember");
        remember::run(self, params, writer).await.map(Json)
    }

    /// The read verb (design §5.3).
    #[tool(
        name = "recall",
        description = "Search everything past sessions stored — distilled claims and the verbatim text \
behind them — before assuming, guessing, or asking something the user may already have said.\n\n\
Call it at the start of a session, when a new topic comes up, and before any answer that depends \
on what the user prefers or decided earlier. Ask in words: the wording is matched literally and \
the meaning semantically, so a question works better than keywords. Drop `query` to list what \
`entities`, `tags` or `kinds` select on their own.\n\n\
Hits are ranked by match, by how well they have held up since last used, and by stated \
importance — each visible in `signals`. Every hit carries the `source` it was distilled from, \
which `inspect` takes as it stands. A corrected claim is absent unless you ask with `as_of` or \
`include_invalidated`.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        ),
        // Spelled out because the macro derives it only from a `Json<T>`
        // return, and this arm builds its own result to append the links.
        output_schema = rmcp::handler::server::common::schema_for_output::<RecallResult>()
    )]
    async fn recall(
        &self,
        Parameters(params): Parameters<RecallParams>,
    ) -> Result<CallToolResult, ErrorData> {
        // The `Json<T>` shape by hand — JSON text first, structured content
        // beside it — plus one `resource_link` per document the hits are
        // slices of (#135), so a client can open the source directly.
        let result = recall::run(self, params).await?;
        let links = recall::links(&result);
        let value = serde_json::to_value(&result)
            .map_err(|error| crate::tools::internal(format!("rendering recall failed: {error}")))?;
        let mut answer = CallToolResult::structured(value);
        answer.content.extend(links);
        Ok(answer)
    }

    /// The session-start verb (design §3.2).
    ///
    /// The only tool that does not answer with `Json<T>`. Its whole payload is
    /// one markdown block meant to go into the prompt as written, and `Json`
    /// puts the JSON serialisation in `content` — so every client would show
    /// the model an escaped string with `\n` in it instead of the block. There
    /// is nothing to parse here, so there is nothing an output schema would
    /// describe.
    #[tool(
        name = "context",
        description = "Read this before your first move in a session: the standing instructions, \
who the user is, what is relevant right now, and the lessons earlier sessions paid for — as one \
markdown block.\n\n\
Call it once at the start of a session, and again when the topic shifts enough that a different \
set of memories matters. Pass `query` to aim the Relevant section at what you are about to do; \
leave it out for a general orientation. The other sections do not change with it.\n\n\
The block is capped at `budget_chars` (6000 by default) and fills its sections in priority order, \
dropping whole entries rather than cutting one in half — so it is a briefing, not an inventory. \
Use `recall` for anything it left out, including the verbatim text, which never appears here.\n\n\
Every line ends with its memory id. Hand that to `inspect` to see where the claim came from, or \
to `remember`'s `supersedes` the moment you learn it is wrong.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn context(
        &self,
        Parameters(params): Parameters<ContextParams>,
    ) -> Result<CallToolResult, ErrorData> {
        context::run(self, params).await
    }

    /// The destructive verb (design §5.4).
    ///
    /// The only tool whose description spends most of its words talking the
    /// caller *out* of the call: a correction through `remember` keeps the
    /// history, and forgetting is for what should never have been written
    /// down. The two-step on `query` is enforced in the tool, not described
    /// here as etiquette.
    #[tool(
        name = "forget",
        description = "Remove memories the store should no longer act on — by id when you know \
which, or by query when you do not.\n\n\
Reach for `remember` with `supersedes` first: a claim that turned out to be wrong is a correction, \
and a correction stays readable and dated. Forget is for what should not have been stored at all — \
something private, something that only made sense inside one session, notes on a project that no \
longer exists.\n\n\
By default a forgotten memory is closed, not deleted: it stops surfacing in `recall` and \
`context`, and stays visible to `inspect` with `forgotten` as the reason, so a mistaken forget is \
recoverable and an audit still adds up. `purge: true` deletes outright — the claim, its whole \
correction history, and for an episode its verbatim text and slices. That is unrecoverable, and it \
is the only way to remove something that must not stay on disk. Purging anonymous text does not \
purge the claims distilled from it; purging a document (an episode with a title and kind) is \
refused while live claims cite it, unless `cascade: true` purges those claims with it.\n\n\
Forgetting by `query` is refused until the identical call has been made with `dry_run: true`, \
and a query matches on the words you write, not on their meaning — the opposite of `recall`, \
deliberately, so a deletion never reaches something that merely resembles what you asked for.",
        annotations(destructive_hint = true)
    )]
    async fn forget(
        &self,
        Parameters(params): Parameters<ForgetParams>,
    ) -> Result<Json<ForgetResult>, ErrorData> {
        forget::run(self, params).await.map(Json)
    }

    /// The audit verb (design §3.1).
    #[tool(
        name = "inspect",
        description = "Look behind a stored memory: where it came from, what it used to say, and \
what the store actually holds.\n\n\
Use it when a recalled claim matters enough to check, when two claims disagree, or when you need \
to quote the original wording rather than the distilled version. `ref` takes one of: a memory id \
(bare, or `memory:<id>`) for the claim, its full correction history oldest-first, and the verbatim \
text it was distilled from; `episode:<id>` for that text with every claim drawn from it; \
`entity:<name>` for everything ever said about a subject, corrected claims included; \
`doc:<space>/<title>` for the newest document under a title, with its earlier versions listed; \
`docs` or `docs:<space>` to list documents and how many claims cite each; or `stats` for per-space \
counts. A document comes back one chunk at a time — `offset` and `limit` page through it.\n\n\
Nothing is ever deleted here, so a claim that was corrected is still readable and still dated — \
which is what lets you tell a belief that changed from a belief that was always wrong.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn inspect(
        &self,
        Parameters(params): Parameters<InspectParams>,
    ) -> Result<Json<InspectResult>, ErrorData> {
        inspect::run(self, params).await.map(Json)
    }

    /// The maintenance verb (design §5.5). It surfaces candidates and stops
    /// there — the merge is a judgement, and the only LLM in this system is
    /// the one reading the answer.
    #[tool(
        name = "consolidate",
        description = "Find what stored memory needs tidying up, and get back everything needed to \
do it. This decides nothing and changes nothing.\n\n\
Call it when memory has started to feel noisy: recall keeps returning the same claim in different \
words, two stored claims look like they disagree, or you are picking up a project that has \
accumulated a lot. It is a maintenance verb, not a search one — to find a particular claim, use \
`recall`.\n\n\
Five lists come back, every claim in each with its full text so you can judge it here:\n\n\
- `near_duplicates` — groups of live claims saying the same thing; merge a group with one \
`remember` whose `supersedes` lists every other member. `min_similarity` is the weakest pair \
anywhere in the group, so a low number means it chained through a middle claim.\n\
- `contradictions` — pairs about one subject that are close without being the same; nothing here \
has judged that they disagree. Read both and decide.\n\
- `stale_contexts` — short-lived claims recall has kept alive past their class; store a durable \
one again with a slower `decay_class`, `forget` scaffolding.\n\
- `over_full_tags` — tags carrying more live lessons than a briefing shows for one; merge as a \
duplicate group merges.\n\
- `orphan_documents` — documents no live claim cites; distil and cite one, or `forget` it with \
`purge: true`.\n\n\
Every list can be empty, and usually some are — an empty answer means there is nothing worth your \
attention, not that the call failed. `scanned` says what was actually compared.\n\n\
This looks in the current space only, unlike every other read here: a tidy-up should not reach \
the shared user space unless you name it with `space`.",
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            open_world_hint = false
        )
    )]
    async fn consolidate(
        &self,
        Parameters(params): Parameters<ConsolidateParams>,
    ) -> Result<Json<ConsolidateResult>, ErrorData> {
        consolidate::run(self, params).await.map(Json)
    }

    /// The insight verb (design §3.1, issue #26). A reflection is a memory
    /// row like any other; the citations are what make it one.
    #[tool(
        name = "reflect",
        description = "Store something you worked out from what memory already holds, together with \
the ids it was drawn from.\n\n\
Call it when reading several memories together tells you something none of them says on its own: \
a pattern across failures, what a preference and a constraint mean taken together, the reason \
behind a decision. This is the verb for a conclusion you reached; `remember` is for something you \
were told, and an insight with nothing behind it is a `remember` call. The cited ids are shown by \
`inspect`, so a later session can check the evidence still stands.\n\n\
Stored as a `lesson` unless you say otherwise. `kind: \"summary\"` is a digest of the cited \
claims: `context` shows it in their place under budget pressure and `inspect` expands them, so \
compressing a finished session this way costs no detail.\n\n\
Returns the id. `created: false` means an equivalent insight was already there — read its \
`content`, and if yours differs, send it again with `supersedes` set to that id. `related` \
carries nearby live claims with their text, for the same decision.",
        annotations(destructive_hint = false, idempotent_hint = true)
    )]
    async fn reflect(
        &self,
        Parameters(params): Parameters<ReflectParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ReflectResult>, ErrorData> {
        let writer = crate::tools::writer(self, &context, "reflect");
        reflect::run(self, params, writer).await.map(Json)
    }
}

/// The rituals, and only the rituals (design §3.3).
///
/// A separate `impl` block because each router macro appends its own
/// generated fn to the block it decorates, and because the two are different
/// kinds of thing: a tool is something the model may choose to call, a prompt
/// is something a person asks for and the model then reads as its
/// instruction. #23 measured how much that difference is worth — a
/// description competing with the host's own memory lost 6 sessions out of 6,
/// and a ritual is not in that competition.
///
/// Neither body touches the store. What a ritual returns is text about which
/// tools to call in which order; running them is the agent's turn, not this
/// one, and a prompt that did the work itself would be the server-side
/// pipeline design §1 exists to not have.
#[prompt_router]
impl AgmemService {
    /// Read memory before the first move: call `context`, then work from it.
    #[prompt(
        name = "recall_first",
        title = "Recall first",
        description = "Session-start ritual: read the memory block, treat it as \
established fact, and correct it rather than working around it."
    )]
    async fn recall_first(
        &self,
        Parameters(focus): Parameters<Focus>,
    ) -> Result<GetPromptResult, ErrorData> {
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            prompts::recall_first(&focus),
        )])
        .with_description("Read memory before the first move"))
    }

    /// Distil the session and write down what outlives it.
    #[prompt(
        name = "checkpoint",
        title = "Checkpoint",
        description = "End-of-session ritual: distil what is durable, recall each \
claim before writing it, then remember the batch with supersedes on the corrections."
    )]
    async fn checkpoint(
        &self,
        Parameters(focus): Parameters<Focus>,
    ) -> Result<GetPromptResult, ErrorData> {
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            Role::User,
            prompts::checkpoint(&focus),
        )])
        .with_description("Checkpoint this session into memory"))
    }
}

// Both routers come from fields rather than from `Self::*_router()`. For tools
// that is load-bearing — the default expression rebuilds the routes on every
// request, which would serve the built-in descriptions and silently discard
// every override — and for prompts it is symmetry.
//
// `#[tool_handler]` stays first: with `get_info` hand-written neither macro
// generates one, but the order is what decides which capabilities an
// auto-generated one would carry, and a future edit that deletes `get_info`
// should degrade to "tools and prompts" rather than to "tools".
#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for AgmemService {
    fn get_info(&self) -> ServerInfo {
        // Writing `get_info` by hand replaces the one the handler macros would
        // generate, so every capability has to be repeated here or the server
        // advertises none of it — a client that is not told about prompts does
        // not ask for them, and the rituals simply never appear. The server
        // info likewise: rmcp's `Implementation::from_build_env()` reports
        // *rmcp's* name and version, not ours.
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new("agmem", env!("CARGO_PKG_VERSION")))
        .with_instructions(INSTRUCTIONS)
    }

    // The `memory://` surface (design §3.3, issue #31). Hand-written rather
    // than routed because there is no router to want: two URI forms, both
    // answered by [`crate::resources`], and a template standing in for the
    // per-record listing.

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        resources::list(self).await
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(resources::templates())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        resources::read(self, &request.uri).await.map(Into::into)
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

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;
    use crate::config::{Cli, ToolDescriptions};
    use crate::prompts;
    use crate::tools::NAMES;

    fn config(tool_desc: ToolDescriptions) -> Config {
        let mut config = Cli::try_parse_from(["agmem", "--data", "/tmp/agmem-test"])
            .expect("parse")
            .resolve()
            .expect("resolve");
        config.tool_desc = tool_desc;
        config
    }

    fn names(router: &ToolRouter<AgmemService>) -> Vec<String> {
        let mut names: Vec<String> = router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_core_list_is_every_tool_but_the_gated_pair() {
        let mut config = config(ToolDescriptions::default());
        assert_eq!(config.tools, ToolGroup::Core, "core is the default");
        let core = gated(AgmemService::tool_router(), &config);

        let mut expected: Vec<String> = NAMES
            .iter()
            .filter(|name| !GATED.contains(name))
            .map(|name| (*name).to_owned())
            .collect();
        expected.sort();
        assert_eq!(names(&core), expected);
        for name in GATED {
            assert!(
                !core.has_route(name),
                "{name} is neither listed nor callable"
            );
        }

        config.tools = ToolGroup::All;
        let all = gated(AgmemService::tool_router(), &config);
        let mut everything: Vec<String> = NAMES.iter().map(|name| (*name).to_owned()).collect();
        everything.sort();
        assert_eq!(names(&all), everything, "`all` is the whole surface");
    }

    #[test]
    fn an_override_survives_gating() {
        let config = config(ToolDescriptions::from_iter([
            ("recall", "Ask the store first."),
            ("forget", "Never read."),
        ]));
        let core = gated(described(AgmemService::tool_router(), &config), &config);
        assert_eq!(
            core.get("recall").expect("routed").description,
            Some(Cow::Borrowed("Ask the store first.")),
            "gating removes routes; it does not undo the wording"
        );
        assert!(
            core.get("forget").is_none(),
            "an override for a gated tool is accepted and then has nowhere to land"
        );
    }

    #[test]
    fn the_name_list_is_the_router() {
        let mut routed: Vec<_> = AgmemService::tool_router().list_all();
        routed.sort_by(|left, right| left.name.cmp(&right.name));
        let mut declared = NAMES;
        declared.sort_unstable();

        assert_eq!(
            routed.iter().map(|tool| &*tool.name).collect::<Vec<_>>(),
            declared,
            "tools::NAMES is what an override is validated against; a tool \
             renamed in the #[tool] attribute has to be renamed there too"
        );
    }

    #[test]
    fn the_ritual_list_is_the_prompt_router() {
        let mut routed: Vec<_> = AgmemService::prompt_router()
            .list_all()
            .into_iter()
            .map(|prompt| prompt.name)
            .collect();
        routed.sort();
        let mut declared = prompts::NAMES;
        declared.sort_unstable();

        assert_eq!(routed, declared, "prompts::NAMES is the ritual vocabulary");
    }

    #[test]
    fn every_ritual_declares_its_focus_argument() {
        for prompt in AgmemService::prompt_router().list_all() {
            let arguments = prompt.arguments.unwrap_or_default();
            let names: Vec<_> = arguments.iter().map(|arg| arg.name.as_str()).collect();
            assert_eq!(
                names,
                ["focus"],
                "{} takes exactly one optional argument, because a client \
                 renders a prompt argument as free text and a ritual that \
                 needs configuring is one nobody runs",
                prompt.name
            );
            assert_ne!(
                arguments[0].required,
                Some(true),
                "{} must run with no argument at all",
                prompt.name
            );
            assert!(
                prompt.description.is_some_and(|text| !text.is_empty()),
                "a ritual with no description is one nobody finds"
            );
        }
    }

    #[test]
    fn an_override_replaces_one_description_and_leaves_the_rest() {
        let built_in = AgmemService::tool_router();
        let overridden = described(
            AgmemService::tool_router(),
            &config(ToolDescriptions::from_iter([(
                "recall",
                "Ask the store first.",
            )])),
        );

        assert_eq!(
            overridden
                .get("recall")
                .expect("recall is routed")
                .description,
            Some(Cow::Borrowed("Ask the store first.")),
            "the override is served whole, not spliced into the built-in"
        );
        for tool in NAMES.iter().filter(|name| **name != "recall") {
            assert_eq!(
                overridden.get(tool).expect("routed").description,
                built_in.get(tool).expect("routed").description,
                "{tool} was not named, so it keeps agmem's wording"
            );
        }
    }

    #[test]
    fn no_overrides_change_nothing() {
        let built_in = AgmemService::tool_router();
        let untouched = described(
            AgmemService::tool_router(),
            &config(ToolDescriptions::default()),
        );
        for tool in NAMES {
            assert_eq!(
                untouched.get(tool).expect("routed").description,
                built_in.get(tool).expect("routed").description
            );
        }
    }
}
