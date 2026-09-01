//! `forget` — the destructive verb (design §3.1, §5.4).
//!
//! Two things make forgetting safe enough to hand an agent. The first is that
//! it does not, by default, destroy anything: a forgotten memory is *closed*,
//! the same mechanism a correction uses, so it stops answering `recall` while
//! staying readable and dated through `inspect`. "We decided not to keep this"
//! and "this was never said" stay different states, which is what makes a
//! wrong forget recoverable.
//!
//! The second is that scope is confirmed by construction rather than by
//! convention. Forgetting by id is exact — the agent already named the rows.
//! Forgetting by *query* is not: it is a retrieval, and a retrieval's edges
//! are only visible once you look at them. So the first call must ask with
//! `dry_run: true` and get back the list, and only an identical second call
//! executes it. The confirmation lives on the service, which the daemon builds
//! per session — one agent's dry run can never authorise another's delete.

use std::sync::Mutex;

use agmem_core::{ChunkId, EpisodeId, MemoryId, MemoryRecord, SpaceName};
use agmem_store::StoreError;
use agmem_store::repo::{self, EpisodeDetail, Forget as StoreForget, Hit as StoreHit, Search};
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::service::AgmemService;
use crate::tools::{self, internal, invalid, store_error};

/// How much of a match a dry run shows before cutting it off.
///
/// A distilled claim is one sentence by contract, so this only ever bites on
/// an episode — where the point of the line is to recognise the text, not to
/// re-read it.
const PREVIEW_CHARS: usize = 300;

/// One `forget` call: what to remove, and how thoroughly.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ForgetParams {
    /// The ids to forget: `memory:<id>`, `episode:<id>`, or a bare id from a
    /// `recall` hit. Exact, and needs no dry run.
    #[serde(default)]
    pub ids: Vec<String>,

    /// What to forget, in words, when you do not have the ids. Matched on the
    /// words themselves, not on meaning — so it selects what you wrote rather
    /// than what resembles it. Refused unless the identical call has already
    /// been made with `dry_run: true`.
    #[serde(default)]
    pub query: Option<String>,

    /// Where to look: `current`, `user`, `all`, or a space name. Defaults to
    /// `current` and `user` together.
    #[serde(default)]
    pub space: Option<String>,

    /// Delete outright instead of closing. Unrecoverable: it takes the
    /// claim's whole correction history with it, and an episode's verbatim
    /// text and slices. Off by default.
    #[serde(default)]
    pub purge: bool,

    /// Report what would be forgotten and change nothing. Required as the
    /// first of two calls when forgetting by `query`.
    #[serde(default)]
    pub dry_run: bool,
}

/// What was selected, and what happened to it.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ForgetResult {
    /// The spaces this call looked in.
    pub spaces: Vec<String>,

    /// Whether this was the scope check rather than the act.
    pub dry_run: bool,

    /// Whether the rows were deleted rather than closed.
    pub purge: bool,

    /// Everything the request selected, whether or not it moved. A purge
    /// lists the correction history it pulls in, which is the blast radius
    /// the dry run exists to show.
    pub matched: Vec<ForgetMatch>,

    /// The memories closed by this call. Shorter than `matched` when
    /// something in it was already closed.
    pub invalidated: Vec<String>,

    /// The memories and episodes deleted by this call.
    pub purged: Vec<String>,

    /// How many episode slices went with the purged episodes.
    pub chunks_purged: usize,
}

/// One row a forget selected.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ForgetMatch {
    /// Its id. Pass it to `inspect` to read the whole row before deciding.
    pub id: String,

    /// What it is.
    pub kind: MatchKind,

    /// The claim, or the opening of the verbatim text, cut at 300 characters.
    pub content: String,

    /// The space holding it.
    pub space: String,

    /// Why it is already closed, when it is — `superseded`, `forgotten`, or
    /// `expired`. A soft forget leaves such a row exactly as it is, so this is
    /// what explains an id in `matched` that is missing from `invalidated`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,

    /// For an episode: how many distilled claims cite it and will outlive it.
    /// Purging text does not purge what was learned from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived: Option<usize>,
}

/// What a match is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum MatchKind {
    /// A distilled claim.
    Memory,
    /// Verbatim text.
    Episode,
}

/// The scope one `dry_run` offered, which the executing call must repeat.
///
/// `purge` is part of it deliberately: previewing what a soft forget would
/// close does not authorise deleting the same rows, and the two produce
/// different lists anyway once chains are expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    query: String,
    spaces: Vec<SpaceName>,
    purge: bool,
}

/// The one-slot record of what this session's last dry run offered: the
/// scope, and the ids it showed.
///
/// One slot rather than a set: a confirmation is for the call being made now,
/// and letting several accumulate would turn "confirm the scope" into "confirm
/// some scope, once, at some point". It is consumed on use, so executing the
/// same query twice means dry-running it twice — which is right, because the
/// second execution acts on a store the first one changed.
///
/// The ids travel with the scope (issue #66) because the scope alone confirms
/// the *question*, not the *answer*: a query re-run at confirm time selects
/// whatever matches now, and a row written between the two calls would be
/// closed — or purged — without ever having been previewed.
#[derive(Debug, Default)]
pub struct Pending(Mutex<Option<(Scope, Vec<String>)>>);

impl Pending {
    /// Record what this dry run offered, replacing anything older.
    fn arm(&self, scope: Scope, matched: Vec<String>) -> Result<(), ErrorData> {
        *self.0.lock().map_err(|_| poisoned())? = Some((scope, matched));
        Ok(())
    }

    /// Consume a confirmation for exactly this scope, or refuse. Returns the
    /// ids the dry run previewed — the only rows the caller may act on.
    fn confirm(&self, scope: &Scope) -> Result<Vec<String>, ErrorData> {
        let mut slot = self.0.lock().map_err(|_| poisoned())?;
        match slot.take() {
            Some((armed, matched)) if armed == *scope => Ok(matched),
            other => {
                *slot = other;
                Err(invalid(
                    "forgetting by query needs the same call with `dry_run: true` first — \
                     read what it matched, then send this call again unchanged",
                ))
            }
        }
    }
}

/// Run the forget path (design §5.4).
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS` for anything the caller can fix — no
/// selector or both of them, an id in no searched space, a query without its
/// dry run, an episode asked to be closed rather than purged — and
/// `INTERNAL_ERROR` for a failing embedder or store.
pub async fn run(service: &AgmemService, params: ForgetParams) -> Result<ForgetResult, ErrorData> {
    let ForgetParams {
        ids,
        query,
        space,
        purge,
        dry_run,
    } = params;
    let query = query
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty());
    let spaces = tools::spaces(service, space.as_deref()).await?;

    // 1. One selector, and it decides whether a confirmation is owed. Ids are
    //    already the answer to "which rows"; a query is a question about them.
    //    A confirmed query yields the dry run's id list — the snapshot this
    //    call is allowed to act on.
    let mut armable: Option<Scope> = None;
    let mut previewed: Option<Vec<String>> = None;
    let targets = match (ids.is_empty(), query) {
        (false, None) => by_ids(service, &spaces, &ids, purge).await?,
        (true, Some(text)) => {
            let scope = Scope {
                query: text.clone(),
                spaces: spaces.clone(),
                purge,
            };
            if dry_run {
                armable = Some(scope);
            } else {
                previewed = Some(service.pending_forget().confirm(&scope)?);
            }
            by_query(service, &spaces, &text).await?
        }
        (false, Some(_)) => {
            return Err(invalid(
                "give `ids` or `query`, not both — they answer the same question and \
                 a call that mixes them is not a scope anyone confirmed",
            ));
        }
        (true, None) => {
            return Err(invalid(
                "forget needs `ids` (exact) or `query` (with `dry_run: true` first)",
            ));
        }
    };

    // 2. A purge takes the whole correction chain: leaving the earlier
    //    versions of a claim behind would make "delete this" mean "delete the
    //    latest wording of this", which is not what anyone asks for.
    let targets = if purge {
        expand_chains(service, targets).await?
    } else {
        targets
    };

    // 2b. The snapshot discipline (issue #66): the scope confirmed the
    //     question, the id list is the answer that was actually read. A row
    //     matching now that the dry run never showed — written since, or
    //     pulled in by a chain that grew — stops the call before anything
    //     moves. Fewer rows than previewed is fine: everything acted on was
    //     seen. The confirmation is already spent, so the way forward is a
    //     fresh dry run against the store as it is now.
    if let Some(previewed) = &previewed {
        let unseen: Vec<&str> = targets
            .iter()
            .map(Target::id)
            .filter(|id| !previewed.iter().any(|seen| seen == id))
            .collect();
        if !unseen.is_empty() {
            return Err(invalid(format!(
                "the store changed since the dry run: {} matching row(s) were never \
                 previewed ({}). Nothing was forgotten; run the same call with \
                 `dry_run: true` again and read the fresh list.",
                unseen.len(),
                unseen.join(", ")
            )));
        }
    }

    let matched: Vec<ForgetMatch> = targets.iter().map(ForgetMatch::new).collect();
    let spaces_named: Vec<String> = spaces.iter().map(ToString::to_string).collect();
    if dry_run {
        if let Some(scope) = armable {
            let shown = targets
                .iter()
                .map(|target| target.id().to_owned())
                .collect();
            service.pending_forget().arm(scope, shown)?;
        }
        return Ok(ForgetResult {
            spaces: spaces_named,
            dry_run: true,
            purge,
            matched,
            invalidated: Vec::new(),
            purged: Vec::new(),
            chunks_purged: 0,
        });
    }

    let forgotten = repo::forget(
        service.db(),
        &StoreForget {
            spaces,
            memories: targets.iter().filter_map(Target::memory).collect(),
            episodes: targets.iter().filter_map(Target::episode).collect(),
            purge,
        },
    )
    .await
    .map_err(|error| store_error(&error))?;

    let (invalidated, purged) = if purge {
        let mut gone: Vec<String> = forgotten.memories.into_iter().map(Into::into).collect();
        gone.extend(forgotten.episodes.into_iter().map(String::from));
        (Vec::new(), gone)
    } else {
        (
            forgotten.memories.into_iter().map(Into::into).collect(),
            Vec::new(),
        )
    };
    Ok(ForgetResult {
        spaces: spaces_named,
        dry_run: false,
        purge,
        matched,
        invalidated,
        purged,
        chunks_purged: forgotten.chunks,
    })
}

/// One row a forget will act on, in the space that turned out to hold it.
#[derive(Debug)]
enum Target {
    /// A distilled claim.
    Memory {
        space: SpaceName,
        record: Box<MemoryRecord>,
    },
    /// Verbatim text, with what hangs off it.
    Episode {
        space: SpaceName,
        detail: Box<EpisodeDetail>,
    },
}

impl Target {
    /// Its id, when it is a memory.
    fn memory(&self) -> Option<MemoryId> {
        match self {
            Self::Memory { record, .. } => Some(record.id.clone()),
            Self::Episode { .. } => None,
        }
    }

    /// Its id, when it is an episode.
    fn episode(&self) -> Option<EpisodeId> {
        match self {
            Self::Episode { detail, .. } => Some(detail.episode.id.clone()),
            Self::Memory { .. } => None,
        }
    }

    /// The id as it is written, for deduplication.
    fn id(&self) -> &str {
        match self {
            Self::Memory { record, .. } => record.id.as_str(),
            Self::Episode { detail, .. } => detail.episode.id.as_str(),
        }
    }
}

/// Resolve every reference the caller named, refusing the first that misses.
///
/// A miss is refused rather than skipped: an agent that asked to forget four
/// things and is told three were forgotten has to work out which, and the one
/// that survived is the one that mattered.
async fn by_ids(
    service: &AgmemService,
    spaces: &[SpaceName],
    ids: &[String],
    purge: bool,
) -> Result<Vec<Target>, ErrorData> {
    let mut targets: Vec<Target> = Vec::with_capacity(ids.len());
    for raw in ids {
        let target = match parse(raw)? {
            Reference::Memory(id) => memory(service, spaces, &id)
                .await?
                .ok_or_else(|| missing_memory(id.as_str(), spaces))?,
            Reference::Episode(id) => {
                refuse_closing_text(&id, purge)?;
                episode_target(service, spaces, &id).await?.ok_or_else(|| {
                    invalid(format!(
                        "no episode {id} in {}; recall it first, or widen `space`",
                        names(spaces)
                    ))
                })?
            }
            Reference::Unqualified(id) => unqualified(service, spaces, &id, purge).await?,
        };
        if !targets.iter().any(|seen| seen.id() == target.id()) {
            targets.push(target);
        }
    }
    Ok(targets)
}

/// Everything a query matches, in retrieval order.
///
/// The literal arm only — no vector. `recall` wants what *resembles* the
/// question, and KNN obliges by always returning its nearest neighbours,
/// however far away they are; on a small store that is the whole store. As a
/// selector for deletion that is wrong in the unrecoverable direction, while
/// BM25 has the property this needs by construction: a row that does not
/// contain the words does not match at all. "Forget what I wrote" is a
/// question about words, and only `recall` is a question about meaning.
///
/// Verbatim chunks are left out (`episodes = false`) for the same reason: a
/// chunk is a slice, not a thing anyone forgets, and following one to its
/// episode would turn a term match into a delete of text nobody read.
/// Episodes are forgotten by id, deliberately.
async fn by_query(
    service: &AgmemService,
    spaces: &[SpaceName],
    text: &str,
) -> Result<Vec<Target>, ErrorData> {
    let mut search = Search::new(spaces.to_vec());
    search.text = Some(text.to_owned());
    search.episodes = false;
    search.pool = usize::from(service.config().pool);

    let candidates = repo::search_hybrid(service.db(), &search)
        .await
        .map_err(|error| store_error(&error))?;
    Ok(candidates
        .into_iter()
        .filter_map(|candidate| match candidate.hit {
            StoreHit::Memory(record) => Some(Target::Memory {
                space: record.space.clone(),
                record,
            }),
            StoreHit::Chunk(_) => None,
        })
        .collect())
}

/// Replace every memory target with its whole supersession chain, in order,
/// without repeating a row two targets share.
async fn expand_chains(
    service: &AgmemService,
    targets: Vec<Target>,
) -> Result<Vec<Target>, ErrorData> {
    let mut expanded: Vec<Target> = Vec::with_capacity(targets.len());
    for target in targets {
        match target {
            Target::Episode { .. } => expanded.push(target),
            Target::Memory { space, record } => {
                let chain = repo::history_chain(service.db(), &space, &record.id)
                    .await
                    .map_err(|error| store_error(&error))?;
                for link in chain {
                    if !expanded.iter().any(|seen| seen.id() == link.id.as_str()) {
                        expanded.push(Target::Memory {
                            space: space.clone(),
                            record: Box::new(link),
                        });
                    }
                }
            }
        }
    }
    Ok(expanded)
}

/// The claim an id names, in whichever searched space holds it.
async fn memory(
    service: &AgmemService,
    spaces: &[SpaceName],
    id: &MemoryId,
) -> Result<Option<Target>, ErrorData> {
    for space in spaces {
        match repo::history_chain(service.db(), space, id).await {
            Ok(chain) => {
                let record = chain
                    .into_iter()
                    .find(|link| link.id == *id)
                    .ok_or_else(|| internal("the chain walk lost the memory it started from"))?;
                return Ok(Some(Target::Memory {
                    space: space.clone(),
                    record: Box::new(record),
                }));
            }
            Err(StoreError::UnknownMemory { .. }) => continue,
            Err(error) => return Err(store_error(&error)),
        }
    }
    Ok(None)
}

/// Verbatim text, in whichever searched space holds it.
async fn episode_target(
    service: &AgmemService,
    spaces: &[SpaceName],
    id: &EpisodeId,
) -> Result<Option<Target>, ErrorData> {
    for space in spaces {
        match repo::episode(service.db(), space, id).await {
            Ok(detail) => {
                return Ok(Some(Target::Episode {
                    space: space.clone(),
                    detail: Box::new(detail),
                }));
            }
            Err(StoreError::UnknownEpisode { .. }) => continue,
            Err(error) => return Err(store_error(&error)),
        }
    }
    Ok(None)
}

/// Refuse to *close* an episode, before anything looks it up.
///
/// Verbatim text has no validity window — nothing about it can be true later
/// and false now — so there is no soft state for it to enter. Saying that is
/// better than silently doing nothing, and much better than treating it as a
/// licence to delete.
fn refuse_closing_text(id: &EpisodeId, purge: bool) -> Result<(), ErrorData> {
    if purge {
        return Ok(());
    }
    Err(invalid(format!(
        "episode:{id} is verbatim text and has no validity window to close; \
         use `purge: true` to delete it, or forget the claims distilled from it"
    )))
}

/// A bare id, resolved the way `recall` hands them out: memory, then episode,
/// then the slice that turns out to belong to one.
///
/// A chunk id is refused rather than followed. `inspect` resolves one to its
/// episode because reading the parent answers the question; deleting the
/// parent because a slice matched is a scope surprise, and this is the one
/// tool where a scope surprise is unrecoverable.
async fn unqualified(
    service: &AgmemService,
    spaces: &[SpaceName],
    id: &str,
    purge: bool,
) -> Result<Target, ErrorData> {
    let memory_id = MemoryId::new(id).map_err(|_| grammar(id))?;
    if let Some(target) = memory(service, spaces, &memory_id).await? {
        return Ok(target);
    }
    let episode_id = EpisodeId::new(id).map_err(|_| grammar(id))?;
    if let Some(target) = episode_target(service, spaces, &episode_id).await? {
        refuse_closing_text(&episode_id, purge)?;
        return Ok(target);
    }
    let chunk_id = ChunkId::new(id).map_err(|_| grammar(id))?;
    for space in spaces {
        let parent = repo::episode_of_chunk(service.db(), space, &chunk_id)
            .await
            .map_err(|error| store_error(&error))?;
        if let Some(parent) = parent {
            return Err(invalid(format!(
                "{id} is one slice of episode:{parent}; forget the episode to remove the \
                 text, or forget the claims distilled from it"
            )));
        }
    }
    Err(missing_memory(id, spaces))
}

/// What a `forget` id named.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reference {
    Memory(MemoryId),
    Episode(EpisodeId),
    /// A bare ULID, before the store has said which table answers to it.
    Unqualified(String),
}

/// An id, or a message that teaches the grammar.
fn parse(raw: &str) -> Result<Reference, ErrorData> {
    let raw = raw.trim();
    match raw.split_once(':') {
        Some(("memory", id)) => MemoryId::new(id)
            .map(Reference::Memory)
            .map_err(|_| grammar(raw)),
        Some(("episode", id)) => EpisodeId::new(id)
            .map(Reference::Episode)
            .map_err(|_| grammar(raw)),
        _ if MemoryId::new(raw).is_ok() => Ok(Reference::Unqualified(raw.to_owned())),
        _ => Err(grammar(raw)),
    }
}

/// The grammar, as something to act on rather than guess at.
fn grammar(raw: &str) -> ErrorData {
    invalid(format!(
        "each id must be `memory:<id>`, `episode:<id>`, or a bare id; got {raw:?}"
    ))
}

/// Nothing in any searched space answers to this id.
fn missing_memory(id: &str, spaces: &[SpaceName]) -> ErrorData {
    invalid(format!(
        "no memory or episode {id} in {}; recall it first, or widen `space`",
        names(spaces)
    ))
}

/// The searched spaces, as the caller would write them.
fn names(spaces: &[SpaceName]) -> String {
    spaces
        .iter()
        .map(SpaceName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The confirmation slot is only ever swapped, so this needs a panic in
/// another tool to happen at all — but a poisoned lock must not read as "no
/// confirmation was pending", which is the one failure that loosens the gate.
fn poisoned() -> ErrorData {
    internal("the forget confirmation lock was poisoned by an earlier panic")
}

impl ForgetMatch {
    /// One resolved target as the agent sees it before deciding.
    fn new(target: &Target) -> Self {
        match target {
            Target::Memory { space, record } => Self {
                id: record.id.as_str().to_owned(),
                kind: MatchKind::Memory,
                content: preview(&record.content),
                space: space.as_str().to_owned(),
                invalid_reason: record
                    .invalid_reason
                    .map(|reason| reason.as_str().to_owned()),
                derived: None,
            },
            Target::Episode { space, detail } => Self {
                id: detail.episode.id.as_str().to_owned(),
                kind: MatchKind::Episode,
                content: preview(&detail.episode.content),
                space: space.as_str().to_owned(),
                invalid_reason: None,
                derived: Some(detail.derived.len()),
            },
        }
    }
}

/// Enough of a row to recognise it, cut on a character boundary.
fn preview(text: &str) -> String {
    match text.char_indices().nth(PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(query: &str, purge: bool) -> Scope {
        Scope {
            query: query.to_owned(),
            spaces: vec![SpaceName::user()],
            purge,
        }
    }

    #[test]
    fn a_confirmation_is_spent_by_the_call_it_authorises() {
        let pending = Pending::default();
        let asked = scope("old notes", false);
        assert!(
            pending.confirm(&asked).is_err(),
            "an unarmed gate refuses everything"
        );

        pending
            .arm(asked.clone(), vec!["01A".to_owned()])
            .expect("arm");
        assert_eq!(
            pending.confirm(&asked).expect("the same call goes through"),
            ["01A"],
            "the confirmation hands back exactly what the dry run showed"
        );
        assert!(
            pending.confirm(&asked).is_err(),
            "a confirmation authorises one call, not a standing licence"
        );
    }

    #[test]
    fn a_dry_run_does_not_authorise_a_different_call() {
        let pending = Pending::default();
        pending
            .arm(scope("old notes", false), Vec::new())
            .expect("arm");
        assert!(
            pending.confirm(&scope("old notes", true)).is_err(),
            "previewing what would be closed does not authorise deleting it"
        );
        assert!(
            pending.confirm(&scope("older notes", false)).is_err(),
            "a different query is a different scope"
        );
        pending
            .confirm(&scope("old notes", false))
            .expect("the refusals left the confirmation standing");
    }

    #[test]
    fn the_id_grammar_names_what_it_accepts() {
        let ulid = "01K3ZQ8V9WXYZABCDEFGHJKMNP";
        assert_eq!(
            parse(&format!("memory:{ulid}")).expect("memory ref"),
            Reference::Memory(MemoryId::new(ulid).expect("ulid"))
        );
        assert_eq!(
            parse(&format!("  episode:{ulid} ")).expect("episode ref"),
            Reference::Episode(EpisodeId::new(ulid).expect("ulid"))
        );
        assert_eq!(
            parse(ulid).expect("bare id"),
            Reference::Unqualified(ulid.to_owned())
        );
        assert!(
            parse("entity:alice")
                .expect_err("not a forgettable row")
                .message
                .contains("memory:<id>")
        );
    }

    #[test]
    fn a_preview_cuts_text_but_never_a_claim() {
        let claim = "the user prefers Rust over Python for CLI tools";
        assert_eq!(preview(claim), claim);

        let long: String = "é".repeat(PREVIEW_CHARS + 10);
        let cut = preview(&long);
        assert!(cut.ends_with('…'));
        assert_eq!(
            cut.chars().count(),
            PREVIEW_CHARS + 1,
            "the cut lands on a character, not inside one"
        );
    }
}
