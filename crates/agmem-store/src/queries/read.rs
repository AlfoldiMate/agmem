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

use super::{Builder, Script};
use crate::repo::{Filters, Liveness, Lookup, Search};

/// How many neighbours HNSW visits per query; the `EF` of `<|K,EF|>`.
const EF_SEARCH: usize = 80;

/// RRF's rank-smoothing constant: the `60` in `1 / (60 + rank)` (design §5.3).
const RRF_K: usize = 60;

/// Ceiling on the candidate pool and on a direct lookup's limit.
///
/// Both are formatted into the query text rather than bound — the KNN `K` has
/// to be — so they are clamped where they enter, not trusted.
pub(crate) const MAX_POOL: usize = 1_000;

/// How far a history walk follows `supersedes`/`superseded_by`.
///
/// Chains are a handful of links in practice; the bound is what stops a cycle
/// introduced by a hand-edited store from walking forever.
const MAX_CHAIN: usize = 64;

/// Every `memory` column a read projects, minus `embedding` (see
/// [`crate::types`]).
const MEMORY_FIELDS: &str = "record::id(id) AS id, space, kind, content, content_hash,
     entities, tags, decay_class, <float> strength AS strength, last_accessed,
     access_count, valid_from, invalid_at, invalid_reason,
     IF supersedes IS NONE { NONE } ELSE { record::id(supersedes) } AS supersedes,
     IF superseded_by IS NONE { NONE } ELSE { record::id(superseded_by) } AS superseded_by,
     source.kind AS source_kind,
     IF type::is_record(source.ref) { record::id(source.ref) } ELSE { source.ref }
         AS source_ref,
     created_at";

/// Every `episode_chunk` column a read projects, minus `embedding`.
const CHUNK_FIELDS: &str =
    "record::id(id) AS id, record::id(episode) AS episode, space, text, position";

/// Every `episode` column a read projects.
const EPISODE_FIELDS: &str =
    "record::id(id) AS id, space, content, content_hash, occurred_at, session, created_at";

/// The whole read of one `recall` call: every retrieval arm the request has,
/// fused into one order, and the rows behind it.
pub(crate) fn search(search: &Search) -> Script {
    let pool = search.pool.clamp(1, MAX_POOL);
    let memories = memory_where(&search.filters, search.liveness);
    let mut builder = Builder::plain();
    let mut arms: Vec<&str> = Vec::new();

    if search.text.is_some() {
        builder.push(format!(
            "LET $ft = (SELECT id, search::score(1) AS s FROM memory
                 WHERE {memories} AND content @1@ $text
                 ORDER BY s DESC LIMIT {pool})"
        ));
        arms.push("$ft");
    }
    if search.vector.is_some() {
        builder.push(format!(
            "LET $vs = (SELECT id, vector::distance::knn() AS d FROM memory
                 WHERE {memories} AND embedding <|{pool},{EF_SEARCH}|> $vector
                 ORDER BY d)"
        ));
        arms.push("$vs");
    }
    // Episodes are verbatim and append-only: they are never superseded, so
    // liveness and the memory-side filters have nothing to say about them.
    if search.episodes && search.text.is_some() {
        builder.push(format!(
            "LET $ftc = (SELECT id, search::score(1) AS s FROM episode_chunk
                 WHERE space IN $spaces AND text @1@ $text
                 ORDER BY s DESC LIMIT {pool})"
        ));
        arms.push("$ftc");
    }
    if search.episodes && search.vector.is_some() {
        builder.push(format!(
            "LET $vsc = (SELECT id, vector::distance::knn() AS d FROM episode_chunk
                 WHERE space IN $spaces AND embedding <|{pool},{EF_SEARCH}|> $vector
                 ORDER BY d)"
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

/// Everything in one supersession chain, oldest first, `$target` included.
///
/// `{..N+collect}` follows a record link repeatedly and gathers what it
/// passed through, so both halves of the chain are one walk each rather than
/// one round-trip per link.
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

/// One KNN probe per candidate vector: the nearest live memory in `$space`.
///
/// This is the near-dup gate (design §5.2 step 4), which asks the same
/// question once per memory a `remember` batch carries — so the probes travel
/// as one request rather than one round-trip each. `K` is 1 and, like every
/// other `K`, a literal.
///
/// `(SELECT … LIMIT 1)[0]` is `NONE` when the space holds no vectors at all,
/// which is what an empty store answers with; the distance is cast because a
/// vector identical to a stored one gives an integral 0 that `f64` refuses.
pub(crate) fn nearest_live(count: usize) -> Script {
    let mut builder = Builder::plain();
    for index in 0..count {
        builder.push(format!(
            "LET $n{index} = (SELECT record::id(id) AS id,
                 <float> vector::distance::knn() AS distance FROM memory
                 WHERE space = $space AND invalid_at IS NONE
                     AND embedding <|1,{EF_SEARCH}|> $vec{index}
                 ORDER BY distance LIMIT 1)[0]"
        ));
    }
    let probes: Vec<String> = (0..count).map(|index| format!("$n{index}")).collect();
    builder.finish(format!("RETURN [{}]", probes.join(", ")))
}

/// Recall's reinforcement (design §5.3 step 5), for a whole page of hits.
///
/// An id naming no row is a silent no-op `UPDATE`, which is what makes this
/// safe to fire and forget; the returned ids are the ones that existed.
pub(crate) const REINFORCE: &str = "UPDATE $ids
     SET strength += 1.0, access_count += 1, last_accessed = time::now()
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
