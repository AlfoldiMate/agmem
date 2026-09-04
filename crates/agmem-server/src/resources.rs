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
//! | `memory://<space>/doc/<id>` | a document's text as stored, under its own media type (#135) |
//!
//! `resources/list` serves one entry per registered space, plus the newest
//! few documents of the spaces this session reads — enough for a client's
//! `@` picker, which is not a file browser. The record and document forms
//! are published as *templates*, because a store of a thousand claims must
//! not push a thousand rows at every client that renders the list as a menu.
//! The index is where the concrete URIs come from: each entry carries its own.
//!
//! Everything but a document is `application/json`, shaped by the same serde
//! types the tools answer with — a resource that renders differently from
//! the tool it mirrors would be two answers to one question. A document is
//! the exception on purpose: its JSON form is still there at
//! `memory://<space>/<id>`, and the `doc/` form exists so a plan can be
//! attached to a conversation the way a file is.

use agmem_core::{Episode, EpisodeId, SpaceName};
use agmem_store::StoreError;
use agmem_store::repo::{self, DocumentFilter, Lookup};
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

/// The path segment that tells the document form from the record form.
const DOC_SEGMENT: &str = "doc/";

/// What a document reads as when it was stored without a media type.
const DOC_MIME: &str = "text/plain";

/// How many documents per space `resources/list` shows: the newest, for a
/// picker — every document still answers at its own URI.
const LISTED_DOCUMENTS: usize = 10;

/// The `memory://<space>/doc/<id>` form of a document — what `agmem doc put`
/// prints and what a `recall` hit's `resource_link` points at.
pub fn document_uri(space: &str, id: &str) -> String {
    format!("{SCHEME}{space}/{DOC_SEGMENT}{id}")
}

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

/// One resource per registered space, then the newest documents of the
/// spaces this session reads — `current` and `user`, recall's default pair.
///
/// # Errors
/// [`ErrorData`] with `INTERNAL_ERROR` for a failing store.
pub async fn list(service: &AgmemService) -> Result<ListResourcesResult, ErrorData> {
    let spaces = repo::spaces(service.db())
        .await
        .map_err(|error| store_error(&error))?;
    let mut resources: Vec<Resource> = spaces
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

    let mut attached = vec![service.config().space.clone()];
    if !attached.contains(&SpaceName::user()) {
        attached.push(SpaceName::user());
    }
    let filter = DocumentFilter {
        kinds: Vec::new(),
        tags: Vec::new(),
        limit: LISTED_DOCUMENTS,
    };
    for space in &attached {
        let documents = repo::documents(service.db(), space, &filter)
            .await
            .map_err(|error| store_error(&error))?;
        resources.extend(
            documents
                .into_iter()
                .map(|summary| document_resource(space, &summary.episode)),
        );
    }
    Ok(ListResourcesResult::with_all_items(resources))
}

/// A document as a listed resource: named by its title, sized, typed.
fn document_resource(space: &SpaceName, episode: &Episode) -> Resource {
    let title = episode.title.clone().unwrap_or_default();
    let kind = episode
        .doc_kind
        .map_or("document", agmem_core::DocKind::as_str);
    let mut description = format!("A {kind} in `{space}`");
    if !episode.tags.is_empty() {
        description.push_str(", tagged ");
        description.push_str(&episode.tags.join(", "));
    }
    Resource::new(
        document_uri(space.as_str(), episode.id.as_str()),
        title.clone(),
    )
    .with_title(title)
    .with_description(description)
    .with_mime_type(episode.mime.as_deref().unwrap_or(DOC_MIME))
    .with_size(episode.content.len() as u64)
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
        ResourceTemplate::new(format!("{SCHEME}{{space}}/{DOC_SEGMENT}{{id}}"), "document")
            .with_title("One document, as text")
            .with_description(
                "A named, typed document's text as stored, under its own media \
                 type — the address `agmem doc put` prints, and where a `recall` \
                 hit from a document links to. Its JSON form, with versions and \
                 what cites it, is the record form of the same id.",
            ),
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
            "no such resource: {uri} — this server serves {SCHEME}<space>, \
             {SCHEME}<space>/<id> and {SCHEME}<space>/{DOC_SEGMENT}<id>"
        )));
    };
    let text = match path.split_once('/') {
        None => index(service, path).await?,
        Some((space, rest)) => match rest.strip_prefix(DOC_SEGMENT) {
            Some(id) => return document(service, space, id, uri).await,
            None => record(service, space, rest).await?,
        },
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
            offset: None,
            limit: None,
            doc_kinds: Vec::new(),
            tags: Vec::new(),
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

/// The `memory://<space>/doc/<id>` form: the text, as stored, as itself.
///
/// The one resource that is not a tool's JSON: a client attaching a plan
/// wants the plan, not an envelope around it. Anonymous text has no name to
/// attach by and answers only at its record form, so it is a miss here — a
/// miss that says where the text does read.
async fn document(
    service: &AgmemService,
    space: &str,
    id: &str,
    uri: &str,
) -> Result<ReadResourceResult, ErrorData> {
    let space_name: SpaceName = space.parse().map_err(|_| unknown_space(space))?;
    let episode_id: EpisodeId = id
        .parse()
        .map_err(|_| not_found(format!("{uri}: `{id}` is not a document id")))?;
    let detail = repo::episode(service.db(), &space_name, &episode_id)
        .await
        .map_err(|error| match error {
            StoreError::UnknownEpisode { .. } => {
                not_found(format!("{uri}: no document with that id in `{space}`"))
            }
            other => store_error(&other),
        })?;
    let episode = detail.episode;
    if !episode.is_document() {
        return Err(not_found(format!(
            "{uri}: that is anonymous text, not a document; it reads at {SCHEME}{space}/{id}"
        )));
    }
    let mime = episode.mime.unwrap_or_else(|| DOC_MIME.to_owned());
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(episode.content, uri).with_mime_type(mime),
    ]))
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
