//! One module per MCP tool, attached to [`crate::service::AgmemService`].
//!
//! A tool module owns its request and response types and the flow between
//! them; [`crate::service`] owns only the `#[tool]` attribute that routes to
//! it. What lives here besides the modules is the one thing every tool has to
//! get right and cannot discover on its own: what its annotations must say,
//! and which failures are the caller's fault. The vocabulary the reading tools
//! share sits here too — [`spaces`] resolves `current|user|all|<name>` the same
//! way for every read, [`embed_query`] decides once what a dimensionless
//! embedder means, and [`provenance`] spells a `source` the same way in every
//! answer. The two writing tools share [`resolve_space`] and [`memory_id`] for
//! the same reason: where a claim lands, and what counts as an id, cannot be
//! two different answers depending on which verb was called.
//!
//! # The annotation contract (design §3.1)
//!
//! | tool | annotations |
//! |---|---|
//! | `remember` | `destructive_hint = false, idempotent_hint = true` |
//! | `recall` | `read_only_hint = true, open_world_hint = false` |
//! | `context` | `read_only_hint = true, open_world_hint = false` |
//! | `forget` | `destructive_hint = true` |
//! | `inspect` | `read_only_hint = true, open_world_hint = false` |
//! | `consolidate` | `read_only_hint = true, open_world_hint = false` |
//! | `reflect` | `destructive_hint = false, idempotent_hint = true` |
//!
//! rmcp omits an unset hint from the wire entirely, and the MCP spec's default
//! for a *missing* `destructiveHint` is **true** — so a tool that says nothing
//! is treated as destructive by every client. Read-only tools must therefore
//! spell out `destructive_hint = false` as well as `read_only_hint = true`;
//! saying only the latter leaves the harmful default standing. The unit test
//! below pins that behaviour, so an rmcp upgrade that changes it fails here
//! rather than quietly loosening what a client believes about `forget`.
//!
//! Annotations are declared on the `#[tool]` attribute itself, in the
//! `#[tool_router]` block of [`crate::service`]:
//!
//! ```ignore
//! #[tool(
//!     name = "recall",
//!     annotations(read_only_hint = true, destructive_hint = false, open_world_hint = false)
//! )]
//! ```

pub mod consolidate;
pub mod context;
pub mod forget;
pub mod inspect;
pub mod recall;
pub mod reflect;
pub mod remember;

/// Not a tool: `recall`'s abstention floor and knee trim (see its module
/// doc). Private because only `recall` answers with a page it must be honest
/// about — `context` budgets rather than ranks, and the write path's reads
/// are its own gates.
mod abstain;

/// Not a tool: `recall`'s entity hop (see its module doc). Private because
/// only `recall` may hop — `search_hybrid` also serves `forget`'s dry-run and
/// `context`'s budgeted sections, where rows the query never matched must not
/// widen the set.
mod hop;

/// Not a tool: `recall`'s per-source occupancy cap (see its module doc).
/// Private for the same reason as `hop` — `context` assembles its own page
/// and is deliberately uncapped by *source*, a choice its module records.
/// (Its Lessons section bounds by *tag* instead — [`LESSONS_PER_TAG`].)
mod occupancy;

use std::borrow::Cow;
use std::sync::Arc;

/// Every tool agmem serves, in the order design §3.1 tables them.
///
/// The `#[tool]` attributes in [`crate::service`] are the definition; this is
/// the same list in a form other code can read, and the test below fails if
/// the two ever drift. It exists because a description override names a tool
/// by string (`AGMEM_TOOL_DESC_<TOOL>`), and a name that matches nothing has
/// to be refused rather than quietly dropped.
///
/// `list_tools` does **not** report this order — rmcp's `ToolRouter::list_all`
/// sorts by name.
pub const NAMES: [&str; 7] = [
    "remember",
    "recall",
    "context",
    "forget",
    "inspect",
    "consolidate",
    "reflect",
];

use agmem_core::{MemoryId, Source, SpaceName, Writer};
use agmem_store::{StoreError, repo};
use rmcp::{ErrorData, RoleServer, service::RequestContext};

use crate::service::AgmemService;

/// How many live lessons one tag may hold before the store calls it over-full
/// (issue #82). Reflexion's measured result is that a bounded lesson window of
/// about this size beats unbounded accumulation (arXiv:2303.11366); playbook
/// tags (`role:<agent>`) make unbounded growth the default outcome otherwise.
/// `context` reads it as a per-tag cap on the Lessons section; `consolidate`
/// reads it as the bound whose excess is worth a merge.
pub(crate) const LESSONS_PER_TAG: usize = 3;

/// Which spaces a read looks in (design §3.1).
///
/// `current` and `all` are keywords, not names, so a space actually called one
/// of those is unreachable — the documented vocabulary wins over a slug that
/// collides with it. `user` needs no such rule: the keyword and the reserved
/// space are the same thing, so it falls through and parses.
///
/// Unset means the pair an agent nearly always wants together — this project,
/// and the person behind it.
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS` for a name that is not a valid slug.
pub(crate) async fn spaces(
    service: &AgmemService,
    requested: Option<&str>,
) -> Result<Vec<SpaceName>, ErrorData> {
    let current = service.config().space.clone();
    Ok(match requested {
        None => {
            let mut both = vec![current, SpaceName::user()];
            both.dedup();
            both
        }
        Some("current") => vec![current],
        Some("all") => repo::spaces(service.db())
            .await
            .map_err(|error| store_error(&error))?,
        Some(name) => vec![
            name.parse()
                .map_err(|error| invalid(format!("space: {error}")))?,
        ],
    })
}

/// The query vector for the semantic arms, or `None` in BM25-only mode.
///
/// Both reading tools need one and neither should decide separately what a
/// dimensionless embedder means: `--embedder none` is a degraded but working
/// mode (design §6), not a failure, so it drops the vector arms rather than
/// refusing the call.
///
/// # Errors
/// [`ErrorData`] with `INTERNAL_ERROR` when the embedder fails.
pub(crate) async fn embed_query(
    service: &AgmemService,
    text: &str,
) -> Result<Option<Vec<f32>>, ErrorData> {
    if service.embedder().dim() == 0 {
        return Ok(None);
    }
    agmem_embed::embed_query(Arc::clone(service.embedder()), text.to_owned())
        .await
        .map(Some)
        .map_err(|error| internal(format!("embedding the query failed: {error}")))
}

/// The space a write lands in, registered if it is a new one.
///
/// The read side's keywords resolve here too (issue #65): `current` is the
/// configured space and `user` the reserved one, exactly as [`spaces`] reads
/// them — before this, `remember(space: "current")` created a literal space
/// *named* `current`, which no future read with the same word would ever look
/// in. `all` is refused: a write lands in one space, and "all of them" is not
/// one.
///
/// Startup registers the configured space (design §5.1 step 8); a call that
/// names another one registers it here, so `inspect` can list every space that
/// actually holds something.
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS` for `all`, or a name that is not a
/// valid slug.
pub(crate) async fn resolve_space(
    service: &AgmemService,
    requested: Option<&str>,
) -> Result<SpaceName, ErrorData> {
    let space = match requested {
        None | Some("current") => return Ok(service.config().space.clone()),
        Some("user") => SpaceName::user(),
        Some("all") => {
            return Err(invalid(
                "a write lands in one space; `all` is read-only vocabulary. Name the \
                 space, or leave it unset for the current one.",
            ));
        }
        Some(name) => name
            .parse()
            .map_err(|error| invalid(format!("space: {error}")))?,
    };
    if space != service.config().space {
        repo::ensure_space(service.db(), &space)
            .await
            .map_err(|error| store_error(&error))?;
    }
    Ok(space)
}

/// The `_meta` key a client may send to name the session a write belongs to.
///
/// Speculative on purpose (issue #75): no MCP field carries a host session id
/// over stdio today, but the seam costs nothing and a client that starts
/// sending one is attributed correctly from that moment on.
const SESSION_META_KEY: &str = "agmem/session";

/// Who is performing this write (issue #75), assembled from the request.
///
/// The client name and version come from the MCP `initialize` handshake,
/// which rmcp keeps on the connection; the session is the id a client offered
/// in `_meta`, falling back to the one the service minted for this
/// connection. `unknown` stands in for a client that never introduced itself
/// — a fact about the session, not a guess about the writer.
pub(crate) fn writer(
    service: &AgmemService,
    context: &RequestContext<RoleServer>,
    tool: &str,
) -> Writer {
    let client = context.client_info();
    Writer {
        client: client
            .as_ref()
            .map(|info| info.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        client_version: client
            .as_ref()
            .map(|info| info.version.clone())
            .filter(|version| !version.is_empty()),
        session: context
            .meta
            .get(SESSION_META_KEY)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| service.session().to_owned()),
        tool: tool.to_owned(),
    }
}

/// A memory id as sent, with or without the `memory:` table prefix agmem's own
/// output leaves off.
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS`, naming `field`, for anything else.
pub(crate) fn memory_id(raw: &str, field: &str) -> Result<MemoryId, ErrorData> {
    MemoryId::new(raw.strip_prefix("memory:").unwrap_or(raw))
        .map_err(|error| invalid(format!("{field}: {error}")))
}

/// A memory's provenance in the form `inspect`'s `ref` takes.
///
/// One string rather than a nested object, because it is a *pointer*: an agent
/// reading `episode:01M…` on a recall hit can pass it straight back to
/// `inspect` and get the verbatim text behind the claim.
pub(crate) fn provenance(source: &Source) -> String {
    match source {
        Source::Agent => "agent".to_owned(),
        Source::Episode { episode } => format!("episode:{episode}"),
        Source::External { origin } => format!("external:{origin}"),
    }
}

/// The caller sent something that cannot be acted on.
///
/// `INVALID_PARAMS` is what an agent can actually do something about: it names
/// the field, and re-sending the same request unchanged will fail the same way.
pub(crate) fn invalid(message: impl Into<Cow<'static, str>>) -> ErrorData {
    ErrorData::invalid_params(message, None)
}

/// The request was fine and agmem could not serve it.
pub(crate) fn internal(message: impl Into<Cow<'static, str>>) -> ErrorData {
    ErrorData::internal_error(message, None)
}

/// A store failure, sorted by whose fault it is.
///
/// Only one variant is ever the caller's: a `supersedes` id naming a memory
/// this space does not hold. Everything else is ours, and is logged here
/// because the agent is not going to read a database error usefully — it is
/// still sent, though, since a local single-user server has no one else to
/// tell.
pub(crate) fn store_error(error: &StoreError) -> ErrorData {
    match error {
        StoreError::UnknownMemory { .. } => invalid(error.to_string()),
        other => {
            tracing::error!(error = %other, "the store refused a tool call");
            internal(other.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use rmcp::model::ToolAnnotations;

    use super::memory_id;

    #[test]
    fn a_supersedes_id_is_accepted_with_or_without_its_table() {
        let bare = "01M145SMNET1XRYA713EWAQTD3";
        assert_eq!(
            memory_id(bare, "f").expect("bare ulid").as_str(),
            memory_id(&format!("memory:{bare}"), "f")
                .expect("prefixed")
                .as_str(),
            "agmem returns bare ULIDs but the schema sketch shows memory:…; \
             both round-trip"
        );
        let error = memory_id("not-an-id", "memories[2].supersedes").expect_err("rejected");
        assert!(
            error.message.contains("memories[2].supersedes"),
            "a rejection has to name the entry that caused it: {}",
            error.message
        );
    }

    #[test]
    fn an_unset_hint_is_read_as_the_dangerous_default() {
        let silent = ToolAnnotations::new();
        assert!(
            silent.is_destructive(),
            "a tool that declares nothing is destructive; read-only tools must \
             set destructive_hint = false explicitly"
        );
        assert!(!silent.is_idempotent());

        let read_only = ToolAnnotations::new().read_only(true).destructive(false);
        assert!(!read_only.is_destructive());
        assert_eq!(read_only.read_only_hint, Some(true));
    }

    #[test]
    fn unset_hints_stay_off_the_wire() {
        let json = serde_json::to_value(ToolAnnotations::new().read_only(true)).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({ "readOnlyHint": true }),
            "rmcp omits what was never set, rather than sending the default"
        );
    }
}
