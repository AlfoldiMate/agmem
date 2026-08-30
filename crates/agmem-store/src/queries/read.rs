//! The SurrealQL behind the read path (design §5.3).
//!
//! A recall is one request. Its arms — BM25 and HNSW, over `memory` and
//! `episode_chunk` — are `LET`s holding ranked id lists, and `search::rrf`
//! fuses them by rank into one order. Only the survivors are then fetched by
//! id, so the wide projection is paid for on the pool, not on the table.
//!
//! Three engine facts shape the text, each verified against 3.2:
//!
//! - **`ORDER BY` may only name an idiom the projection carries** ("Missing
//!   order idiom … in statement selection"), so every arm projects the column
//!   it sorts on.
//! - **`CONTAINSANY` over an `entities.*`/`tags.*` index plans as a
//!   `UnionIndexScan`** — one `IndexScan` per value — while `=` on the same
//!   column silently scans and finds nothing (design §2.2).
//! - **`record::id(NONE)` is an error**, not `NONE`, so every optional record
//!   link is unwrapped behind an `IF`.
//! - **`@N@` ANDs the terms it is given.** `content @1@ 'who formats python'`
//!   matches nothing unless a row contains all three words, so one absent word
//!   — and a question always has one — takes the whole fulltext arm to empty.
//!   OR is spelled as one match reference per term (issue #39).
//! - **A conjunct beside the KNN operator loses rows on a cold index**, so
//!   every vector arm runs bare in a subquery and is filtered outside it
//!   (issue #40; see [`OVER_FETCH`]).

use super::{Builder, Script};
use crate::repo::{Filters, Liveness, Lookup, Search};

/// How many neighbours HNSW visits per query; the `EF` of `<|K,EF|>`.
const EF_SEARCH: usize = 80;

/// How many candidates a vector arm draws before its filters are applied.
///
/// **No conjunct may ride the KNN operator** (issue #40). A cold `KnnScan`
/// carrying one — `1 = 1` is enough — emits fewer rows than the same scan
/// without it, and a single unfiltered scan repairs every filtered scan after
/// it for that connection's life. agmem's arms all carry `space`/liveness, so
/// nothing ever warms them and every recall a process serves comes back short.
/// Running the scan bare and filtering its *result* cannot hit that, because
/// there is no predicate to push.
///
/// What it costs is that a candidate spent on another space, or on a
/// superseded row, no longer counts toward the pool — which is what this
/// multiplier buys back. Measured on 384 rows across two spaces: a full 64 of
/// 64 where a bare `K` gave 48, and faster than the pushed-down form it
/// replaces (~72 ms against ~112 ms).
const OVER_FETCH: usize = 4;

/// How many live neighbours the gate reports per probe.
///
/// One decides the near-duplicate question; the rest are the correction band
/// (issue #38), and a handful is what an agent can act on — a longer list is a
/// search result, which is what `recall` is for.
const NEIGHBOURS: usize = 4;

/// How many candidates the near-dup gate draws before narrowing to its space.
///
/// Its `K` is 1 — the single nearest live neighbour — but for the reason in
/// [`OVER_FETCH`] the scan no longer knows what "live" or "in this space"
/// mean, so it needs room to reach past the rows the gate will discard.
const NEAR_DUP_PROBE: usize = 64;

/// The `memory` columns [`memory_where`] can name.
///
/// A filter applied outside the scan reads them off materialised rows rather
/// than off the table, so the subquery has to carry every one of them whether
/// this particular request filters on it or not.
const FILTERABLE: &str = "id, space, kind, entities, tags, valid_from, invalid_at";

/// How many query words reach the fulltext arms.
///
/// Each one costs a match reference, a bound parameter and a disjunct in two
/// `WHERE` clauses. A question is a handful of words; a pasted paragraph is
/// not a question, and truncating it is better than building a query text
/// proportional to whatever was pasted.
const MAX_TERMS: usize = 12;

/// RRF's rank-smoothing constant: the `60` in `1 / (60 + rank)` (design §5.3).
pub(crate) const RRF_K: usize = 60;

/// Ceiling on the candidate pool and on a direct lookup's limit.
///
/// Both are formatted into the query text rather than bound — the KNN `K` has
/// to be — so they are clamped where they enter, not trusted.
pub(crate) const MAX_POOL: usize = 1_000;

/// How far a history walk follows `supersedes`/`superseded_by`.
///
/// Chains are a handful of links in practice; the bound is what stops a cycle
/// introduced by a hand-edited store from walking forever. It bounds *depth*,
/// not width: `supersedes` is a list, so the backwards half is a tree whenever
/// a claim was written to merge several.
const MAX_CHAIN: usize = 64;

/// Every `memory` column a read projects, minus `embedding` (see
/// [`crate::types`]).
const MEMORY_FIELDS: &str = "record::id(id) AS id, space, kind, content, content_hash,
     entities, tags, decay_class, <float> strength AS strength, last_accessed,
     access_count, valid_from, invalid_at, invalid_reason,
     array::map(supersedes ?? [], |$link| record::id($link)) AS supersedes,
     IF superseded_by IS NONE { NONE } ELSE { record::id(superseded_by) } AS superseded_by,
     source.kind AS source_kind,
     IF type::is_record(source.ref) { record::id(source.ref) } ELSE { source.ref }
         AS source_ref,
     array::map(derived_from ?? [],
         |$link| { table: record::table($link), id: record::id($link) }) AS derived_from,
     created_at";

/// Every `episode_chunk` column a read projects, minus `embedding`.
const CHUNK_FIELDS: &str =
    "record::id(id) AS id, record::id(episode) AS episode, space, text, position";

/// Every `episode` column a read projects.
const EPISODE_FIELDS: &str =
    "record::id(id) AS id, space, content, content_hash, occurred_at, session, created_at";

/// The words a fulltext arm searches for, in the order they were written.
///
/// Split on anything that is not alphanumeric rather than on whitespace: the
/// index's `class` tokenizer would split `don't` into two tokens itself, and a
/// term that the engine then ANDs internally is the bug this exists to avoid.
/// Duplicates are dropped because a repeated word buys a second match
/// reference and no extra recall.
///
/// No stop-word list. A row that matches only `the` scores near zero and sits
/// at the bottom of a pool that exists to be rescored — which is cheaper than
/// a word list to keep, and does not silently drop a query that is *all* stop
/// words.
pub(crate) fn terms(text: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for token in text.split(|character: char| !character.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let term = token.to_lowercase();
        if !terms.contains(&term) {
            terms.push(term);
        }
        if terms.len() == MAX_TERMS {
            break;
        }
    }
    terms
}

/// One fulltext arm: every term OR'd, scored by the sum of what matched.
///
/// `@N@` ANDs the words inside one reference, so the disjunction has to be
/// written out — `content @1@ $t0 OR content @2@ $t1` — with the scores added
/// rather than taken from a single reference. A reference that did not match
/// contributes 0, so the sum is a term-count-weighted score rather than NONE
/// (issue #39).
fn fulltext(column: &str, terms: &[String]) -> (String, String) {
    let matches: Vec<String> = terms
        .iter()
        .enumerate()
        .map(|(index, _)| format!("{column} @{}@ $t{index}", index + 1))
        .collect();
    let scores: Vec<String> = (1..=terms.len())
        .map(|reference| format!("search::score({reference})"))
        .collect();
    (matches.join(" OR "), scores.join(" + "))
}

/// The whole read of one `recall` call: every retrieval arm the request has,
/// fused into one order, and the rows behind it.
///
/// `terms` comes from [`terms`] and is what the caller binds as `$t0…$tn`;
/// empty means the request has no fulltext arms, however much text it carried.
pub(crate) fn search(search: &Search, terms: &[String]) -> Script {
    let pool = search.pool.clamp(1, MAX_POOL);
    let memories = memory_where(&search.filters, search.liveness);
    let mut builder = Builder::plain();
    let mut arms: Vec<&str> = Vec::new();

    if !terms.is_empty() {
        let (matches, scores) = fulltext("content", terms);
        builder.push(format!(
            "LET $ft = (SELECT id, {scores} AS s FROM memory
                 WHERE {memories} AND ({matches})
                 ORDER BY s DESC LIMIT {pool})"
        ));
        arms.push("$ft");
    }
    if search.vector.is_some() {
        let inner = pool * OVER_FETCH;
        builder.push(format!(
            "LET $vs = (SELECT id, d FROM
                 (SELECT {FILTERABLE}, vector::distance::knn() AS d FROM memory
                  WHERE embedding <|{inner},{EF_SEARCH}|> $vector)
                 WHERE {memories} ORDER BY d LIMIT {pool})"
        ));
        arms.push("$vs");
    }
    // Episodes are verbatim and append-only: they are never superseded, so
    // the memory-side filters have nothing to say about them. Liveness has
    // exactly one thing to say: text that had not yet been recorded was not
    // known at `$as_of`, read off the chunk's own `occurred_at` (schema v4).
    let chunks = chunk_where(search.liveness);
    if search.episodes && !terms.is_empty() {
        // The references restart at 1: they are scoped to the statement, and
        // this is a different statement over a different table.
        let (matches, scores) = fulltext("text", terms);
        builder.push(format!(
            "LET $ftc = (SELECT id, {scores} AS s FROM episode_chunk
                 WHERE {chunks} AND ({matches})
                 ORDER BY s DESC LIMIT {pool})"
        ));
        arms.push("$ftc");
    }
    if search.episodes && search.vector.is_some() {
        let inner = pool * OVER_FETCH;
        builder.push(format!(
            "LET $vsc = (SELECT id, d FROM
                 (SELECT id, space, occurred_at, vector::distance::knn() AS d FROM episode_chunk
                  WHERE embedding <|{inner},{EF_SEARCH}|> $vector)
                 WHERE {chunks} ORDER BY d LIMIT {pool})"
        ));
        arms.push("$vsc");
    }

    let arms = arms.join(", ");
    builder.push(format!(
        "LET $fused = search::rrf([{arms}], {pool}, {RRF_K})"
    ));
    builder.push("LET $hits = $fused.map(|$hit| $hit.id)");
    builder.push("LET $mids = $hits.filter(|$id| record::tb($id) = 'memory')");
    builder.push("LET $cids = $hits.filter(|$id| record::tb($id) = 'episode_chunk')");
    builder.finish(format!(
        "RETURN {{
             scored: $fused.map(|$hit| {{ id: record::id($hit.id),
                 table: record::tb($hit.id), rrf: <float> $hit.rrf_score }}),
             memories: (SELECT {MEMORY_FIELDS} FROM $mids),
             chunks: (SELECT {CHUNK_FIELDS} FROM $cids)
         }}"
    ))
}

/// Tier-1 retrieval: indexed filters, no embedding, no fusion.
///
/// Ordered by `strength` so the limit keeps what the agent has reinforced
/// most, then by id — a ULID, so that reads as most-recent-first.
pub(crate) fn direct_lookup(lookup: &Lookup) -> String {
    let limit = lookup.limit.clamp(1, MAX_POOL);
    let clauses = memory_where(&lookup.filters, lookup.liveness);
    format!(
        "SELECT {MEMORY_FIELDS} FROM memory WHERE {clauses}
         ORDER BY strength DESC, id DESC LIMIT {limit}"
    )
}

/// How many memories the filters select, with no limit and no ordering.
///
/// The companion to [`direct_lookup`]: both that and the search answer with a
/// page, and a page the size of the whole set is indistinguishable from it
/// unless something counts. `GROUP ALL` with a `?? 0` fallback because a
/// selection that matches nothing groups into no rows at all rather than into
/// a zero.
pub(crate) fn count_matching(lookup: &Lookup) -> String {
    let clauses = memory_where(&lookup.filters, lookup.liveness);
    format!("RETURN (SELECT count() AS count FROM memory WHERE {clauses} GROUP ALL)[0].count ?? 0")
}

/// Everything in one supersession chain, oldest first, `$target` included.
///
/// `{..N+collect}` follows a record link repeatedly and gathers what it
/// passed through, so both halves of the chain are one walk each rather than
/// one round-trip per link. It follows a list the same way it follows a single
/// link, which is what makes a merge — one claim closing several — walk back to
/// every wording it replaced, breadth first. Reversing that puts the furthest
/// ancestors first, so "oldest first" survives a chain that is really a tree.
pub(crate) fn history_chain() -> Script {
    let mut builder = Builder::plain();
    builder.push(format!(
        "LET $earlier = array::reverse($target.{{..{MAX_CHAIN}+collect}}.supersedes)"
    ));
    builder.push(format!(
        "LET $later = $target.{{..{MAX_CHAIN}+collect}}.superseded_by"
    ));
    builder.push("LET $chain = array::flatten([$earlier, [$target], $later])");
    builder.finish(format!(
        "RETURN {{ ids: $chain.map(|$link| record::id($link)),
             rows: (SELECT {MEMORY_FIELDS} FROM $chain) }}"
    ))
}

/// One episode with everything hanging off it (design §3.1, `inspect`).
///
/// The three reads travel together because they are one question — "what did
/// this text produce" — and answering it in three round-trips would let the
/// slices and the claims disagree about what the episode is.
///
/// `(SELECT … FROM $target WHERE space = $space)[0]` is `NONE` both when the
/// id names nothing and when it names a row in another space, so one branch
/// covers both: an id is a capability inside a space, not across them.
pub(crate) fn episode() -> Script {
    let mut builder = Builder::plain();
    builder.push(format!(
        "LET $episode = (SELECT {EPISODE_FIELDS} FROM $target WHERE space = $space)[0]"
    ));
    builder.finish(format!(
        "RETURN IF $episode IS NONE {{ NONE }} ELSE {{
             {{ episode: $episode,
                chunks: (SELECT {CHUNK_FIELDS} FROM episode_chunk
                    WHERE episode = $target ORDER BY position),
                derived: (SELECT {MEMORY_FIELDS} FROM memory
                    WHERE space = $space AND source.ref = $target
                    ORDER BY created_at) }}
         }}"
    ))
}

/// Which episode a retrieval slice belongs to (design §3.1, `inspect`).
///
/// A `recall` hit over verbatim text carries the *chunk* id, so the id an
/// agent is handed and the id `inspect` answers to were different things
/// (issue #36). Resolving one to the other is a single projection. The
/// `WHERE space` clause keeps an id a capability inside its space, exactly as
/// [`episode`] does.
pub(crate) fn chunk_episode() -> Script {
    Builder::plain().finish(
        "RETURN (SELECT VALUE record::id(episode) FROM $target WHERE space = $space)[0]".to_owned(),
    )
}

/// Which table in `$space` answers to each of the ids asked about.
///
/// A reflection cites ids an agent read off some earlier answer, and a bare
/// ULID says nothing about its table — the problem `inspect` solves by trying
/// each table in turn (issue #36). Both are asked in one round-trip here,
/// because `reflect` has to resolve every citation before it writes anything.
/// The `space` clause keeps an id a capability inside its space, as every
/// other read does; an id that names nothing simply appears in neither list.
pub(crate) const LOCATE: &str = "RETURN {
     memories: (SELECT VALUE record::id(id) FROM memory
         WHERE space IN $spaces AND id IN $mids),
     episodes: (SELECT VALUE record::id(id) FROM episode
         WHERE space IN $spaces AND id IN $eids)
 }";

/// One KNN probe per candidate vector: the nearest live memories in `$space`.
///
/// This is the near-dup gate (design §5.2 step 4), which asks the same
/// question once per memory a `remember` batch carries — so the probes travel
/// as one request rather than one round-trip each. The scan draws
/// [`NEAR_DUP_PROBE`] candidates bare and the space and liveness narrow its
/// result, rather than riding along inside it (issue #40); every `K` is a
/// literal, which the operator requires.
///
/// It returns [`NEIGHBOURS`] rows rather than one because the same pass
/// answers two questions (issue #38): the nearest row decides the near-dup
/// gate, and the rest of the band is handed back as claims the new one might
/// be correcting. An empty list is what a space holding no vectors answers
/// with; the distance is cast because a vector identical to a stored one gives
/// an integral 0 that `f64` refuses.
pub(crate) fn nearest_live(count: usize) -> Script {
    let mut builder = Builder::plain();
    for index in 0..count {
        builder.push(format!(
            "LET $n{index} = (SELECT id, content, distance FROM
                 (SELECT record::id(id) AS id, content, space, invalid_at,
                      <float> vector::distance::knn() AS distance FROM memory
                  WHERE embedding <|{NEAR_DUP_PROBE},{EF_SEARCH}|> $vec{index})
                 WHERE space = $space AND invalid_at IS NONE
                 ORDER BY distance LIMIT {NEIGHBOURS})"
        ));
    }
    let probes: Vec<String> = (0..count).map(|index| format!("$n{index}")).collect();
    builder.finish(format!("RETURN [{}]", probes.join(", ")))
}

/// Recall's reinforcement (design §5.3 step 5), for a whole page of hits.
///
/// An id naming no row is a silent no-op `UPDATE`, which is what makes this
/// safe to fire and forget; the returned ids are the ones that existed.
///
/// `$cap` is `core::scoring::MAX_STABILITY` (issue #52): past it another
/// recall still updates `access_count` and `last_accessed`, but buys no more
/// strength — without the ceiling, the prune horizon scaled linearly with use
/// and a hot `fast` note became effectively permanent.
pub(crate) const REINFORCE: &str = "UPDATE $ids
     SET strength = math::min([strength + 1.0, $cap]),
         access_count += 1, last_accessed = time::now()
     RETURN VALUE record::id(id)";

/// Every registered space, alphabetically.
///
/// The registry is a listing rather than a gate (see `repo::ensure_space`), so
/// this is the complete set of names a read may be pointed at.
pub(crate) const SPACES: &str = "SELECT VALUE name FROM space ORDER BY name";

/// Per-space counts for `inspect`.
pub(crate) const STATS: &str = "RETURN {
     memories: (SELECT count() AS count FROM memory
         WHERE space = $space GROUP ALL)[0].count ?? 0,
     live: (SELECT count() AS count FROM memory
         WHERE space = $space AND invalid_at IS NONE GROUP ALL)[0].count ?? 0,
     episodes: (SELECT count() AS count FROM episode
         WHERE space = $space GROUP ALL)[0].count ?? 0,
     chunks: (SELECT count() AS count FROM episode_chunk
         WHERE space = $space GROUP ALL)[0].count ?? 0,
     live_by_kind: (SELECT kind, count() AS count FROM memory
         WHERE space = $space AND invalid_at IS NONE GROUP BY kind ORDER BY kind)
 }";

/// Every live memory in `$space` with the vector behind it (design §5.5).
///
/// The one read that projects `embedding`, and the only one that has a reason
/// to: `consolidate` asks which stored claims are near *each other*, which is
/// not a question a KNN probe answers — that one takes a query vector and
/// finds its neighbours, so asking it N times means N HNSW scans, each with
/// its own recall loss and its own exposure to the shape issue #40 is about.
/// One flat scan and an all-pairs pass in Rust is exact, cheaper, and needs no
/// index at all.
///
/// `embedding IS NOT NONE` drops what a BM25-only write left behind: a row
/// with no vector has nothing to be near, and carrying it to the caller only
/// to discard it there wastes the widest column in the projection.
///
/// Ordered like [`direct_lookup`] — strongest first, then newest — so a space
/// past the cap loses its weakest rows rather than an arbitrary slice, and the
/// first member of a cluster is the natural one to keep.
///
/// The row and its vector come back as two projections over one selection
/// rather than one wide row, because the `SurrealValue` derive has no
/// `flatten`: spelling them together would mean a second copy of
/// [`MEMORY_FIELDS`]'s nineteen columns in Rust. Both carry the id, so the
/// caller pairs them by name and depends on nothing about their order.
pub(crate) fn live_vectors(limit: usize) -> Script {
    let limit = limit.clamp(1, MAX_POOL);
    let mut builder = Builder::plain();
    builder.push(format!(
        "LET $ids = (SELECT VALUE id FROM memory
             WHERE space = $space AND invalid_at IS NONE AND embedding IS NOT NONE
             ORDER BY strength DESC, id DESC LIMIT {limit})"
    ));
    builder.finish(format!(
        "RETURN {{ memories: (SELECT {MEMORY_FIELDS} FROM $ids),
             vectors: (SELECT record::id(id) AS id, embedding FROM $ids) }}"
    ))
}

/// Live `fast` memories the startup prune can no longer reach (design §5.5).
///
/// This is [`crate::queries::write::PRUNE_EXPIRED`]'s selector with the
/// `strength` factor taken *out*, which is the whole finding: the sweep scales
/// each row's horizon by its own strength, so reinforcement buys a working
/// note more time on every recall — months at the strength cap, where it was
/// years before #52 bounded it. Those rows are not a bug — the scaling is
/// deliberate — but nothing ever revisits them, so they sit in the `fast`
/// class holding something that turned out to be durable. The fix is a
/// judgement call (`remember` it again at a slower class, or `forget` it),
/// which is why this surfaces candidates instead of acting.
///
/// `$min_count` is what separates "reinforcement kept this alive" from "this
/// is merely a fast note nobody has touched yet"; the latter is the prune's
/// business and will expire on its own.
///
/// Idle-first, so the most overdue row is the one the agent reads first.
pub(crate) fn stale_contexts(limit: usize) -> String {
    let limit = limit.clamp(1, MAX_POOL);
    format!(
        "SELECT {MEMORY_FIELDS} FROM memory
         WHERE space = $space AND decay_class = $class AND invalid_at IS NONE
           AND access_count >= $min_count
           AND last_accessed + duration::from_secs(<int> math::round($horizon)) < time::now()
         ORDER BY last_accessed ASC LIMIT {limit}"
    )
}

/// The `WHERE` clauses every `memory` read shares.
///
/// A filter that is empty is left out of the text entirely rather than bound
/// as `[]`: `tags CONTAINSANY []` matches nothing, which would turn "no tag
/// filter" into "no results".
fn memory_where(filters: &Filters, liveness: Liveness) -> String {
    let mut clauses = vec!["space IN $spaces".to_owned()];
    match liveness {
        Liveness::Live => clauses.push("invalid_at IS NONE".to_owned()),
        Liveness::AsOf(_) => clauses.push(
            "valid_from <= $as_of AND (invalid_at IS NONE OR $as_of < invalid_at)".to_owned(),
        ),
        Liveness::Any => {}
    }
    if !filters.kinds.is_empty() {
        clauses.push("kind IN $kinds".to_owned());
    }
    if !filters.entities.is_empty() {
        clauses.push("entities CONTAINSANY $entities".to_owned());
    }
    if !filters.tags.is_empty() {
        clauses.push("tags CONTAINSANY $tags".to_owned());
    }
    clauses.join(" AND ")
}

/// The `WHERE` clauses every `episode_chunk` read shares.
///
/// Chunks carry none of the memory-side filters, so this is spaces plus at
/// most one clause: a chunk whose episode had not yet occurred was not known
/// at `$as_of`. A pre-v4 row the backfill missed reads `occurred_at = NONE`,
/// which fails the comparison and drops out — conservative, never wrong
/// about the date. `Any` matches everything, as it does for memories.
fn chunk_where(liveness: Liveness) -> String {
    match liveness {
        Liveness::AsOf(_) => "space IN $spaces AND occurred_at <= $as_of".to_owned(),
        Liveness::Live | Liveness::Any => "space IN $spaces".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use agmem_core::Kind;

    use super::*;

    #[test]
    fn a_question_becomes_one_term_per_word() {
        assert_eq!(
            terms("Which language does the user prefer?"),
            ["which", "language", "does", "the", "user", "prefer"]
        );
    }

    #[test]
    fn a_term_never_holds_two_tokens() {
        // The index's `class` tokenizer splits these itself and then ANDs the
        // halves, which is the behaviour the whole disjunction exists to
        // avoid — so they have to arrive already split.
        assert_eq!(
            terms("don't v2.1 hard-won"),
            ["don", "t", "v2", "1", "hard", "won"]
        );
    }

    #[test]
    fn repeats_and_empties_are_dropped_and_the_count_is_capped() {
        assert_eq!(terms("rust RUST Rust rust"), ["rust"]);
        assert!(terms("   ").is_empty());
        assert!(terms("?!  —  ...").is_empty());

        let long: Vec<String> = (0..MAX_TERMS + 5).map(|n| format!("w{n}")).collect();
        assert_eq!(terms(&long.join(" ")).len(), MAX_TERMS);
    }

    #[test]
    fn a_fulltext_arm_ors_its_references_and_sums_their_scores() {
        let (matches, scores) = fulltext("content", &["red".to_owned(), "blue".to_owned()]);
        assert_eq!(matches, "content @1@ $t0 OR content @2@ $t1");
        assert_eq!(scores, "search::score(1) + search::score(2)");
    }

    #[test]
    fn the_arms_a_request_has_follow_from_its_terms() {
        let mut request = Search::new(vec!["test".parse().expect("slug")]);
        request.vector = Some(vec![0.0; 384]);

        let vector_only = search(&request, &[]).text;
        assert!(!vector_only.contains("$ft"), "{vector_only}");
        assert!(vector_only.contains("$vs"), "{vector_only}");

        let both = search(&request, &["red".to_owned()]).text;
        assert!(both.contains("content @1@ $t0"), "{both}");
        assert!(
            both.contains("text @1@ $t0"),
            "the chunk arm restarts its references — they are scoped to the \
             statement, and that is a different statement: {both}"
        );
    }

    /// For every `<|K,EF|>` in the script, the text between the `WHERE` that
    /// governs it and the operator itself — which is exactly what a
    /// `KnnScan` would carry as a pushed-down predicate.
    fn knn_clauses(script: &str) -> Vec<String> {
        script
            .match_indices("<|")
            .map(|(at, _)| {
                let before = &script[..at];
                let start = before.rfind("WHERE").map_or(0, |where_| where_ + 5);
                before[start..].to_owned()
            })
            .collect()
    }

    #[test]
    fn no_conjunct_rides_the_knn_operator() {
        // Issue #40: a cold `KnnScan` carrying any predicate at all — a bare
        // `1 = 1` reproduces it — emits fewer rows than the same scan without
        // one. Every vector arm therefore scans bare and filters its result,
        // and this is what says so before a store is ever opened.
        let mut request = Search::new(vec!["test".parse().expect("slug")]);
        request.vector = Some(vec![0.0; 384]);
        request.filters.kinds = vec![Kind::Fact];

        let script = search(&request, &["red".to_owned()]).text;
        let clauses = knn_clauses(&script);
        assert_eq!(clauses.len(), 2, "one arm per table: {script}");
        for clause in &clauses {
            assert!(
                !clause.contains(" AND "),
                "a predicate rides the scan: {clause}"
            );
        }

        let gate = nearest_live(2).text;
        for clause in knn_clauses(&gate) {
            assert!(
                !clause.contains(" AND "),
                "a predicate rides the gate's scan: {clause}"
            );
        }
    }

    #[test]
    fn as_of_dates_the_chunk_arms_and_stays_off_the_scan() {
        let mut request = Search::new(vec!["test".parse().expect("slug")]);
        request.vector = Some(vec![0.0; 384]);
        request.liveness = Liveness::AsOf("2026-03-01T00:00:00Z".parse().expect("timestamp"));

        let script = search(&request, &["red".to_owned()]).text;
        let chunk_arms: Vec<&str> = script
            .split("LET ")
            .filter(|arm| arm.starts_with("$ftc") || arm.starts_with("$vsc"))
            .collect();
        assert_eq!(chunk_arms.len(), 2, "{script}");
        for arm in chunk_arms {
            assert!(
                arm.contains("occurred_at <= $as_of"),
                "an as-of read must date the verbatim side too: {arm}"
            );
        }
        // And the clause filters the scan's result, never the scan itself
        // (issue #40).
        for clause in knn_clauses(&script) {
            assert!(
                !clause.contains(" AND "),
                "a predicate rides the scan: {clause}"
            );
        }
    }

    #[test]
    fn a_vector_arm_over_fetches_and_caps_at_the_pool() {
        let mut request = Search::new(vec!["test".parse().expect("slug")]);
        request.vector = Some(vec![0.0; 384]);
        request.pool = 10;

        let script = search(&request, &[]).text;
        assert!(
            script.contains(&format!("<|{},{EF_SEARCH}|>", 10 * OVER_FETCH)),
            "the scan draws more than the pool keeps: {script}"
        );
        assert!(
            script.contains("LIMIT 10"),
            "the pool still caps the arm: {script}"
        );
    }

    #[test]
    fn consolidations_scan_carries_the_vector_and_no_knn() {
        let script = live_vectors(50).text;
        assert!(script.contains("embedding FROM $ids"), "{script}");
        assert!(
            !script.contains("<|"),
            "an all-pairs read has no query vector to probe with: {script}"
        );
        assert!(script.contains("LIMIT 50"), "{script}");
        assert!(
            live_vectors(usize::MAX)
                .text
                .contains(&format!("LIMIT {MAX_POOL}")),
            "the cap is what keeps the all-pairs pass bounded"
        );
    }

    #[test]
    fn the_stale_selector_does_not_scale_the_horizon_by_strength() {
        // The whole arm exists because `PRUNE_EXPIRED` *does* scale it, which
        // is what puts these rows out of the sweep's reach. Scaling here too
        // would select exactly the rows the prune already closed — nothing.
        let script = stale_contexts(20);
        let selector = &script[script.find("WHERE").expect("a selector")..];
        assert!(selector.contains("$horizon"), "{selector}");
        assert!(
            !selector.contains("strength"),
            "the unscaled horizon is the point of this query: {selector}"
        );
        assert!(
            selector.contains("access_count >= $min_count"),
            "{selector}"
        );
        assert!(selector.contains("decay_class = $class"), "{selector}");
    }
}
