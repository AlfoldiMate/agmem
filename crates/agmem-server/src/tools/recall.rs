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

use agmem_core::scoring::{self, Ranked, Signals};
use agmem_core::{Kind, MemoryId};
use agmem_store::repo::{self, Candidate, Filters, Hit as StoreHit, Liveness, Lookup, Search};
use jiff::Timestamp;
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::service::AgmemService;
use crate::tools::{self, abstain, embed_query, hop, invalid, occupancy, provenance, store_error};

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
    /// replaced it. Verbatim text is dated too: an episode recorded after
    /// the instant stays out of the answer.
    #[serde(default)]
    pub as_of: Option<String>,

    /// Rank claims still true at or after this instant (RFC3339) ahead of
    /// the rest. A soft window, not a filter: nothing is hidden, and each
    /// hit reports its fit in `signals.temporal`. Combine with `until` for
    /// a range; use `as_of` instead when you want one instant's hard truth.
    #[serde(default)]
    pub since: Option<String>,

    /// Rank claims already true at or before this instant (RFC3339) ahead
    /// of the rest. Soft, like `since`. Note that an all-past window over a
    /// live read cannot surface claims corrected since — add
    /// `include_invalidated: true` to reach those.
    #[serde(default)]
    pub until: Option<String>,

    /// Rank claims created or corrected at or after this instant (RFC3339)
    /// ahead of the rest — "what changed since I last looked". Soft, like
    /// `since`.
    #[serde(default)]
    pub changed_since: Option<String>,

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

    /// Present when the per-source occupancy cap changed this page: some
    /// source had more strong hits than one source may hold, and the surplus
    /// yielded its slots to the next-ranked hits from elsewhere. Absent means
    /// the page is exactly the ranking, uncapped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capped: Option<Capped>,

    /// Present when this answer filled `k` and the filters select more claims
    /// than it carries — so what came back is a page. Absent means `k` did not
    /// cut anything short: either the page never filled, or nothing beyond it
    /// matches these filters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<Truncated>,

    /// Present when match quality changed this page (issue #77): either the
    /// tail fell off a marked drop in retrieval quality, or — `kept: 0` —
    /// nothing matched well enough to answer at all and the page is honestly
    /// empty. Absent means every hit earned its slot on ranking alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cut: Option<Cut>,

    /// Present when the call carried a temporal window (issue #78): what
    /// was asked, how well the page fits it, and what a soft window cannot
    /// do. Absent on every call without `since`/`until`/`changed_since`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dated: Option<Dated>,
}

/// What the temporal window did to this page (issue #78).
#[derive(Debug, Serialize, JsonSchema)]
pub struct Dated {
    /// The window as asked, echoed back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<String>,

    /// The best temporal fit on the page, absent when nothing was datable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_fit: Option<f64>,

    /// The same thing in words.
    pub note: String,
}

/// What the occupancy cap moved out of the page (issue #76).
///
/// A page one source dominates reads exactly like a page many sources agree
/// on, so the cut is admitted here the way `truncated` admits `k`'s.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Capped {
    /// The most hits of this page any single source may hold.
    pub cap: usize,

    /// How many over-quota hits left the page for lower-ranked ones from
    /// other sources.
    pub displaced: usize,

    /// The sources that were over quota — the same `episode:<id>` or
    /// `external:<origin>` strings the hits carry, ready for `inspect`.
    pub sources: Vec<String>,

    /// The same thing in words.
    pub note: String,
}

impl Capped {
    fn new(cap: usize, resliced: occupancy::Resliced) -> Self {
        let sources = resliced.sources.join(", ");
        let note = format!(
            "{displaced} strong hit(s) from {sources} were moved off this page: no single \
             source may hold more than {cap} of its slots, so the surplus yielded to the \
             next-ranked hits from elsewhere. Raise `k`, or `inspect` the source, to see \
             what was deferred.",
            displaced = resliced.displaced,
        );
        Self {
            cap,
            displaced: resliced.displaced,
            sources: resliced.sources,
            note,
        }
    }
}

/// What match quality did to the page (issue #77).
///
/// The struct-with-note shape [`Capped`] and [`Truncated`] already have: the
/// numbers are what make the note checkable.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Cut {
    /// How many hits survived; 0 is the abstention — nothing matched well
    /// enough to act on, which is not the same as nothing being stored.
    pub kept: usize,

    /// How many hits the page held before the cut.
    pub considered: usize,

    /// The best cosine similarity a vector arm measured on the page, absent
    /// when nothing was measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub best_similarity: Option<f64>,

    /// The same thing in words, with the next move.
    pub note: String,
}

impl Cut {
    fn new(verdict: abstain::Verdict) -> Self {
        let abstain::Verdict {
            kept,
            considered,
            best_similarity,
        } = verdict;
        let note = if kept == 0 {
            let best = best_similarity
                .map_or_else(|| "nothing measurable".to_owned(), |s| format!("{s:.2}"));
            format!(
                "Nothing here matched well enough to answer: the best of {considered} \
                 candidate(s) was {best} similar to the query. That is not an empty store — \
                 it is a page with nothing on it worth acting on. Ask in different words, or \
                 drop `query` and use `entities`/`tags` to list what is stored."
            )
        } else {
            format!(
                "{kept} of {considered} candidates are returned: the rest fell off a marked \
                 drop in match quality, not off `k`. Raising `k` will not bring them back; a \
                 filters-only call lists what is there."
            )
        };
        Self {
            kept,
            considered,
            best_similarity,
            note,
        }
    }
}

/// What a `k` left behind.
///
/// A page of hits looks exactly like a whole store: fifty out of fifty and
/// fifty out of five hundred serialise identically, so an agent that reads a
/// page and reports what memory holds is right by luck. This says which of
/// the two happened, in the answer, at the moment it matters.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Truncated {
    /// How many live claims the filters select across the searched spaces,
    /// ignoring `query` and `k`. Relevance is not counted here: this is the
    /// size of the set the ranking chose from, not of what matched well.
    pub matching_claims: u64,

    /// How many of them this answer carries.
    pub returned_claims: usize,

    /// The `k` that did the cutting.
    pub k: usize,

    /// The same thing in words, because a number is only acted on when
    /// something says why it matters.
    pub note: String,
}

impl Truncated {
    /// Only ever built when the count exceeds what came back.
    fn new(matching_claims: u64, returned_claims: usize, k: usize) -> Self {
        let note = format!(
            "These are the {returned_claims} strongest of {matching_claims} live claims these \
             filters select — a ranked page, which reads exactly like a whole store. Raise `k` \
             to see more of it. Do not audit memory from this: judging what is duplicated, \
             contradicted or stale needs every claim compared against every other, which is \
             what `consolidate` does and no page can."
        );
        Self {
            matching_claims,
            returned_claims,
            k,
            note,
        }
    }
}

/// One match, with the reasoning behind its place in the order.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RecallHit {
    /// The id of this hit. Pass it to `inspect` to see where it came from, or
    /// to `remember`'s `supersedes` to correct it — the latter only for a claim,
    /// since an `episode` hit is a slice of verbatim text and nothing to
    /// correct.
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

    /// Cosine similarity between the query and this hit, when a vector arm
    /// measured it — the one signal here with an absolute scale, comparable
    /// across calls. Absent for a hit only the text arms or the entity hop
    /// surfaced, and everywhere in a BM25-only deployment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f64>,

    /// How well this hit fits the `since`/`until`/`changed_since` window the
    /// call carried, in `[0, 1]` — 1.0 for any overlap, decaying with the
    /// distance outside it. Absent when the call had no window, or for a hit
    /// with no date to judge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temporal: Option<f64>,

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
        since,
        until,
        changed_since,
        include_invalidated,
    } = params;

    let spaces = tools::spaces(service, space.as_deref()).await?;
    let k = usize::from(resolve_k(service, k)?);
    let liveness = resolve_liveness(as_of.as_deref(), include_invalidated)?;
    let window = resolve_window(since.as_deref(), until.as_deref(), changed_since.as_deref())?;
    let filters = Filters {
        kinds,
        entities,
        tags,
    };
    // The search consumes the filters; the count that follows needs the same
    // ones, or it would answer a different question than the one that was cut.
    let counted = filters.clone();
    let pool = usize::from(service.config().pool);

    // 1. A call with nothing to match on is not a search — it is the tier-1
    //    indexed lookup, which costs no embedding and no fusion (§5.3 step 1).
    let query = query.filter(|text| !text.trim().is_empty());
    let is_search = query.is_some();
    let mut hopped = Vec::new();
    let candidates = match query {
        Some(text) => {
            let mut search = Search::new(spaces.clone());
            search.vector = embed_query(service, &text).await?;
            search.text = Some(text);
            search.filters = filters;
            search.liveness = liveness;
            search.pool = pool;
            let mut candidates = repo::search_hybrid(service.db(), &search)
                .await
                .map_err(|error| store_error(&error))?;
            // 1b. Follow the hits' own entities one hop (§5.3 step 3b). The
            //     row a chain question needs rarely matches the question's
            //     words, and no agent makes the second, filtered call that
            //     would fetch it — so this answer carries it instead.
            hopped = hop::run(service, &spaces, &search.filters, liveness, &mut candidates).await;
            candidates
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
                    similarity: None,
                    hit: StoreHit::Memory(Box::new(memory)),
                })
                .collect()
        }
    };

    // 2. Rescore in Rust. Decay is read from the clock, so a candidate the
    //    engine ranked first can still lose to one that has been used since.
    let now = Timestamp::now();
    let mut ranked: Vec<(StoreHit, Ranked)> =
        scoring::rank(candidates.into_iter().map(|candidate| {
            // The temporal fit rides at [`scoring::WEIGHT_TEMPORAL`] only
            // when a window was asked for; a chunk is an event at its
            // `occurred_at`, and one without a date stays out of the term
            // entirely — undatable is not unfitting.
            let temporal = match (&window, &candidate.hit) {
                (Some(query), StoreHit::Memory(memory)) => Some(scoring::temporal_fit(
                    query,
                    scoring::Interval {
                        from: memory.valid_from,
                        to: memory.invalid_at,
                    },
                    memory
                        .invalid_at
                        .map_or(memory.created_at, |at| at.max(memory.created_at)),
                )),
                (Some(query), StoreHit::Chunk(chunk)) => chunk.occurred_at.map(|at| {
                    scoring::temporal_fit(
                        query,
                        scoring::Interval {
                            from: at,
                            to: Some(at),
                        },
                        at,
                    )
                }),
                (None, _) => None,
            };
            let signals = match &candidate.hit {
                StoreHit::Memory(memory) => Signals::for_memory(candidate.rrf, memory, now),
                StoreHit::Chunk(_) => Signals::for_episode_chunk(candidate.rrf),
            }
            .with_similarity(candidate.similarity)
            .with_temporal(temporal);
            (candidate.hit, signals)
        }));

    // The page as pure ranking would have cut it, before any policy touches
    // it. The knee trim (2d) may only cut rows that earned their slot by
    // score; a row the occupancy cap promoted was placed *because* it ranks
    // lower than the page around it, and the rank skip it sits on reads as
    // exactly the cliff the knee looks for — trimming it would undo the
    // policy that put it there. Hop-voted rows are exempt by name for the
    // same reason (issue #43): the arm is weak by design, so its row is the
    // knee's natural first victim, full page or not.
    let by_score: Vec<String> = ranked
        .iter()
        .take(k)
        .map(|(hit, _)| hit_key(hit).to_owned())
        .collect();

    // 2b. No single source may flood the page (issue #76): over-quota hits
    //     defer to the next-ranked ones from elsewhere. Before the hop's
    //     tail reserve on purpose — the hop may then promote one hop-voted
    //     row back over quota, which is bounded (one row, one source) where
    //     capping last would re-create the miss the hop exists to fix.
    let page_cap = occupancy::cap(k);
    let capped = occupancy::apply(&mut ranked, k, page_cap, |(hit, _)| match hit {
        StoreHit::Memory(memory) => match &memory.source {
            agmem_core::Source::Agent => None,
            source => Some(provenance(source)),
        },
        StoreHit::Chunk(chunk) => Some(format!("episode:{}", chunk.episode)),
    })
    .map(|resliced| Capped::new(page_cap, resliced));

    // 2c. `take(k)` cuts at exactly the depth the hop arm's weakness leaves
    //     its rows at, and the row was fetched precisely to be seen — so a
    //     full page that would carry no hop-voted row gives its last slot to
    //     the best one below the cut (issue #43).
    hop::reserve_tail(
        &mut ranked,
        k,
        |(hit, _)| matches!(hit, StoreHit::Memory(memory) if hopped.contains(&memory.id)),
    );
    ranked.truncate(k);
    let considered = ranked.len();

    // 2d. Cut what did not really match (issue #77): the knee trims the tail
    //     that merely ranked, and the floor empties a page whose best
    //     measured hit is not an answer. After the promotions, with the
    //     promoted rows exempt from the trim (`by_score`) though not from
    //     abstention; before reinforcement, because a row cut off the page
    //     was not recalled. The filters-only path asked for a listing, not a
    //     search, and is never cut.
    let cut = if is_search {
        abstain::apply(
            &mut ranked,
            |(_, ranked)| (ranked.signals.similarity, ranked.rrf_normalized),
            |(hit, _)| {
                !by_score.iter().any(|id| id == hit_key(hit))
                    || matches!(hit, StoreHit::Memory(memory) if hopped.contains(&memory.id))
            },
        )
        .map(Cut::new)
    } else {
        None
    };
    let abstained = cut.as_ref().is_some_and(|cut| cut.kept == 0);
    // An abstention empties the page; `capped` and `truncated` both describe
    // a page that no longer exists.
    let capped = if abstained { None } else { capped };

    // 3. Being recalled is what keeps a memory alive; verbatim text has no
    //    curve to be pushed back up. Historical reads don't count (issue #63):
    //    an `as_of` or `include_invalidated` call asks what *was* believed,
    //    and a read that mutates present ranking state answers a different
    //    question than it was asked — the same reasoning that keeps `context`
    //    from reinforcing on a schedule.
    let touched: Vec<MemoryId> = ranked
        .iter()
        .filter_map(|(hit, _)| match hit {
            StoreHit::Memory(memory) => Some(memory.id.clone()),
            StoreHit::Chunk(_) => None,
        })
        .collect();
    if liveness == Liveness::Live {
        reinforce(service, &touched).await;
    }

    // 4. `take(k)` is silent, and a full page is the one shape that cannot be
    //    read as complete or partial from the outside. Count only when the
    //    page filled up: a short answer is its own evidence of being whole.
    let truncated = if considered == k && !abstained {
        let mut lookup = Lookup::new(spaces.clone());
        lookup.filters = counted;
        lookup.liveness = liveness;
        let matching = repo::count_matching(service.db(), &lookup)
            .await
            .map_err(|error| store_error(&error))?;
        let returned = touched.len();
        (matching > returned as u64).then(|| Truncated::new(matching, returned, k))
    } else {
        None
    };

    let dated = window.map(|_| {
        let best_fit = ranked
            .iter()
            .filter_map(|(_, ranked)| ranked.signals.temporal)
            .max_by(f64::total_cmp);
        Dated::new(since, until, changed_since, best_fit, liveness)
    });

    Ok(RecallResult {
        spaces: spaces.iter().map(ToString::to_string).collect(),
        hits: ranked
            .into_iter()
            .map(|(hit, ranked)| RecallHit::new(hit, &ranked))
            .collect(),
        capped,
        truncated,
        cut,
        dated,
    })
}

impl Dated {
    fn new(
        since: Option<String>,
        until: Option<String>,
        changed_since: Option<String>,
        best_fit: Option<f64>,
        liveness: Liveness,
    ) -> Self {
        let mut note = "The window rescores rather than filters: hits fitting it rank higher, \
                        nothing is hidden for missing it, and each hit's own fit is in \
                        `signals.temporal`."
            .to_owned();
        // The one thing a soft window cannot do on a live read: a claim
        // corrected since the window closed is off the page before ranking
        // ever sees it. Say so exactly when it applies.
        let past = until
            .as_deref()
            .and_then(|stamp| stamp.parse::<Timestamp>().ok())
            .is_some_and(|instant| instant < Timestamp::now());
        if past && liveness == Liveness::Live {
            note.push_str(
                " This read is live-only, so a claim corrected since then is not on the page \
                 however well it fits — pass `include_invalidated: true`, or `as_of`, to reach \
                 what was true then.",
            );
        }
        Self {
            since,
            until,
            changed_since,
            best_fit,
            note,
        }
    }
}

impl RecallHit {
    /// One store hit as the agent sees it.
    fn new(hit: StoreHit, ranked: &Ranked) -> Self {
        let signals = HitSignals {
            rrf: ranked.signals.rrf,
            rrf_normalized: ranked.rrf_normalized,
            similarity: ranked.signals.similarity,
            temporal: ranked.signals.temporal,
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

/// The id a hit answers to, whichever table it lives in.
fn hit_key(hit: &StoreHit) -> &str {
    match hit {
        StoreHit::Memory(memory) => memory.id.as_str(),
        StoreHit::Chunk(chunk) => chunk.id.as_str(),
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

/// The soft temporal window this call ranks through (issue #78), `None`
/// when no temporal parameter was given — the everyday path, whose scoring
/// arithmetic must stay untouched.
///
/// The one refusal is an inverted range: a caller who sent `since > until`
/// believes the window means something, and no ranking of an empty
/// intersection would honour it.
fn resolve_window(
    since: Option<&str>,
    until: Option<&str>,
    changed_since: Option<&str>,
) -> Result<Option<scoring::TemporalQuery>, ErrorData> {
    let parse = |field: &str, stamp: Option<&str>| {
        stamp
            .map(|stamp| {
                stamp
                    .parse::<Timestamp>()
                    .map_err(|error| invalid(format!("{field}: {error}")))
            })
            .transpose()
    };
    let query = scoring::TemporalQuery {
        since: parse("since", since)?,
        until: parse("until", until)?,
        changed_since: parse("changed_since", changed_since)?,
    };
    if query.since.is_none() && query.until.is_none() && query.changed_since.is_none() {
        return Ok(None);
    }
    if query
        .since
        .zip(query.until)
        .is_some_and(|(since, until)| since > until)
    {
        return Err(invalid("since must not be after until"));
    }
    Ok(Some(query))
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

/// Push every memory this call returned back up its decay curve (§5.3 step 6).
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
