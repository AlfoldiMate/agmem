//! `recall` — the read verb (design §3.1, §5.3).
//!
//! Retrieval is two halves that meet here. SurrealDB answers "what matched",
//! fusing BM25 and vector arms over memories and episode chunks into one
//! ranked pool; `core::scoring` answers "what still matters", decaying each
//! candidate from the clock at the moment of the call. Neither half is
//! authoritative alone, which is why the store deliberately does not rescore
//! and this module deliberately does not query.
//!
//! Every hit carries the signals behind it. An agent that can see *why*
//! something surfaced — matched well, or merely survived well — can discount
//! it; an agent handed a bare list of sentences cannot. That is also what
//! makes a bad memory diagnosable instead of mysterious.

use std::sync::Arc;

use agmem_core::scoring::{self, Ranked, Signals};
use agmem_core::{Kind, MemoryId};
use agmem_store::repo::{self, Candidate, Filters, Hit as StoreHit, Liveness, Lookup, Search};
use jiff::Timestamp;
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::service::AgmemService;
use crate::tools::{self, internal, invalid, provenance, store_error};

/// How many hits a call that does not say gets back.
const DEFAULT_K: u16 = 10;

/// One `recall` call: what to look for, and how to narrow it.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RecallParams {
    /// What you want to know, in words — a question or a topic, not keywords.
    /// Both halves of retrieval use it: the wording matches literally, the
    /// meaning matches semantically. Leave it out to list what the filters
    /// alone select, strongest first.
    #[serde(default)]
    pub query: Option<String>,

    /// How many hits to return. Defaults to 10.
    #[serde(default)]
    pub k: Option<u16>,

    /// Where to look: `current` for this project, `user` for the person,
    /// `all` for every space, or a space name. Defaults to `current` and
    /// `user` together, which is almost always what you want.
    #[serde(default)]
    pub space: Option<String>,

    /// Keep only these kinds.
    #[serde(default)]
    pub kinds: Vec<Kind>,

    /// Keep only memories about one of these subjects.
    #[serde(default)]
    pub entities: Vec<String>,

    /// Keep only memories carrying one of these labels.
    #[serde(default)]
    pub tags: Vec<String>,

    /// What was believed at this instant, RFC3339 — corrections are dated, so
    /// this returns the claim that was live then rather than the one that
    /// replaced it.
    #[serde(default)]
    pub as_of: Option<String>,

    /// Include claims that have since been corrected or forgotten. Off by
    /// default: a closed claim is history, not an answer.
    #[serde(default)]
    pub include_invalidated: bool,
}

/// What was found, best first.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RecallResult {
    /// The spaces this call actually searched.
    pub spaces: Vec<String>,

    /// The matches, highest `score` first.
    pub hits: Vec<RecallHit>,
}

/// One match, with the reasoning behind its place in the order.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RecallHit {
    /// The id of this memory, to correct it with `remember`'s `supersedes` or
    /// to look it up with `inspect`.
    pub id: String,

    /// What this is: a distilled claim, or `episode` for a slice of the
    /// verbatim text one was distilled from.
    pub kind: HitKind,

    /// The claim, or the verbatim slice.
    pub content: String,

    /// The space it came from.
    pub space: String,

    /// The final rank, combining everything in `signals`. Comparable within
    /// one call and not across calls.
    pub score: f64,

    /// Why it surfaced.
    pub signals: HitSignals,

    /// Where it came from: `agent`, `episode:<id>`, or `external:<origin>`.
    /// Pass it to `inspect` to read the text behind a distilled claim.
    pub source: String,

    /// The subjects this claim is about.
    pub entities: Vec<String>,

    /// Its labels.
    pub tags: Vec<String>,

    /// When the claim started being true, RFC3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,

    /// When it stopped being true; absent while it is still live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_at: Option<String>,

    /// Why it stopped: `superseded`, `forgotten`, or `expired`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,

    /// The id of the claim that replaced this one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

/// What a hit is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HitKind {
    /// A distilled statement about the world, the user, or the project.
    Fact,
    /// A procedural insight: "X fails when Y; do Z".
    Lesson,
    /// A standing behavioral rule.
    Instruction,
    /// A slice of verbatim text, stored unedited as ground truth.
    Episode,
}

/// The signals behind one hit's `score`, all in `[0, 1]` except `rrf`.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct HitSignals {
    /// How well retrieval matched, fused across the text and vector arms. It
    /// has no fixed scale; compare `rrf_normalized` instead.
    pub rrf: f64,

    /// `rrf` on a 0–1 scale against the rest of this call: 1.0 for the
    /// strongest retrieval hit, 0.0 for the weakest one returned — or 0.0
    /// everywhere when nothing matched on words or meaning at all.
    pub rrf_normalized: f64,

    /// How much of the memory has survived its decay curve since it was last
    /// used. 1.0 for a pinned memory, or for verbatim text.
    pub retention: f64,

    /// The standing importance of its decay class — what keeps a rarely
    /// matched instruction ahead of a well matched scratch note.
    pub importance: f64,
}

/// Run the read path (design §5.3).
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS` for anything the caller can fix — a
/// `k` past the configured ceiling, a bad space name, an unparseable `as_of`
/// — and `INTERNAL_ERROR` for a failing embedder or store.
pub async fn run(service: &AgmemService, params: RecallParams) -> Result<RecallResult, ErrorData> {
    let RecallParams {
        query,
        k,
        space,
        kinds,
        entities,
        tags,
        as_of,
        include_invalidated,
    } = params;

    let spaces = tools::spaces(service, space.as_deref()).await?;
    let k = usize::from(resolve_k(service, k)?);
    let liveness = resolve_liveness(as_of.as_deref(), include_invalidated)?;
    let filters = Filters {
        kinds,
        entities,
        tags,
    };
    let pool = usize::from(service.config().pool);

    // 1. A call with nothing to match on is not a search — it is the tier-1
    //    indexed lookup, which costs no embedding and no fusion (§5.3 step 1).
    let candidates = match query.filter(|text| !text.trim().is_empty()) {
        Some(text) => {
            let mut search = Search::new(spaces.clone());
            search.vector = embed_query(service, &text).await?;
            search.text = Some(text);
            search.filters = filters;
            search.liveness = liveness;
            search.pool = pool;
            repo::search_hybrid(service.db(), &search)
                .await
                .map_err(|error| store_error(&error))?
        }
        None => {
            let mut lookup = Lookup::new(spaces.clone());
            lookup.filters = filters;
            lookup.liveness = liveness;
            lookup.limit = pool;
            repo::direct_lookup(service.db(), &lookup)
                .await
                .map_err(|error| store_error(&error))?
                .into_iter()
                .map(|memory| Candidate {
                    rrf: 0.0,
                    hit: StoreHit::Memory(Box::new(memory)),
                })
                .collect()
        }
    };

    // 2. Rescore in Rust. Decay is read from the clock, so a candidate the
    //    engine ranked first can still lose to one that has been used since.
    let now = Timestamp::now();
    let ranked: Vec<(StoreHit, Ranked)> = scoring::rank(candidates.into_iter().map(|candidate| {
        let signals = match &candidate.hit {
            StoreHit::Memory(memory) => Signals::for_memory(candidate.rrf, memory, now),
            StoreHit::Chunk(_) => Signals::for_episode_chunk(candidate.rrf),
        };
        (candidate.hit, signals)
    }))
    .into_iter()
    .take(k)
    .collect();

    // 3. Being recalled is what keeps a memory alive; verbatim text has no
    //    curve to be pushed back up.
    let touched: Vec<MemoryId> = ranked
        .iter()
        .filter_map(|(hit, _)| match hit {
            StoreHit::Memory(memory) => Some(memory.id.clone()),
            StoreHit::Chunk(_) => None,
        })
        .collect();
    reinforce(service, &touched).await;

    Ok(RecallResult {
        spaces: spaces.iter().map(ToString::to_string).collect(),
        hits: ranked
            .into_iter()
            .map(|(hit, ranked)| RecallHit::new(hit, &ranked))
            .collect(),
    })
}

impl RecallHit {
    /// One store hit as the agent sees it.
    fn new(hit: StoreHit, ranked: &Ranked) -> Self {
        let signals = HitSignals {
            rrf: ranked.signals.rrf,
            rrf_normalized: ranked.rrf_normalized,
            retention: ranked.signals.retention,
            importance: ranked.signals.importance,
        };
        match hit {
            StoreHit::Memory(memory) => {
                let memory = *memory;
                Self {
                    id: memory.id.into(),
                    kind: memory.kind.into(),
                    content: memory.content,
                    space: memory.space.into(),
                    score: ranked.score,
                    signals,
                    source: provenance(&memory.source),
                    entities: memory.entities,
                    tags: memory.tags,
                    valid_from: Some(memory.valid_from.to_string()),
                    invalid_at: memory.invalid_at.map(|at| at.to_string()),
                    invalid_reason: memory
                        .invalid_reason
                        .map(|reason| reason.as_str().to_owned()),
                    superseded_by: memory.superseded_by.map(Into::into),
                }
            }
            StoreHit::Chunk(chunk) => Self {
                id: chunk.id.into(),
                kind: HitKind::Episode,
                content: chunk.text,
                space: chunk.space.into(),
                score: ranked.score,
                signals,
                source: format!("episode:{}", chunk.episode),
                entities: Vec::new(),
                tags: Vec::new(),
                valid_from: None,
                invalid_at: None,
                invalid_reason: None,
                superseded_by: None,
            },
        }
    }
}

impl From<Kind> for HitKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Fact => Self::Fact,
            Kind::Lesson => Self::Lesson,
            Kind::Instruction => Self::Instruction,
        }
    }
}

/// How many hits to return, refused rather than silently clamped.
///
/// A `k` of 200 against a ceiling of 50 is a caller that believes it is
/// getting 200; truncating that quietly turns a configuration limit into a
/// wrong answer.
fn resolve_k(service: &AgmemService, requested: Option<u16>) -> Result<u16, ErrorData> {
    let max = service.config().max_k;
    let k = requested.unwrap_or_else(|| DEFAULT_K.min(max));
    if k == 0 || k > max {
        return Err(invalid(format!("k must be between 1 and {max}")));
    }
    Ok(k)
}

/// The validity window this call reads through.
///
/// `as_of` is the more specific of the two and wins: asking what was believed
/// at an instant already includes the claims that have been corrected since,
/// and only the ones that were live then.
fn resolve_liveness(as_of: Option<&str>, include_invalidated: bool) -> Result<Liveness, ErrorData> {
    match as_of {
        Some(stamp) => Ok(Liveness::AsOf(
            stamp
                .parse()
                .map_err(|error| invalid(format!("as_of: {error}")))?,
        )),
        None if include_invalidated => Ok(Liveness::Any),
        None => Ok(Liveness::Live),
    }
}

/// The query vector for the semantic arms, or `None` in BM25-only mode.
async fn embed_query(service: &AgmemService, text: &str) -> Result<Option<Vec<f32>>, ErrorData> {
    if service.embedder().dim() == 0 {
        return Ok(None);
    }
    agmem_embed::embed_query(Arc::clone(service.embedder()), text.to_owned())
        .await
        .map(Some)
        .map_err(|error| internal(format!("embedding the query failed: {error}")))
}

/// Push every memory this call returned back up its decay curve (§5.3 step 5).
///
/// Fire-and-forget in the sense that matters: a failure is logged and dropped
/// rather than turned into a failed recall — the agent has its hits either
/// way, and a lost reinforcement costs nothing but a slightly faster fade. It
/// is awaited rather than detached because it is one local `UPDATE`, and a
/// spawned task would make "was this reinforced" untestable.
async fn reinforce(service: &AgmemService, ids: &[MemoryId]) {
    if let Err(error) = repo::reinforce(service.db(), ids).await {
        tracing::warn!(%error, "reinforcement failed; the hits are unaffected");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_of_outranks_include_invalidated() {
        let instant: Timestamp = "2026-08-28T09:00:00Z".parse().expect("timestamp");
        for include in [false, true] {
            assert_eq!(
                resolve_liveness(Some("2026-08-28T09:00:00Z"), include).expect("valid"),
                Liveness::AsOf(instant),
                "a point in time already says which claims to include"
            );
        }
        assert_eq!(
            resolve_liveness(None, false).expect("valid"),
            Liveness::Live
        );
        assert_eq!(resolve_liveness(None, true).expect("valid"), Liveness::Any);
        assert!(
            resolve_liveness(Some("last tuesday"), false)
                .expect_err("not RFC3339")
                .message
                .contains("as_of")
        );
    }
}
