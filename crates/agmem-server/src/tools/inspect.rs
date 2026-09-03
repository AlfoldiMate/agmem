//! `inspect` — provenance and history made walkable (design §3.1).
//!
//! This is the trust verb, and the poisoning defence. Everything else in agmem
//! hands the agent a claim; this hands it the paper trail — which text the
//! claim was distilled from, what it used to say before it was corrected, and
//! what the store actually holds. A memory system that cannot be audited is
//! one an agent has to take on faith, and taking a stored sentence on faith is
//! exactly how a bad memory outlives the session that made it.
//!
//! One `ref` grammar covers all four questions, because they are the same
//! question asked of different rows:
//!
//! | `ref` | answer |
//! |---|---|
//! | `memory:<id>` | the record, its whole supersession chain oldest→newest, the text it came from, and what it was reflected out of |
//! | a bare id | whichever of those rows it names — a verbatim hit hands out a *chunk* id, which answers with its episode |
//! | `episode:<id>` | the verbatim text, its retrieval slices, and every claim distilled from it |
//! | `entity:<name>` | every claim naming that subject, closed ones included |
//! | `doc:<space>/<title>` | the newest document under that title, its earlier versions listed behind it (#134) |
//! | `docs` or `docs:<space>` | the documents a space holds, newest first, with how many claims cite each |
//! | `stats` | per-space counts |
//!
//! A document's content comes back **windowed** — `offset`/`limit` in chars,
//! one chunk's worth by default — because a plan can be 100k characters and a
//! tool result that size is a context the agent cannot use.

use std::fmt;

use agmem_core::{
    ChunkId, DecayClass, Derivation, DocKind, EpisodeId, Kind, MemoryId, MemoryRecord, Source,
    SpaceName,
};
use agmem_store::StoreError;
use agmem_store::repo::{self, DocumentFilter, Filters, Liveness, Lookup};
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::service::AgmemService;
use crate::tools::{internal, invalid, provenance, store_error};

/// One `inspect` call: what to look at, and where.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct InspectParams {
    /// What to look at: `memory:<id>` for a claim's history and the text
    /// behind it, `episode:<id>` for stored verbatim text, `entity:<name>`
    /// for everything said about a subject, `doc:<space>/<title>` for the
    /// newest document under a title, `docs` or `docs:<space>` to list
    /// documents, or `stats` for what each space holds. Any bare id another
    /// call handed you also works, whichever of those it turns out to name.
    #[serde(rename = "ref")]
    pub reference: String,

    /// Where to look: `current`, `user`, `all`, or a space name. Defaults to
    /// `current` and `user` together — or to `all` for `stats`, which is a
    /// question about the whole store.
    #[serde(default)]
    pub space: Option<String>,

    /// For an episode: where in its content to start reading, in characters.
    /// Defaults to the beginning.
    #[serde(default)]
    pub offset: Option<usize>,

    /// For an episode: how many characters of content to return. A document
    /// defaults to one chunk's worth and says in `window` where to continue;
    /// anonymous text comes back whole unless this is set.
    #[serde(default)]
    pub limit: Option<usize>,

    /// For `docs`: keep only these kinds.
    #[serde(default)]
    pub doc_kinds: Vec<DocKind>,

    /// For `docs`: keep only documents carrying one of these tags.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// What was found, and where it was looked for.
#[derive(Debug, Serialize, JsonSchema)]
pub struct InspectResult {
    /// The reference this answers, in its canonical form.
    #[serde(rename = "ref")]
    pub reference: String,

    /// The spaces that were searched.
    pub spaces: Vec<String>,

    /// The answer, shaped by what `ref` named.
    pub found: Inspected,
}

/// One of the four answers, tagged by `kind`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inspected {
    /// A claim, where it came from, and what it used to say.
    Memory {
        /// The claim asked about.
        // Boxed because it dwarfs the other variants. `Box` is transparent to
        // serde and schemars alike, so neither the wire shape nor the schema
        // changes — and this stays a `//` comment, because a doc comment here
        // would ship the reason to every agent's context.
        memory: Box<MemoryView>,

        /// Its whole supersession chain, oldest first, this claim included. A
        /// claim that has never been corrected is a chain of one; a longer one
        /// is a belief that changed, and each link is dated.
        chain: Vec<MemoryView>,

        /// The verbatim text it was distilled from, when there is one. Quote
        /// from this rather than from the claim when precision matters.
        #[serde(skip_serializing_if = "Option::is_none")]
        episode: Option<EpisodeView>,

        /// The full records behind the claim's `derived_from` citations —
        /// what a `summary` stands in for, expanded one call deep (issue
        /// #85). Only memory citations expand; an episode citation stays a
        /// ref, reachable through its own `inspect`. Absent when the claim
        /// cites no memories, or when every cited one has been purged.
        #[serde(skip_serializing_if = "Option::is_none")]
        expands: Option<Vec<MemoryView>>,
    },

    /// Stored verbatim text and everything that came out of it.
    Episode {
        /// The text, unedited — or the window of it that was asked for.
        episode: EpisodeView,

        /// The slices retrieval matches against, in reading order. On a
        /// document they carry their size and not their text: read the
        /// content through the window instead.
        chunks: Vec<ChunkView>,

        /// The claims distilled from it or drawn from it, oldest first.
        derived: Vec<MemoryView>,

        /// Which part of the content `episode.content` holds, when it is not
        /// the whole of it.
        #[serde(skip_serializing_if = "Option::is_none")]
        window: Option<Window>,

        /// On a document: every version stored under its title, newest
        /// first, this one included. The first is the current one.
        #[serde(skip_serializing_if = "Option::is_none")]
        versions: Option<Vec<VersionView>>,
    },

    /// The documents a space holds.
    Documents {
        /// Newest first. Each carries how many live claims cite it, so an
        /// orphan — a document nothing was learned from — is visible as such.
        documents: Vec<DocumentView>,
    },

    /// Everything said about one subject.
    Entity {
        /// The subject asked about.
        entity: String,

        /// Every claim naming it, strongest first — closed ones included, so
        /// a contradiction shows up as history rather than as a conflict.
        memories: Vec<MemoryView>,
    },

    /// What the store holds.
    Stats {
        /// One entry per space searched.
        counts: Vec<SpaceCounts>,
    },
}

/// A memory as `inspect` renders it: the whole row, including the counters
/// `recall` folds away into a score.
#[derive(Debug, Serialize, JsonSchema)]
pub struct MemoryView {
    /// The record id.
    pub id: String,

    /// What the claim is.
    pub kind: Kind,

    /// The claim itself.
    pub content: String,

    /// The space holding it.
    pub space: String,

    /// The subjects it is about.
    pub entities: Vec<String>,

    /// Its labels.
    pub tags: Vec<String>,

    /// Where it came from: `agent`, `episode:<id>`, or `external:<origin>`.
    pub source: String,

    /// Who wrote the row: the MCP client, the session the write belonged to,
    /// and the verb that performed it. Absent on rows stored before agmem
    /// recorded writers — absence means "not recorded", never "unknown
    /// client" (see the `derived_from` note below on why this is `Option`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writer: Option<WriterView>,

    /// What this claim was reflected out of, when `reflect` wrote it: the
    /// memories and episodes it was drawn from, as refs to hand straight back
    /// to `inspect`. Absent on a claim that cites nothing.
    ///
    /// This is what makes a conclusion checkable rather than something to take
    /// on faith — the evidence is named, and each piece of it has its own
    /// history to walk.
    ///
    // An `Option` rather than a `Vec` with `skip_serializing_if`: schemars
    // marks a bare `Vec` *required* whatever serde then omits, and a schema
    // that requires a field the answer leaves out is one a strict client is
    // entitled to reject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<Vec<String>>,

    /// How fast it fades between uses.
    pub decay_class: DecayClass,

    /// Ebbinghaus stability: 1.0 when written, raised by every recall that
    /// returns it. A high number on an old memory means it is still in use.
    pub strength: f64,

    /// How often recall has returned it.
    pub access_count: u32,

    /// When recall last returned it, RFC3339.
    pub last_accessed: String,

    /// When the claim started being true, RFC3339.
    pub valid_from: String,

    /// When it stopped; absent while it is still live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_at: Option<String>,

    /// Why it stopped: `superseded`, `forgotten`, or `expired`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,

    /// The claims this one corrected — several when it merged a duplicate
    /// cluster into one wording. Absent, not empty, when it corrected nothing
    /// (see the `derived_from` note above on why this is `Option`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<Vec<String>>,

    /// The claim that corrected this one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,

    /// When the row was written, RFC3339.
    pub created_at: String,
}

/// Who performed the write that created a row (issue #75).
#[derive(Debug, Serialize, JsonSchema)]
pub struct WriterView {
    /// The client's name from its MCP handshake; `unknown` when the session
    /// never introduced itself.
    pub client: String,

    /// The client's version, when it offered one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,

    /// The session the write belonged to.
    pub session: String,

    /// The verb that performed the write: `remember` or `reflect`.
    pub tool: String,
}

/// Verbatim text as stored — never rewritten, never superseded.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EpisodeView {
    /// The record id.
    pub id: String,

    /// The space holding it.
    pub space: String,

    /// The text, exactly as it was given — or the requested window of it,
    /// when `window` is present.
    pub content: String,

    /// How many characters the whole text has.
    pub chars: usize,

    /// When the events described happened, RFC3339.
    pub occurred_at: String,

    /// The conversation or working session it belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,

    /// When the row was written, RFC3339.
    pub created_at: String,

    /// The document's name. Present with `doc_kind`; absent on anonymous
    /// text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    /// What kind of document this is; absent on anonymous text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_kind: Option<DocKind>,

    /// A document's labels; absent when it has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,

    /// The content's media type, when one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

/// One retrieval slice of an episode.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ChunkView {
    /// The record id.
    pub id: String,

    /// Zero-based position within the episode.
    pub position: u32,

    /// How many characters the slice has.
    pub chars: usize,

    /// The slice. Absent on a document's chunks — its content is read
    /// through the window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Which part of an episode's content the answer holds.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Window {
    /// The character offset the returned content starts at.
    pub offset: usize,

    /// How many characters were returned.
    pub returned: usize,

    /// How many characters the whole content has.
    pub total: usize,

    /// The `offset` to send to read on from here; absent at the end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

/// One version of a document, in its title's chain.
#[derive(Debug, Serialize, JsonSchema)]
pub struct VersionView {
    /// The record id; `inspect` it to read that version.
    pub id: String,

    /// When it was stored, RFC3339.
    pub created_at: String,

    /// How many characters it has.
    pub chars: usize,
}

/// One document on a listing.
#[derive(Debug, Serialize, JsonSchema)]
pub struct DocumentView {
    /// The record id.
    pub id: String,

    /// The space holding it.
    pub space: String,

    /// Its name. Several documents can share one: the newest is current.
    pub title: String,

    /// What kind of document it is.
    pub doc_kind: DocKind,

    /// Its labels.
    pub tags: Vec<String>,

    /// The content's media type, when one was recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,

    /// How many characters it has.
    pub chars: usize,

    /// How many live claims cite it, through `source` or `derived_from`.
    /// Zero means nothing was learned from it, or everything learned from
    /// it has since been closed.
    pub cited: u64,

    /// When it was stored, RFC3339.
    pub created_at: String,
}

/// What one space holds.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SpaceCounts {
    /// The space these counts describe.
    pub space: String,

    /// Every memory ever written here, live or closed.
    pub memories: u64,

    /// Those still live.
    pub live: u64,

    /// Those closed — superseded, forgotten, or expired. Memories are never
    /// deleted, so this is history rather than loss.
    pub invalidated: u64,

    /// Verbatim episodes.
    pub episodes: u64,

    /// Retrieval slices of those episodes.
    pub chunks: u64,

    /// Live memories per kind; a kind with none is absent.
    pub live_by_kind: Vec<KindCount>,
}

/// How many live memories of one kind a space holds.
#[derive(Debug, Serialize, JsonSchema)]
pub struct KindCount {
    /// The kind.
    pub kind: Kind,

    /// How many.
    pub count: u64,
}

/// What a `ref` named.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reference {
    Memory(MemoryId),
    Episode(EpisodeId),
    Entity(String),
    Stats,
    /// The newest document under a title in one space — the space token is
    /// resolved like `space` is (`current`, `user`, a name), never `all`.
    Doc {
        space: String,
        title: String,
    },
    /// The documents in one space, or in the searched spaces when unnamed.
    Docs(Option<String>),
    /// A bare ULID, before the store has said which table answers to it. It
    /// never survives a call: `run` replaces it with what it resolved to, so
    /// the echoed `ref` is always canonical.
    Unqualified(String),
}

/// Answer one reference (design §3.1).
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS` for a `ref` that is not in the grammar,
/// a bad space name, or an id none of the searched spaces holds — and
/// `INTERNAL_ERROR` for a failing store.
pub async fn run(
    service: &AgmemService,
    params: InspectParams,
) -> Result<InspectResult, ErrorData> {
    let reference = parse(&params.reference)?;
    // `stats` is a question about the store rather than about a row, so with
    // no space given it covers every space rather than the usual two. A
    // `doc:`/`docs:` ref names its own space, which wins over `space`.
    let requested = match &reference {
        Reference::Doc { space, .. } => Some(space.as_str()),
        Reference::Docs(Some(space)) => Some(space.as_str()),
        Reference::Stats => params.space.as_deref().or(Some("all")),
        _ => params.space.as_deref(),
    };
    let spaces = crate::tools::spaces(service, requested).await?;
    let window = WindowSpec {
        offset: params.offset,
        limit: params.limit,
    };

    // Every arm hands back the reference it *resolved* to, not the one it was
    // given: a bare id only becomes `memory:` or `episode:` once the store has
    // said which it is, and the echoed `ref` has to be the canonical form.
    let (reference, found) = match reference {
        Reference::Stats => (
            Reference::Stats,
            Inspected::Stats {
                counts: counts(service, &spaces).await?,
            },
        ),
        Reference::Docs(named) => {
            let filter = DocumentFilter {
                kinds: params.doc_kinds.clone(),
                tags: params.tags.clone(),
                limit: DOCS_PER_LISTING,
            };
            (
                Reference::Docs(named),
                Inspected::Documents {
                    documents: documents(service, &spaces, &filter).await?,
                },
            )
        }
        Reference::Doc { space, title } => {
            let found = document(service, &spaces, &title, window)
                .await?
                .ok_or_else(|| {
                    missing(
                        "document titled",
                        &format!("{title:?}"),
                        &spaces,
                        "list what is there with `docs`, or check the space",
                    )
                })?;
            (Reference::Doc { space, title }, found)
        }
        Reference::Entity(name) => {
            let memories = about(service, &spaces, &name).await?;
            (
                Reference::Entity(name.clone()),
                Inspected::Entity {
                    entity: name,
                    memories,
                },
            )
        }
        Reference::Memory(id) => {
            let found = memory(service, &spaces, &id).await?.ok_or_else(|| {
                missing(
                    "memory",
                    id.as_str(),
                    &spaces,
                    "recall it first, widen `space`, or drop the `memory:` prefix — \
                     a bare id resolves chunks and episodes too",
                )
            })?;
            (Reference::Memory(id), found)
        }
        Reference::Episode(id) => {
            let found = episode(service, &spaces, &id, window)
                .await?
                .ok_or_else(|| missing("episode", id.as_str(), &spaces, FIRST_OR_WIDER))?;
            (Reference::Episode(id), found)
        }
        Reference::Unqualified(id) => unqualified(service, &spaces, &id, window).await?,
    };

    Ok(InspectResult {
        reference: reference.to_string(),
        spaces: spaces.iter().map(ToString::to_string).collect(),
        found,
    })
}

/// A claim, its chain, and the text behind it.
///
/// The chain is walked in whichever space holds the id: ids are unique across
/// the store but *scoped* by the queries that read them, so an id from a
/// recall that unioned two spaces has to be tried against both.
async fn memory(
    service: &AgmemService,
    spaces: &[SpaceName],
    id: &MemoryId,
) -> Result<Option<Inspected>, ErrorData> {
    let mut walked = None;
    for space in spaces {
        match repo::history_chain(service.db(), space, id).await {
            Ok(chain) => {
                walked = Some((space, chain));
                break;
            }
            Err(StoreError::UnknownMemory { .. }) => continue,
            Err(error) => return Err(store_error(&error)),
        }
    }
    let Some((space, chain)) = walked else {
        return Ok(None);
    };
    let target = chain
        .iter()
        .find(|link| link.id == *id)
        .ok_or_else(unreachable_chain)?
        .clone();

    // The episode is fetched whole and only its text kept. `inspect` is a
    // deliberate, rare call, and one read that cannot disagree with itself
    // beats two that can.
    let episode = match &target.source {
        Source::Episode { episode } => match repo::episode(service.db(), space, episode).await {
            Ok(detail) => Some(detail.episode.into()),
            // `forget` can purge the text while leaving the claims drawn from
            // it (design §5.4), so a source that names nothing is history
            // rather than a broken store: the claim still says where it came
            // from, and there is simply nothing left to quote.
            Err(StoreError::UnknownEpisode { .. }) => None,
            Err(error) => return Err(store_error(&error)),
        },
        Source::Agent | Source::External { .. } => None,
    };

    // The citations, expanded to their full records. A cited memory can live
    // in another searched space (`reflect` resolves citations against the
    // written space and `user` alike), so each is walked the way the target
    // was; one that resolves nowhere was purged, which is history rather than
    // an error — the ref itself still stands in `derived_from`.
    let mut expands = Vec::new();
    for cited in &target.derived_from {
        let Derivation::Memory(cited) = cited else {
            continue;
        };
        if let Some(found) = view(service, spaces, cited).await? {
            expands.push(found);
        }
    }

    Ok(Some(Inspected::Memory {
        memory: Box::new(target.into()),
        chain: chain.into_iter().map(MemoryView::from).collect(),
        episode,
        expands: (!expands.is_empty()).then_some(expands),
    }))
}

/// One memory's current row, from whichever searched space holds it.
async fn view(
    service: &AgmemService,
    spaces: &[SpaceName],
    id: &MemoryId,
) -> Result<Option<MemoryView>, ErrorData> {
    for space in spaces {
        match repo::history_chain(service.db(), space, id).await {
            Ok(chain) => {
                return Ok(chain
                    .into_iter()
                    .find(|link| link.id == *id)
                    .map(MemoryView::from));
            }
            Err(StoreError::UnknownMemory { .. }) => continue,
            Err(error) => return Err(store_error(&error)),
        }
    }
    Ok(None)
}

/// Verbatim text, its slices, and what was distilled from it.
///
/// `Ok(None)` when no searched space holds it — the caller decides whether
/// that is an error or the next thing to try.
async fn episode(
    service: &AgmemService,
    spaces: &[SpaceName],
    id: &EpisodeId,
    window: WindowSpec,
) -> Result<Option<Inspected>, ErrorData> {
    for space in spaces {
        match repo::episode(service.db(), space, id).await {
            Ok(detail) => return Ok(Some(detailed(service, space, detail, window).await?)),
            Err(StoreError::UnknownEpisode { .. }) => continue,
            Err(error) => return Err(store_error(&error)),
        }
    }
    Ok(None)
}

/// The newest document under `title`, from the first searched space that has
/// one.
async fn document(
    service: &AgmemService,
    spaces: &[SpaceName],
    title: &str,
    window: WindowSpec,
) -> Result<Option<Inspected>, ErrorData> {
    for space in spaces {
        let versions = repo::documents_by_title(service.db(), space, title)
            .await
            .map_err(|error| store_error(&error))?;
        let Some(newest) = versions.first() else {
            continue;
        };
        let detail = repo::episode(service.db(), space, &newest.id)
            .await
            .map_err(|error| store_error(&error))?;
        return Ok(Some(detailed(service, space, detail, window).await?));
    }
    Ok(None)
}

/// The documents in each searched space, newest first within a space.
async fn documents(
    service: &AgmemService,
    spaces: &[SpaceName],
    filter: &DocumentFilter,
) -> Result<Vec<DocumentView>, ErrorData> {
    let mut listed = Vec::new();
    for space in spaces {
        let page = repo::documents(service.db(), space, filter)
            .await
            .map_err(|error| store_error(&error))?;
        listed.extend(page.into_iter().filter_map(|summary| {
            let episode = summary.episode;
            Some(DocumentView {
                id: episode.id.to_string(),
                space: episode.space.to_string(),
                title: episode.title?,
                doc_kind: episode.doc_kind?,
                tags: episode.tags,
                mime: episode.mime,
                chars: episode.content.chars().count(),
                cited: summary.cited,
                created_at: episode.created_at.to_string(),
            })
        }));
    }
    Ok(listed)
}

/// An episode's answer: the window of its content, its slices, what came out
/// of it, and — on a document — the versions under its title.
async fn detailed(
    service: &AgmemService,
    space: &SpaceName,
    detail: repo::EpisodeDetail,
    window: WindowSpec,
) -> Result<Inspected, ErrorData> {
    let is_document = detail.episode.is_document();
    let versions = match (&detail.episode.title, is_document) {
        (Some(title), true) => {
            let chain = repo::documents_by_title(service.db(), space, title)
                .await
                .map_err(|error| store_error(&error))?;
            Some(
                chain
                    .into_iter()
                    .map(|version| VersionView {
                        id: version.id.to_string(),
                        created_at: version.created_at.to_string(),
                        chars: version.content.chars().count(),
                    })
                    .collect(),
            )
        }
        _ => None,
    };

    // The default window on a document is one chunk's worth — the unit
    // retrieval already hands out — so a 60-chunk plan is read on the
    // agent's terms rather than dumped. Anonymous text keeps its old shape:
    // whole, unless a window was asked for.
    let default_limit = if is_document {
        detail
            .chunks
            .first()
            .map(|chunk| chunk.text.chars().count())
            .filter(|&chars| chars > 0)
    } else {
        None
    };
    let (content, window) = window.apply(&detail.episode.content, default_limit);

    let chunks = detail
        .chunks
        .into_iter()
        .map(|chunk| ChunkView {
            id: chunk.id.into(),
            position: chunk.position,
            chars: chunk.text.chars().count(),
            text: (!is_document).then_some(chunk.text),
        })
        .collect();
    let mut episode = EpisodeView::from(detail.episode);
    episode.content = content;

    Ok(Inspected::Episode {
        episode,
        chunks,
        derived: detail.derived.into_iter().map(MemoryView::from).collect(),
        window,
        versions,
    })
}

/// How many documents one listing shows per space.
const DOCS_PER_LISTING: usize = 50;

/// The window a caller asked for, before it meets the content.
#[derive(Debug, Clone, Copy, Default)]
struct WindowSpec {
    offset: Option<usize>,
    limit: Option<usize>,
}

impl WindowSpec {
    /// The requested slice of `content`, in chars, and where it sits.
    ///
    /// `None` for the window when the whole content came back unasked — the
    /// answer's shape for anonymous text is then exactly what it was before
    /// windows existed.
    fn apply(self, content: &str, default_limit: Option<usize>) -> (String, Option<Window>) {
        let total = content.chars().count();
        let limit = self.limit.or(default_limit);
        if self.offset.is_none() && limit.is_none() {
            return (content.to_owned(), None);
        }
        let offset = self.offset.unwrap_or(0).min(total);
        let limit = limit.unwrap_or(total.saturating_sub(offset));
        let slice: String = content.chars().skip(offset).take(limit).collect();
        let returned = slice.chars().count();
        let end = offset + returned;
        (
            slice,
            Some(Window {
                offset,
                returned,
                total,
                next_offset: (end < total).then_some(end),
            }),
        )
    }
}

/// A bare id, resolved against every table that could answer to it.
///
/// `recall` hands out bare ids for both kinds of hit, and a verbatim hit's id
/// is a *chunk* id — so the obvious follow-up call was the one that failed,
/// with an error blaming `space` for an id that was never a memory (issue
/// #36). A ULID says nothing about its table, so each is asked in turn:
/// memory first, since that is nearly always what an id names; then the chunk
/// a verbatim hit points at, answered with the episode it belongs to; then the
/// episode itself, for an id copied out of a hit's `source`.
async fn unqualified(
    service: &AgmemService,
    spaces: &[SpaceName],
    id: &str,
    window: WindowSpec,
) -> Result<(Reference, Inspected), ErrorData> {
    // `parse` accepted this as a ULID and all three newtypes validate the same
    // thing, so none of these can fail — but nothing here panics on a store
    // that surprises us.
    let memory_id = MemoryId::new(id).map_err(|_| grammar(id))?;
    if let Some(found) = memory(service, spaces, &memory_id).await? {
        return Ok((Reference::Memory(memory_id), found));
    }

    let chunk_id = ChunkId::new(id).map_err(|_| grammar(id))?;
    for space in spaces {
        let parent = repo::episode_of_chunk(service.db(), space, &chunk_id)
            .await
            .map_err(|error| store_error(&error))?;
        if let Some(parent) = parent {
            let found = episode(service, spaces, &parent, window)
                .await?
                .ok_or_else(unreachable_chunk)?;
            return Ok((Reference::Episode(parent), found));
        }
    }

    let episode_id = EpisodeId::new(id).map_err(|_| grammar(id))?;
    if let Some(found) = episode(service, spaces, &episode_id, window).await? {
        return Ok((Reference::Episode(episode_id), found));
    }

    Err(missing(
        "memory, episode or chunk",
        id,
        spaces,
        FIRST_OR_WIDER,
    ))
}

/// Every claim naming one subject, closed ones included.
///
/// `inspect` is the audit verb, so the window is deliberately wider than
/// `recall`'s: a claim that was corrected is part of what was said about a
/// subject, and each one carries its own `invalid_at`.
async fn about(
    service: &AgmemService,
    spaces: &[SpaceName],
    entity: &str,
) -> Result<Vec<MemoryView>, ErrorData> {
    let mut lookup = Lookup::new(spaces.to_vec());
    lookup.filters = Filters {
        entities: vec![entity.to_owned()],
        ..Filters::default()
    };
    lookup.liveness = Liveness::Any;
    lookup.limit = usize::from(service.config().pool);
    Ok(repo::direct_lookup(service.db(), &lookup)
        .await
        .map_err(|error| store_error(&error))?
        .into_iter()
        .map(MemoryView::from)
        .collect())
}

/// What each space holds.
async fn counts(
    service: &AgmemService,
    spaces: &[SpaceName],
) -> Result<Vec<SpaceCounts>, ErrorData> {
    let mut counts = Vec::with_capacity(spaces.len());
    for space in spaces {
        let stats = repo::stats(service.db(), space)
            .await
            .map_err(|error| store_error(&error))?;
        counts.push(SpaceCounts {
            space: stats.space.to_string(),
            memories: stats.memories,
            live: stats.live,
            invalidated: stats.memories.saturating_sub(stats.live),
            episodes: stats.episodes,
            chunks: stats.chunks,
            live_by_kind: stats
                .live_by_kind
                .into_iter()
                .map(|(kind, count)| KindCount { kind, count })
                .collect(),
        });
    }
    Ok(counts)
}

/// A `ref`, or a message that teaches the grammar.
///
/// A bare ULID is accepted because that is what every other tool hands the
/// agent: `remember` returns bare ids and so does a `recall` hit, so requiring
/// a prefix here would make the obvious call fail. It stays *unqualified*
/// until the store answers — a verbatim hit's id is a chunk id, and nothing in
/// the id itself says so.
fn parse(raw: &str) -> Result<Reference, ErrorData> {
    let raw = raw.trim();
    if raw == "stats" {
        return Ok(Reference::Stats);
    }
    if raw == "docs" {
        return Ok(Reference::Docs(None));
    }
    match raw.split_once(':') {
        Some(("memory", id)) => MemoryId::new(id)
            .map(Reference::Memory)
            .map_err(|_| grammar(raw)),
        Some(("episode", id)) => EpisodeId::new(id)
            .map(Reference::Episode)
            .map_err(|_| grammar(raw)),
        Some(("entity", name)) if !name.trim().is_empty() => {
            Ok(Reference::Entity(name.trim().to_owned()))
        }
        Some(("docs", space)) if space_token(space) => {
            Ok(Reference::Docs(Some(space.trim().to_owned())))
        }
        // The title may itself contain `/`; the space cannot.
        Some(("doc", rest)) => match rest.split_once('/') {
            Some((space, title)) if space_token(space) && !title.trim().is_empty() => {
                Ok(Reference::Doc {
                    space: space.trim().to_owned(),
                    title: title.trim().to_owned(),
                })
            }
            _ => Err(grammar(raw)),
        },
        _ if MemoryId::new(raw).is_ok() => Ok(Reference::Unqualified(raw.to_owned())),
        _ => Err(grammar(raw)),
    }
}

/// Whether a ref's space part is something `space` would accept: an alias
/// or a slug. `all` is not a place a title can be looked up in.
fn space_token(raw: &str) -> bool {
    let raw = raw.trim();
    raw != "all" && (matches!(raw, "current" | "user") || SpaceName::new(raw).is_ok())
}

/// The grammar, as something to act on rather than guess at.
fn grammar(raw: &str) -> ErrorData {
    invalid(format!(
        "ref must be `memory:<id>`, `episode:<id>`, `entity:<name>`, `doc:<space>/<title>`, \
         `docs` or `docs:<space>`, `stats`, or a bare id; got {raw:?}"
    ))
}

/// The advice most missing refs end with.
const FIRST_OR_WIDER: &str = "recall it first, or widen `space`";

/// Nothing in any of the searched spaces answers to this id.
///
/// The hint belongs to the caller, because the useful advice differs: a
/// `memory:<id>` that misses is often a chunk id wearing the wrong prefix, and
/// telling that caller to widen `space` sends them the wrong way (issue #36).
fn missing(what: &str, id: &str, spaces: &[SpaceName], hint: &str) -> ErrorData {
    let names: Vec<&str> = spaces.iter().map(SpaceName::as_str).collect();
    invalid(format!("no {what} {id} in {}; {hint}", names.join(", ")))
}

impl From<MemoryRecord> for MemoryView {
    fn from(memory: MemoryRecord) -> Self {
        Self {
            id: memory.id.into(),
            kind: memory.kind,
            content: memory.content,
            space: memory.space.into(),
            entities: memory.entities,
            tags: memory.tags,
            source: provenance(&memory.source),
            writer: memory.writer.map(|writer| WriterView {
                client: writer.client,
                client_version: writer.client_version,
                session: writer.session,
                tool: writer.tool,
            }),
            derived_from: (!memory.derived_from.is_empty()).then(|| {
                memory
                    .derived_from
                    .iter()
                    .map(ToString::to_string)
                    .collect()
            }),
            decay_class: memory.decay_class,
            strength: memory.strength,
            access_count: memory.access_count,
            last_accessed: memory.last_accessed.to_string(),
            valid_from: memory.valid_from.to_string(),
            invalid_at: memory.invalid_at.map(|at| at.to_string()),
            invalid_reason: memory
                .invalid_reason
                .map(|reason| reason.as_str().to_owned()),
            supersedes: (!memory.supersedes.is_empty())
                .then(|| memory.supersedes.iter().map(ToString::to_string).collect()),
            superseded_by: memory.superseded_by.map(Into::into),
            created_at: memory.created_at.to_string(),
        }
    }
}

impl From<agmem_core::Episode> for EpisodeView {
    fn from(episode: agmem_core::Episode) -> Self {
        Self {
            id: episode.id.into(),
            space: episode.space.into(),
            chars: episode.content.chars().count(),
            content: episode.content,
            occurred_at: episode.occurred_at.to_string(),
            session: episode.session,
            created_at: episode.created_at.to_string(),
            title: episode.title,
            doc_kind: episode.doc_kind,
            tags: (!episode.tags.is_empty()).then_some(episode.tags),
            mime: episode.mime,
        }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(id) => write!(f, "memory:{id}"),
            Self::Episode(id) => write!(f, "episode:{id}"),
            Self::Entity(name) => write!(f, "entity:{name}"),
            Self::Doc { space, title } => write!(f, "doc:{space}/{title}"),
            Self::Docs(Some(space)) => write!(f, "docs:{space}"),
            Self::Docs(None) => f.write_str("docs"),
            Self::Stats => f.write_str("stats"),
            Self::Unqualified(id) => f.write_str(id),
        }
    }
}

/// A chain that does not contain the memory it was walked from is a store bug,
/// not a caller error.
fn unreachable_chain() -> ErrorData {
    internal("the history walk omitted the memory it started from")
}

/// A slice whose episode link names nothing is a store bug, not a caller
/// error.
fn unreachable_chunk() -> ErrorData {
    internal("a retrieval slice names an episode the store does not hold")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ULID: &str = "01M145SMNET1XRYA713EWAQTD3";

    #[test]
    fn the_ref_grammar_accepts_what_the_other_tools_hand_out() {
        let memory = Reference::Memory(MemoryId::new(ULID).expect("ulid"));
        assert_eq!(parse(&format!("memory:{ULID}")).expect("prefixed"), memory);
        assert_eq!(
            parse(ULID).expect("bare"),
            Reference::Unqualified(ULID.to_owned()),
            "`recall` hands out bare ids for claims and for verbatim slices \
             alike, so which table answers is a question for the store"
        );
        assert_eq!(
            parse(&format!(" episode:{ULID} ")).expect("episode"),
            Reference::Episode(EpisodeId::new(ULID).expect("ulid"))
        );
        assert_eq!(
            parse("entity:project-x").expect("entity"),
            Reference::Entity("project-x".to_owned())
        );
        assert_eq!(parse("stats").expect("stats"), Reference::Stats);
        assert_eq!(
            parse("doc:current/plans/phase-9").expect("doc"),
            Reference::Doc {
                space: "current".to_owned(),
                title: "plans/phase-9".to_owned()
            },
            "the first slash ends the space; a title may carry its own"
        );
        assert_eq!(parse("docs").expect("docs"), Reference::Docs(None));
        assert_eq!(
            parse("docs:proj-x").expect("docs in a space"),
            Reference::Docs(Some("proj-x".to_owned()))
        );
        assert!(
            parse("doc:all/plan").is_err(),
            "a title is looked up in one space"
        );
        assert!(parse("doc:current/").is_err(), "and needs a title");
        assert!(parse("docs:not a slug").is_err());
    }

    #[test]
    fn an_unparseable_ref_is_answered_with_the_grammar() {
        for bad in ["", "memory:nonsense", "episode:", "entity:", "who knows"] {
            let error = parse(bad).expect_err("refused");
            assert!(
                error.message.contains("ref must be"),
                "{bad:?} should be answered with the grammar, got: {}",
                error.message
            );
        }
    }

    #[test]
    fn a_reference_round_trips_through_its_canonical_form() {
        for raw in [
            "stats",
            "entity:user",
            ULID,
            &format!("memory:{ULID}"),
            &format!("episode:{ULID}"),
        ] {
            let parsed = parse(raw).expect("valid");
            assert_eq!(parsed.to_string(), raw, "canonical form is what is echoed");
            assert_eq!(parse(&parsed.to_string()).expect("re-parsed"), parsed);
        }
    }
}
