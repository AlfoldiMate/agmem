//! `consolidate` — maintenance without a server-side LLM (design §5.5).
//!
//! Spectron runs consolidation as a scheduled job with a model behind it:
//! find the duplicates, decide the merge, rewrite the store. agmem cannot do
//! the middle step and will not pretend to — there is no LLM on this side of
//! the wire, and the one that exists is the caller. So this tool does the two
//! halves a database is actually good at, and stops:
//!
//! | | |
//! |---|---|
//! | `near_duplicates` | live claims that say the same thing, clustered |
//! | `contradictions` | live claims about one subject that may disagree |
//! | `stale_contexts` | short-lived notes that reinforcement has outlived |
//! | `over_full_tags` | tags holding more live lessons than the bound (issue #82) |
//!
//! Nothing here is a verdict, and nothing here writes. Every candidate carries
//! its **content**, not just its id — the lesson issue #38 paid for, where
//! handing an agent an id and a number left it with no way to tell whether two
//! claims agreed. An answer meant to be acted on has to be readable.
//!
//! The similarity work happens in this process rather than in the engine.
//! `nearest_live` answers "what is near *this new claim*", which is a KNN
//! probe; consolidation asks "which stored claims are near *each other*",
//! which is not one question but N — and N HNSW scans cost more than one flat
//! read and an all-pairs pass, each with its own recall loss (issue #40). The
//! pass is O(n²) over [`repo::MAX_POOL`] rows, which is where the cap comes
//! from.

use std::collections::{BTreeSet, HashMap};

use agmem_core::{Kind, MemoryRecord, SpaceName, dedup, scoring};
use agmem_store::repo::{self, Embedded, Filters, Lookup};
use jiff::Timestamp;
use rmcp::ErrorData;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::service::AgmemService;
use crate::tools::inspect::MemoryView;
use crate::tools::{LESSONS_PER_TAG, store_error};

/// The largest cluster reported as one.
///
/// A cluster is a single follow-up call — one `remember(supersedes: [ … ])`
/// naming the rest — and past a handful that call stops being reviewable. A space
/// with a twenty-way duplicate has a bigger problem than this tool solves in
/// one pass, and it will still be there on the next one.
const MAX_CLUSTER_MEMBERS: usize = 8;

/// How many clusters one answer carries, strongest first.
const MAX_CLUSTERS: usize = 20;

/// How many contradiction candidates one answer carries, closest first.
const MAX_CONTRADICTIONS: usize = 20;

/// Seconds in a day, for reporting idleness in units a person reads.
const SECONDS_PER_DAY: f64 = 86_400.0;

/// One `consolidate` call.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConsolidateParams {
    /// Where to look: `current`, `user`, `all`, or a space name. Defaults to
    /// `current` alone — unlike every other read, which also covers `user`.
    /// Maintenance is scoped to the project you are in unless you widen it.
    #[serde(default)]
    pub space: Option<String>,
}

/// What is worth cleaning up, and what it would take.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ConsolidateResult {
    /// The spaces that were examined.
    pub spaces: Vec<String>,

    /// What each space held, so an empty answer can be told apart from an
    /// unexamined one.
    pub scanned: Vec<SpaceScan>,

    /// Groups of live claims saying the same thing. Merge one by sending
    /// `remember` with the surviving wording and `supersedes` set to the ids
    /// of every other member — one call, one live claim, and each closed
    /// member still readable and pointing at what replaced it. Do not reach
    /// for `forget` here: that deletes the correction history a merge exists
    /// to keep.
    pub near_duplicates: Vec<Cluster>,

    /// Pairs of live claims about one subject that may disagree. Nothing here
    /// judges that they do — read both and decide, and expect to find some of
    /// these in `near_duplicates` too: an embedding cannot tell a claim from
    /// its own negation, so the two lists overlap by design.
    pub contradictions: Vec<Contradiction>,

    /// Short-lived notes that recall has kept alive past the point their class
    /// would have expired them.
    pub stale_contexts: Vec<StaleContext>,

    /// Tags holding more live lessons than the bound `context`'s Lessons
    /// section keeps per tag (issue #82). Merge one the way a duplicate
    /// cluster merges: one `remember` with the wording worth keeping and
    /// `supersedes` naming the lessons it absorbs — a bounded window of
    /// lessons beats an unbounded pile of them.
    pub over_full_tags: Vec<OverFullTag>,

    /// Present only when something limited the answer — no embedder, a
    /// space larger than one pass compares, or more findings than one
    /// answer carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How much of one space this pass actually looked at.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SpaceScan {
    /// The space.
    pub space: String,

    /// How many live memories carried a vector and were compared against each
    /// other. Zero means either an empty space or BM25-only mode — `note`
    /// says which.
    pub compared: usize,

    /// Whether the space holds more live memories than one pass compares. The
    /// strongest were kept; run again after acting on these.
    pub truncated: bool,
}

/// Live claims close enough to be one claim.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Cluster {
    /// The space they are in. Ids are only meaningful inside one.
    pub space: String,

    /// The claims, strongest first — the first is the natural survivor, and
    /// each carries its own text so the merge can be judged rather than
    /// guessed.
    pub members: Vec<MemoryView>,

    /// The **weakest** similarity between any two members, not merely between
    /// the two that were linked. A cluster forms transitively, so a low number
    /// here means it chained: A resembles B and B resembles C, while A and C
    /// may be about different things. Read it before merging the whole group.
    pub min_similarity: f64,

    /// The closest pair in the cluster.
    pub max_similarity: f64,
}

/// Two live claims naming one subject, offered so the caller can decide which
/// of them is true.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Contradiction {
    /// The space they are in.
    pub space: String,

    /// The subjects they both name — what makes them comparable at all.
    pub shared_entities: Vec<String>,

    /// The stronger claim, with its text.
    pub a: MemoryView,

    /// The other one, with its text.
    pub b: MemoryView,

    /// How close they are. High is not evidence of agreement: measured with
    /// this embedder, real contradictions score 0.92–0.97 — above the
    /// clustering bar — because the vector carries the subject, not the
    /// polarity. Read both claims.
    pub similarity: f64,
}

/// A tag whose live lessons outgrew the bound (issue #82).
#[derive(Debug, Serialize, JsonSchema)]
pub struct OverFullTag {
    /// The space the lessons are in.
    pub space: String,

    /// The tag they share.
    pub tag: String,

    /// How many live lessons carry it.
    pub live: usize,

    /// The bound — what `context`'s Lessons section will show for one tag, and
    /// the count a merge should aim the tag back under.
    pub keep: usize,

    /// The lessons, strongest first and each with its text, so the merge can
    /// be judged here. Capped at the same size a duplicate cluster is; `live`
    /// still counts them all.
    pub members: Vec<MemoryView>,
}

/// A short-lived note that outlived its class.
#[derive(Debug, Serialize, JsonSchema)]
pub struct StaleContext {
    /// The claim, with its text and its counters.
    pub claim: MemoryView,

    /// How long since it was last recalled.
    pub idle_days: f64,

    /// How much longer the automatic sweep will leave it, because every recall
    /// extended its horizon. A large number is the finding: this stopped being
    /// a short-lived note some time ago.
    pub expires_in_days: f64,
}

/// Find what is worth cleaning up (design §5.5).
///
/// # Errors
/// [`ErrorData`] with `INVALID_PARAMS` for a space name that is not a valid
/// slug, and `INTERNAL_ERROR` for a failing store.
pub async fn run(
    service: &AgmemService,
    params: ConsolidateParams,
) -> Result<ConsolidateResult, ErrorData> {
    // Every other read defaults to `current` plus `user`. This one does not:
    // offering merges in the shared user space from inside a project session
    // widens the blast radius of what is meant to be a local tidy-up, and
    // `space: "all"` still reaches everything for someone who means it.
    let spaces = crate::tools::spaces(service, params.space.as_deref().or(Some("current"))).await?;

    let mut scanned = Vec::with_capacity(spaces.len());
    let mut clusters = Vec::new();
    let mut contradictions = Vec::new();
    let mut stale = Vec::new();
    let mut over_full = Vec::new();
    let mut truncated_any = false;

    for space in &spaces {
        let rows = repo::live_vectors(service.db(), space, repo::MAX_POOL)
            .await
            .map_err(|error| store_error(&error))?;
        let truncated = rows.len() == repo::MAX_POOL;
        truncated_any |= truncated;
        scanned.push(SpaceScan {
            space: space.to_string(),
            compared: rows.len(),
            truncated,
        });

        let found = compare(space, rows);
        clusters.extend(found.clusters);
        contradictions.extend(found.contradictions);

        stale.extend(
            repo::stale_contexts(service.db(), space, repo::StaleContexts::new())
                .await
                .map_err(|error| store_error(&error))?
                .into_iter()
                .map(overdue),
        );

        let mut lookup = Lookup::new(vec![space.clone()]);
        lookup.filters = Filters {
            kinds: vec![Kind::Lesson],
            ..Filters::default()
        };
        lookup.limit = repo::MAX_POOL;
        let lessons = repo::direct_lookup(service.db(), &lookup)
            .await
            .map_err(|error| store_error(&error))?;
        over_full.extend(over_full_tags(space, lessons));
    }

    // Closest first in both, so the cap keeps what is most worth acting on
    // rather than whichever space came first. A cap that bites is a second
    // way this answer is thinner than the store (issue #68), and `note` has
    // to say so — `scanned` only confesses the row-fetch cut.
    let capped = clusters.len() > MAX_CLUSTERS || contradictions.len() > MAX_CONTRADICTIONS;
    clusters.sort_by(|left, right| rank(right.max_similarity, left.max_similarity));
    clusters.truncate(MAX_CLUSTERS);
    contradictions.sort_by(|left, right| rank(right.similarity, left.similarity));
    contradictions.truncate(MAX_CONTRADICTIONS);
    stale.sort_by(|left, right| rank(right.idle_days, left.idle_days));

    // Fullest first, so the cap keeps the tags most worth merging.
    let tags_capped = over_full.len() > MAX_CLUSTERS;
    over_full.sort_by(|left: &OverFullTag, right: &OverFullTag| right.live.cmp(&left.live));
    over_full.truncate(MAX_CLUSTERS);

    Ok(ConsolidateResult {
        spaces: spaces.iter().map(ToString::to_string).collect(),
        scanned,
        near_duplicates: clusters,
        contradictions,
        stale_contexts: stale,
        over_full_tags: over_full,
        note: note(service, truncated_any, capped, tags_capped),
    })
}

/// The tags whose live lessons outgrew the bound, fullest first within one
/// space (issue #82).
///
/// `lessons` arrives strongest-first from the lookup, so each tag's members
/// are already in the order the answer wants and the member cap keeps the
/// strongest. A lesson carrying several tags counts against each of them —
/// the same reading `cap_by_tag` in `context` applies.
fn over_full_tags(space: &SpaceName, lessons: Vec<MemoryRecord>) -> Vec<OverFullTag> {
    let mut by_tag: HashMap<&str, Vec<&MemoryRecord>> = HashMap::new();
    for lesson in &lessons {
        for tag in &lesson.tags {
            by_tag.entry(tag).or_default().push(lesson);
        }
    }
    let mut found: Vec<OverFullTag> = by_tag
        .into_iter()
        .filter(|(_, members)| members.len() > LESSONS_PER_TAG)
        .map(|(tag, members)| OverFullTag {
            space: space.to_string(),
            tag: tag.to_owned(),
            live: members.len(),
            keep: LESSONS_PER_TAG,
            members: members
                .into_iter()
                .take(MAX_CLUSTER_MEMBERS)
                .map(|lesson| lesson.clone().into())
                .collect(),
        })
        .collect();
    found.sort_by(|left, right| right.live.cmp(&left.live).then(left.tag.cmp(&right.tag)));
    found
}

/// What one space's all-pairs pass produced.
struct Found {
    clusters: Vec<Cluster>,
    contradictions: Vec<Contradiction>,
}

/// Compare every live memory in one space against every other one.
///
/// One pass answers both similarity questions, because they are the same
/// number read against two bands: at or above [`dedup::CLUSTER_THRESHOLD`] a
/// pair is one claim twice, and at or above [`dedup::CORRECTION_FLOOR`] it is
/// one subject that may be stated two ways. The bands overlap from the
/// clustering bar up — on purpose, for the reason on the contradiction branch
/// below — so the closest pairs are reported under both names.
fn compare(space: &SpaceName, rows: Vec<Embedded>) -> Found {
    let units: Vec<Option<dedup::Unit>> = rows
        .iter()
        .map(|row| dedup::Unit::new(&row.embedding))
        .collect();

    // The full matrix, kept because the cluster's weakest *pair* is the number
    // that reveals a chained merge, and that pair need not be an edge.
    let mut similarity: HashMap<(usize, usize), f64> = HashMap::new();
    let mut groups = Union::new(rows.len());
    let mut contradictions = Vec::new();

    for left in 0..rows.len() {
        for right in (left + 1)..rows.len() {
            let (Some(a), Some(b)) = (&units[left], &units[right]) else {
                continue;
            };
            let score = a.similarity(b);
            similarity.insert((left, right), score);

            if dedup::is_cluster_candidate(score) {
                groups.join(left, right);
            }
            // Not an `else`: the bands overlap on purpose. A pair above the
            // clustering bar is the *likeliest* disagreement, not the least
            // likely one, and partitioning here is what kept every real
            // contradiction out of this list (`dedup::is_contradiction_candidate`).
            if dedup::is_contradiction_candidate(score) {
                let shared = shared_entities(&rows[left].memory, &rows[right].memory);
                if !shared.is_empty() {
                    contradictions.push(Contradiction {
                        space: space.to_string(),
                        shared_entities: shared,
                        a: rows[left].memory.clone().into(),
                        b: rows[right].memory.clone().into(),
                        similarity: score,
                    });
                }
            }
        }
    }

    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in 0..rows.len() {
        members.entry(groups.root(index)).or_default().push(index);
    }

    // `rows` arrives strongest-first, so a group's indices are already in the
    // order the answer wants and the cap keeps the strongest members.
    let mut clusters: Vec<Cluster> = members
        .into_values()
        .filter(|group| group.len() > 1)
        .filter_map(|mut group| {
            group.truncate(MAX_CLUSTER_MEMBERS);
            let (min, max) = extremes(&group, &similarity)?;
            Some(Cluster {
                space: space.to_string(),
                members: group
                    .into_iter()
                    .map(|index| rows[index].memory.clone().into())
                    .collect(),
                min_similarity: min,
                max_similarity: max,
            })
        })
        .collect();
    clusters.sort_by(|left, right| rank(right.max_similarity, left.max_similarity));

    Found {
        clusters,
        contradictions,
    }
}

/// The weakest and closest pair *within* a cluster, edges or not.
///
/// `None` only for a group of fewer than two, which the caller has already
/// filtered out.
fn extremes(group: &[usize], similarity: &HashMap<(usize, usize), f64>) -> Option<(f64, f64)> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for (at, left) in group.iter().enumerate() {
        for right in &group[at + 1..] {
            // The matrix is stored once per pair, with the lower index first.
            let score = *similarity.get(&(*left.min(right), *left.max(right)))?;
            min = min.min(score);
            max = max.max(score);
        }
    }
    (min.is_finite() && max.is_finite()).then_some((min, max))
}

/// The subjects two claims both name, compared without regard to case.
///
/// Entities are agent-supplied strings, and the same subject written
/// `Person/Alice` once and `person/alice` the next time is the same subject.
/// The reported spelling is the first claim's, which is the stronger one.
fn shared_entities(left: &MemoryRecord, right: &MemoryRecord) -> Vec<String> {
    let folded: BTreeSet<String> = right
        .entities
        .iter()
        .map(|entity| entity.to_lowercase())
        .collect();
    left.entities
        .iter()
        .filter(|entity| folded.contains(&entity.to_lowercase()))
        .cloned()
        .collect()
}

/// How far past its class one reinforced note has been carried.
///
/// `expires_in_days` is the reprieve strength bought it: the sweep's own
/// comparison is `last_accessed + horizon · clamp(strength) < now`, so this
/// is what is left of that — strength clamped exactly as the sweep clamps it
/// (issue #52), or a row reinforced past the cap would be reported with a
/// reprieve the prune no longer grants.
fn overdue(claim: MemoryRecord) -> StaleContext {
    let idle = Timestamp::now()
        .duration_since(claim.last_accessed)
        .as_secs_f64()
        .max(0.0);
    // `stale_contexts` returned this row, so the class does have a horizon;
    // zero is a safe reading if that ever stops being true, and reports the
    // row as due rather than inventing a reprieve for it.
    let horizon = repo::prune_horizon_secs().unwrap_or(0.0);
    let stability = claim
        .strength
        .clamp(scoring::MIN_STABILITY, scoring::MAX_STABILITY);
    StaleContext {
        idle_days: idle / SECONDS_PER_DAY,
        expires_in_days: (horizon * stability - idle).max(0.0) / SECONDS_PER_DAY,
        claim: claim.into(),
    }
}

/// Why an answer is thinner than the store is, when it is.
fn note(
    service: &AgmemService,
    truncated: bool,
    capped: bool,
    tags_capped: bool,
) -> Option<String> {
    let mut reasons = Vec::new();
    if service.embedder().dim() == 0 {
        reasons.push(
            "this server runs without an embedder, so nothing has a vector and neither \
             `near_duplicates` nor `contradictions` can be computed; `stale_contexts` is unaffected",
        );
    }
    if truncated {
        reasons.push(
            "a space holds more live memories than one pass compares — the strongest were \
             kept, so call again after acting on these",
        );
    }
    if capped {
        reasons.push(
            "more near-duplicate clusters or contradiction candidates were found than one \
             answer carries — the closest were kept, so call again after acting on these",
        );
    }
    if tags_capped {
        reasons.push(
            "more over-full tags were found than one answer carries — the fullest were \
             kept, so call again after acting on these",
        );
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// Descending order over similarities, which are never NaN here but are `f64`.
fn rank(left: f64, right: f64) -> std::cmp::Ordering {
    left.partial_cmp(&right)
        .unwrap_or(std::cmp::Ordering::Equal)
}

/// Disjoint-set union over row indices: what turns a list of close pairs into
/// clusters.
///
/// Transitive by construction, which is the point — A close to B and B close
/// to C is one group to act on rather than two overlapping merges to
/// reconcile. What it costs is that A and C need not resemble each other at
/// all, which is why [`Cluster::min_similarity`] measures every pair rather
/// than only the linked ones.
struct Union(Vec<usize>);

impl Union {
    fn new(size: usize) -> Self {
        Self((0..size).collect())
    }

    fn root(&mut self, mut index: usize) -> usize {
        while self.0[index] != index {
            // Path halving: point each node at its grandparent as we climb, so
            // repeated lookups flatten the tree without a second pass.
            self.0[index] = self.0[self.0[index]];
            index = self.0[index];
        }
        index
    }

    fn join(&mut self, left: usize, right: usize) {
        let (left, right) = (self.root(left), self.root(right));
        if left != right {
            // Toward the lower index, which is the stronger row: a group's
            // root is then the member the answer lists first.
            self.0[left.max(right)] = left.min(right);
        }
    }
}

#[cfg(test)]
mod tests {
    use agmem_core::{Kind, MemoryId, Source};

    use super::*;

    fn union_of(pairs: &[(usize, usize)], size: usize) -> Vec<Vec<usize>> {
        let mut union = Union::new(size);
        for (left, right) in pairs {
            union.join(*left, *right);
        }
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for index in 0..size {
            groups.entry(union.root(index)).or_default().push(index);
        }
        let mut out: Vec<Vec<usize>> = groups.into_values().collect();
        out.sort();
        out
    }

    #[test]
    fn a_chain_of_pairs_becomes_one_group() {
        assert_eq!(union_of(&[(0, 1), (1, 2)], 4), vec![vec![0, 1, 2], vec![3]]);
        assert_eq!(union_of(&[(2, 3), (0, 1)], 4), vec![vec![0, 1], vec![2, 3]]);
        assert_eq!(union_of(&[], 3), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn a_group_is_rooted_at_its_strongest_member() {
        let mut union = Union::new(5);
        union.join(3, 4);
        union.join(1, 3);
        assert_eq!(union.root(4), 1, "rows arrive strongest first");
    }

    #[test]
    fn the_weakest_pair_is_measured_and_not_the_weakest_edge() {
        // The chained-merge case: 0~1 and 1~2 both clear the bar, 0~2 does
        // not. Reporting 0.91 here would hide exactly the risk the field
        // exists to show.
        let similarity = HashMap::from([((0, 1), 0.91), ((1, 2), 0.93), ((0, 2), 0.60)]);
        let (min, max) = extremes(&[0, 1, 2], &similarity).expect("a group of three");
        assert!((min - 0.60).abs() < 1e-9, "{min}");
        assert!((max - 0.93).abs() < 1e-9, "{max}");
    }

    fn claim(entities: &[&str]) -> MemoryRecord {
        MemoryRecord {
            id: MemoryId::new("01M145SMNH1V44GYMHB5KG5MXJ").expect("a ULID"),
            space: SpaceName::user(),
            kind: Kind::Fact,
            content: "the user formats python with black".to_owned(),
            content_hash: "deadbeef".to_owned(),
            entities: entities.iter().map(|name| (*name).to_owned()).collect(),
            tags: vec![],
            embedding: None,
            decay_class: Kind::Fact.default_decay_class(),
            strength: 1.0,
            last_accessed: Timestamp::UNIX_EPOCH,
            access_count: 0,
            valid_from: Timestamp::UNIX_EPOCH,
            invalid_at: None,
            invalid_reason: None,
            supersedes: Vec::new(),
            superseded_by: None,
            source: Source::Agent,
            writer: None,
            derived_from: Vec::new(),
            created_at: Timestamp::UNIX_EPOCH,
        }
    }

    fn lesson(tags: &[&str]) -> MemoryRecord {
        let mut lesson = claim(&[]);
        lesson.kind = Kind::Lesson;
        lesson.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
        lesson
    }

    #[test]
    fn a_tag_past_the_bound_is_reported_and_one_at_it_is_not() {
        let space = SpaceName::user();
        let rows = vec![
            lesson(&["role:architect"]),
            lesson(&["role:architect", "ops"]),
            lesson(&["role:architect"]),
            lesson(&["role:architect"]),
            lesson(&["ops"]),
            lesson(&["ops"]),
            lesson(&[]),
        ];
        let found = over_full_tags(&space, rows);

        // ops holds exactly LESSONS_PER_TAG and stays out; the multi-tag
        // lesson counted toward both.
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].tag, "role:architect");
        assert_eq!(found[0].live, 4);
        assert_eq!(found[0].keep, LESSONS_PER_TAG);
        assert_eq!(found[0].members.len(), 4);
        assert_eq!(found[0].space, space.to_string());
    }

    #[test]
    fn over_full_tags_sort_fullest_first_and_cap_their_members() {
        let space = SpaceName::user();
        let mut rows: Vec<MemoryRecord> = (0..12).map(|_| lesson(&["role:runner"])).collect();
        rows.extend((0..5).map(|_| lesson(&["role:scout", "role:runner"])));
        let found = over_full_tags(&space, rows);

        assert_eq!(found[0].tag, "role:runner");
        assert_eq!(found[0].live, 17);
        assert_eq!(
            found[0].members.len(),
            MAX_CLUSTER_MEMBERS,
            "members cap; `live` still counts them all"
        );
        assert_eq!(found[1].tag, "role:scout");
        assert_eq!(found[1].live, 5);
    }

    #[test]
    fn subjects_match_across_spelling_and_report_the_stronger_claims_wording() {
        let left = claim(&["Person/Alice", "python"]);
        let right = claim(&["person/alice", "rust"]);

        assert_eq!(shared_entities(&left, &right), ["Person/Alice"]);
        assert_eq!(shared_entities(&right, &left), ["person/alice"]);
        assert!(shared_entities(&left, &claim(&["someone-else"])).is_empty());
        assert!(shared_entities(&left, &claim(&[])).is_empty());
    }
}
