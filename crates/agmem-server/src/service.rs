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
    ErrorData, Json, ServerHandler, ServiceExt,
    handler::server::router::prompt::PromptRouter,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, GetPromptResult, Implementation, PromptMessage, Role, ServerCapabilities,
        ServerInfo,
    },
    prompt, prompt_handler, prompt_router,
    service::ServerInitializeError,
    tool, tool_handler, tool_router,
    transport::stdio,
};

use crate::config::Config;
use crate::prompts::{self, Focus};
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
            tool_router: described(Self::tool_router(), &config),
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
        description = "Store what this session learned — the distilled claims, and optionally the \
verbatim text they came from — so the next session starts with it instead of working it out \
again.\n\n\
Call it as soon as something durable is said, unprompted and without waiting for the end of the \
session: a preference or a standing instruction (\"always use X here\"), a decision and the \
reason behind it, a convention, a constraint, a lesson from something that failed. Nothing you \
say persists — an answer that ends \"noted\" or \"I'll remember that\" without a call to this \
tool is a promise the next session cannot keep.\n\n\
Distil before you call: one atomic, self-contained claim per entry, in the third person, \
understandable with no conversation around it — \"the user prefers Rust over Python for CLI \
tools\", not \"he said he likes it better\". Do not store what the code or the ticket already \
records, or what only matters to this turn.\n\n\
Nothing here is ever rewritten. When something already stored turns out to be wrong, send the \
correction with `supersedes` set to the id of the claim it replaces, rather than storing a \
contradiction: the old claim stays readable and dated, and only one of them is live. You do not \
have to go looking for that id first — see `related` below.\n\n\
Returns a diff rather than an acknowledgement — what was created, what was already stored, what \
was closed, and the episode's id. Two of those carry the id and the text of a claim already in \
the store: `duplicates`, which were **not** written, and `related`, which sit alongside what was. \
Read the `content` of both before you answer. A correction reads much like the claim it corrects, \
so it lands in one of those lists rather than in `created` — and if you report it as remembered \
without checking, the claim that is still live is the old and wrong one. Neither list is a \
verdict: nothing here judges that two claims disagree, which is why they are handed back rather \
than acted on. When one of them says something your claim contradicts, send your claim again with \
`supersedes` set to its id.",
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
claim that only surfaced because it never decays is visible as such. Every hit also carries the \
`source` it was distilled from, which `inspect` takes as it stands — a claim worth acting on is \
one to check, not one to hedge around. Nothing is hidden: a claim that was corrected is absent \
unless you ask with `as_of` or `include_invalidated`, which is how you find out what was \
believed at some earlier point.",
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
is the only way to remove something that must not stay on disk. Purging text does not purge the \
claims distilled from it.\n\n\
Forgetting by `query` takes two calls: send it once with `dry_run: true`, read exactly what \
matched, then send the identical call with `dry_run: false` to act. Any other second call is \
refused — including the same query with `purge` flipped. Ids need no dry run, though `dry_run: \
true` previews them too.\n\n\
A query here matches on the words you write, not on their meaning: it selects the memories that \
contain those terms, so write the words you want gone. That is the opposite of `recall`, and \
deliberately — a deletion should never reach something that merely resembles what you asked for.",
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
`entity:<name>` for everything ever said about a subject, corrected claims included; or `stats` \
for per-space counts.\n\n\
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
Three lists come back, and every claim in each carries its full text rather than only its id, so \
you can judge it here instead of looking it up:\n\n\
- `near_duplicates` — groups of live claims saying the same thing. Merge a group with one \
`remember` call: the one wording worth keeping, and `supersedes` set to the ids of every other \
member. `supersedes` takes a list, so a group of any size closes in that single call — reaching \
for `forget` instead deletes the history the merge exists to keep. Read `min_similarity` before \
you do: it is the weakest pair anywhere in the group, not the weakest link, so a low number means \
the group chained together through a middle claim and may not be one claim at all.\n\
- `contradictions` — pairs about the same subject that are close without being the same. Nothing \
here has judged that they disagree; read both and decide. When one of them is wrong, send the \
right one with the wrong one's id in `supersedes`.\n\
- `stale_contexts` — claims filed as short-lived that recall has kept alive far past the point \
their class would have expired them. If one turned out to be durable, store it again with a \
slower `decay_class`; if it was only scaffolding for one session, `forget` it.\n\n\
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
        description = "Store something you worked out from what memory already holds, together \
with the ids it was drawn from.\n\n\
Call it when reading several memories together tells you something none of them says on its own: a \
pattern across three failures, what a preference and a constraint mean taken together, the reason \
behind a decision that only became visible later. This is the verb for a conclusion you reached; \
`remember` is the verb for something you were told.\n\n\
`derived_from` is required, and it is the point. Pass the ids you actually read — memory ids, \
episode ids, bare or prefixed, exactly as `recall`, `remember`, `context` or `inspect` handed them \
to you. They are stored on the insight and shown by `inspect`, so a later session can see what the \
conclusion was built on and check whether that evidence still stands, instead of taking it on \
faith. An insight with nothing behind it is a `remember` call.\n\n\
Stored as a `lesson` unless you say otherwise, which fades slowly and appears in the Lessons \
section of `context`. Ids are looked for in this space and in the shared `user` space, so an \
insight about the project may cite what is known about the person.\n\n\
Returns the id it was stored under. `created: false` means an equivalent insight was already \
there and nothing was written — read `content`, and if yours says something different, send it \
again with `supersedes` set to that id. `related` carries live claims near the insight with their \
text, for the same decision.",
        annotations(destructive_hint = false, idempotent_hint = true)
    )]
    async fn reflect(
        &self,
        Parameters(params): Parameters<ReflectParams>,
    ) -> Result<Json<ReflectResult>, ErrorData> {
        reflect::run(self, params).await.map(Json)
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
                .build(),
        )
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
