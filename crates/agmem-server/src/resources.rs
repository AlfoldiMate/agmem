//! `memory://` resources — addressable memory, for clients that ask (#31).
//!
//! A progressive enhancement over the tools, never a dependency (design §3.3):
//! everything served here is also the answer of `recall` or `inspect`, and a
//! client that has never heard of resources loses nothing. What a resource
//! adds is a *name* — a claim becomes something a person can @-mention into a
//! conversation, instead of something only the model can go and fetch.
//!
//! The grammar is two forms deep and stops there:
//!
//! | URI | answer |
//! |---|---|
//! | `memory://<space>` | the index: the space's live claims, slim, marked when cut |
//! | `memory://<space>/<id>` | the full `inspect` answer for that id |
//!
//! `resources/list` serves one entry per registered space; the record form is
//! published as a *template*, because a store of a thousand claims must not
//! push a thousand rows at every client that renders the list as a menu. The
//! index is where the concrete URIs come from: each entry carries its own.
//!
//! Everything is `application/json`, shaped by the same serde types the tools
//! answer with — a resource that renders differently from the tool it mirrors
//! would be two answers to one question.

use agmem_core::SpaceName;
use agmem_store::repo::{self, Lookup};
use rmcp::ErrorData;
use rmcp::model::{
    ErrorCode, ListResourceTemplatesResult, ListResourcesResult, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate,
};
use serde::Serialize;

use crate::service::AgmemService;
use crate::tools::inspect::{self, InspectParams};
use crate::tools::{internal, store_error};

/// Every answer here is a serde rendering of what a tool would say.
const MIME: &str = "application/json";

/// The scheme, with the `//` that makes `<space>` an authority — which is why
/// the space never contains a slash and the id never needs escaping: spaces
/// are validated slugs and ids are ULIDs.
const SCHEME: &str = "memory://";

/// The index a `memory://<space>` read answers with.
#[derive(Debug, Serialize)]
struct Index {
    /// The space asked about.
    space: String,
    /// How many live claims the space holds — counted independently of
    /// `memories`, so the number stays honest when the listing is a page.
    live: u64,
    /// The live claims, strongest first — all of them unless `truncated` says
    /// otherwise.
    memories: Vec<IndexEntry>,
    /// Present when the space holds more live claims than one read serves:
    /// `memories` is then the strongest page, not the whole space, and this
    /// says so in words (issue #69) — the same honesty rule as recall's
    /// `truncated`. Absent means the index really is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<String>,
}

/// One line of the index: enough to recognise a claim, and the URI to read it.
#[derive(Debug, Serialize)]
struct IndexEntry {
    /// The record's own `memory://<space>/<id>` form — hand it back to
    /// `resources/read` for the correction chain and the source text.
    uri: String,
    /// The record id, as every tool spells it.
    id: String,
    /// What the claim is.
    kind: agmem_core::Kind,
    /// The claim itself.
    content: String,
}

/// One resource per registered space.
///
/// # Errors
/// [`ErrorData`] with `INTERNAL_ERROR` for a failing store.
pub async fn list(service: &AgmemService) -> Result<ListResourcesResult, ErrorData> {
    let spaces = repo::spaces(service.db())
        .await
        .map_err(|error| store_error(&error))?;
    let resources = spaces
        .into_iter()
        .map(|space| {
            Resource::new(format!("{SCHEME}{space}"), space.to_string())
                .with_title(format!("Memory space `{space}`"))
                .with_description(
                    "The space's live claims, strongest first, with the URI \
                     that reads each one's history and source text.",
                )
                .with_mime_type(MIME)
        })
        .collect();
    Ok(ListResourcesResult::with_all_items(resources))
}

/// The record form, as a template rather than a listing (see the module doc).
pub fn templates() -> ListResourceTemplatesResult {
    ListResourceTemplatesResult::with_all_items(vec![
        ResourceTemplate::new(format!("{SCHEME}{{space}}/{{id}}"), "memory")
            .with_title("One stored memory")
            .with_description(
                "A claim with its correction history and the verbatim text it \
                 was distilled from. Any id another call handed out works — \
                 memory, episode, or chunk.",
            )
            .with_mime_type(MIME),
    ])
}

/// Answer one URI (see the module doc for the grammar).
///
/// # Errors
/// [`ErrorData`] with `RESOURCE_NOT_FOUND` for a URI outside the grammar or
/// naming nothing the store holds, and `INTERNAL_ERROR` for a failing store.
pub async fn read(service: &AgmemService, uri: &str) -> Result<ReadResourceResult, ErrorData> {
    let Some(path) = uri.strip_prefix(SCHEME) else {
        return Err(not_found(format!(
            "no such resource: {uri} — this server serves {SCHEME}<space> and \
             {SCHEME}<space>/<id>"
        )));
    };
    let text = match path.split_once('/') {
        None => index(service, path).await?,
        Some((space, id)) => record(service, space, id).await?,
    };
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(text, uri).with_mime_type(MIME),
    ]))
}

/// The `memory://<space>` form: the space's live claims, slim.
async fn index(service: &AgmemService, name: &str) -> Result<String, ErrorData> {
    // Membership first: a URI naming an unregistered space is a resource that
    // does not exist (-32002), not an empty index — a client retrying a typo
    // should be told so, and `stats` on an unknown space would answer zeros.
    let space: SpaceName = name.parse().map_err(|_| unknown_space(name))?;
    let known = repo::spaces(service.db())
        .await
        .map_err(|error| store_error(&error))?;
    if !known.contains(&space) {
        return Err(unknown_space(name));
    }

    // One lookup serves at most `MAX_POOL` rows however large a limit it is
    // asked for, so past that the index is a page, not the space. The count
    // stays the space's own, and `truncated` marks the cut rather than letting
    // `live: 1500` sit beside 1000 entries as if nothing were missing (#69).
    let stats = repo::stats(service.db(), &space)
        .await
        .map_err(|error| store_error(&error))?;
    let mut lookup = Lookup::new(vec![space.clone()]);
    lookup.limit = repo::MAX_POOL;
    let memories: Vec<IndexEntry> = repo::direct_lookup(service.db(), &lookup)
        .await
        .map_err(|error| store_error(&error))?
        .into_iter()
        .map(|memory| IndexEntry {
            uri: format!("{SCHEME}{space}/{id}", id = memory.id.as_str()),
            id: memory.id.to_string(),
            kind: memory.kind,
            content: memory.content,
        })
        .collect();
    let truncated = (stats.live > memories.len() as u64).then(|| {
        format!(
            "The strongest {listed} of {live} live claims — a page, not the \
             whole space. `recall` reaches the rest, and every claim still \
             answers at its own {SCHEME}{space}/<id>.",
            listed = memories.len(),
            live = stats.live,
        )
    });

    render(&Index {
        space: space.to_string(),
        live: stats.live,
        memories,
        truncated,
    })
}

/// The `memory://<space>/<id>` form: the `inspect` answer, exactly.
///
/// The id goes to `inspect` bare, so the URI resolves whatever the id names —
/// a chunk id from a verbatim hit answers with its episode, just as the tool
/// does. What a caller cannot reach through a URI is inspect's wider grammar
/// (`entity:`, `stats`): a resource is an address, not a query language.
async fn record(service: &AgmemService, space: &str, id: &str) -> Result<String, ErrorData> {
    let result = inspect::run(
        service,
        InspectParams {
            reference: id.to_owned(),
            space: Some(space.to_owned()),
        },
    )
    .await
    .map_err(|error| {
        // Inspect blames the *parameters* of a tool call; here the same miss
        // is a URI naming nothing, and the spec has a code for that. Store
        // failures stay what they are.
        if error.code == ErrorCode::INVALID_PARAMS {
            not_found(format!(
                "{SCHEME}{space}/{id}: {message}",
                message = error.message
            ))
        } else {
            error
        }
    })?;
    render(&result)
}

/// Serde is the renderer, so a resource can never drift from its tool.
fn render<T: Serialize>(value: &T) -> Result<String, ErrorData> {
    serde_json::to_string_pretty(value)
        .map_err(|error| internal(format!("rendering the resource failed: {error}")))
}

fn unknown_space(name: &str) -> ErrorData {
    not_found(format!(
        "no such space: {name} — resources/list names the registered ones"
    ))
}

fn not_found(message: String) -> ErrorData {
    ErrorData::resource_not_found(message, None)
}
