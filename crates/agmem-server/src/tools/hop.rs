//! The hop `recall` takes so the agent does not have to.
//!
//! The multihop gate (`docs/eval/multihop-gate/`, issue #27) measured the two
//! halves of chain questions separately. Agents fail them — 0/3 on two hops —
//! and the store was never the reason: every hop is one `entities`-filtered
//! call away, and in 19 read calls no agent passed that filter once. The
//! missing link is a call nobody makes, so `recall` makes it: one
//! filters-only lookup over the entities its own strongest hits name, fused
//! into the pool as one more RRF arm. The chain's next row arrives in the
//! first answer, carrying the name the agent's next question needs.
//!
//! The arm is deliberately weak ([`HOP_WEIGHT`]) and deliberately picky: a
//! vote goes only to a row that leads *onward* — one naming at least one
//! entity that is neither a seed nor the pool's topic ([`leads_onward`]). A
//! row whose every entity the page already names re-states the topic, and on
//! an entity-saturated store such rows outnumber the chain's next link badly
//! enough to crowd it out of any fetch sized to the vote — so the lookup
//! scans wide ([`HOP_SCAN`]), keeps the continuations, and lets a few vote
//! ([`HOP_LIMIT`]), reordering the tail of the page and never its head.
//!
//! The one slot that bends is the last of a full page ([`reserve_tail`],
//! issue #43): the chain's next row tends to land just past a default `k` —
//! rank 22 in the gate's scenario — and a page that cuts the row the hop
//! fetched re-creates the exact miss the arm exists to fix. So a full page
//! that would carry no hop-voted row gives its final slot to the best one
//! below the cut, at the price of its own weakest hit.

use agmem_core::{MemoryId, MemoryRecord, SpaceName};
use agmem_store::repo::{self, Candidate, Filters, Hit, Liveness, Lookup};

use crate::service::AgmemService;

/// How many of the strongest primary candidates lend their entities.
const SEED_HITS: usize = 3;

/// The most entities one hop filters on, however many the seeds carry.
const SEED_CAP: usize = 5;

/// An entity on at least this share of the primary pool is a hub — it names
/// the topic everything is already about, so hopping on it would re-fetch
/// the pool, not the chain.
const HUB_SHARE: f64 = 0.5;

/// Below this many primary candidates, shares are noise and nothing is a hub.
const HUB_MIN_POOL: usize = 8;

/// How many rows the hop lookup scans for continuations.
///
/// The lookup returns storage order, not relevance, and on a dense store the
/// rows that merely re-state the topic outnumber the chain's next link many
/// times over. A scan sized to the vote fills with them before the link ever
/// surfaces — the #43 probe found all eight slots spent that way — so the
/// scan is wider than the vote and [`leads_onward`] decides who votes.
const HOP_SCAN: usize = 32;

/// How many of the scanned continuations may vote.
const HOP_LIMIT: usize = 8;

/// The hop arm's vote relative to a primary arm's `1.0`.
///
/// At `0.5` the best hop-only row scores `0.5 / 61 ≈ 0.0082` — under the
/// `1 / 75` of a row that placed fifteenth in even one primary arm — so the
/// arm reorders the tail of the page and never its head.
const HOP_WEIGHT: f64 = 0.5;

/// Widen `candidates` by one entity hop, in place, returning the ids of the
/// rows the hop voted for — whether the vote pushed them into the pool or
/// merely landed on a row an arm of the query had already fetched. That
/// whole set is what [`reserve_tail`] guards: the #43 measurement found the
/// chain's next row was a weak primary match too, so tracking only the rows
/// the hop *added* would miss the very row it exists for.
///
/// Skipped outright when the caller filtered on `entities` themselves: the
/// hop filters on *different* entities, so its rows would violate the
/// caller's own filter and quietly break what `truncated` counts. A failing
/// hop lookup is logged and dropped rather than surfaced — the primary hits
/// are already in hand, and a thinner answer beats no answer.
pub(super) async fn run(
    service: &AgmemService,
    spaces: &[SpaceName],
    filters: &Filters,
    liveness: Liveness,
    candidates: &mut Vec<Candidate>,
) -> Vec<MemoryId> {
    if !filters.entities.is_empty() {
        return Vec::new();
    }
    let (seeds, hubs) = {
        let pool: Vec<&[String]> = candidates
            .iter()
            .filter_map(|candidate| match &candidate.hit {
                Hit::Memory(memory) => Some(memory.entities.as_slice()),
                Hit::Chunk(_) => None,
            })
            .collect();
        let hubs = hubs(&pool);
        let seeds = seeds(&pool[..pool.len().min(SEED_HITS)], &hubs);
        (seeds, hubs)
    };
    if seeds.is_empty() {
        return Vec::new();
    }

    let mut lookup = Lookup::new(spaces.to_vec());
    lookup.filters = Filters {
        kinds: filters.kinds.clone(),
        entities: seeds.clone(),
        tags: filters.tags.clone(),
    };
    lookup.liveness = liveness;
    lookup.limit = HOP_SCAN;
    match repo::direct_lookup(service.db(), &lookup).await {
        Ok(rows) => {
            let onward: Vec<MemoryRecord> = rows
                .into_iter()
                .filter(|row| leads_onward(&row.entities, &seeds, &hubs))
                .take(HOP_LIMIT)
                .collect();
            merge(candidates, onward)
        }
        Err(error) => {
            tracing::warn!(%error, "the hop lookup failed; the primary hits stand");
            Vec::new()
        }
    }
}

/// The entities the primary pool treats as its topic: each carried by at
/// least [`HUB_SHARE`] of it. Empty below [`HUB_MIN_POOL`] candidates,
/// where shares are noise.
fn hubs(pool: &[&[String]]) -> Vec<String> {
    if pool.len() < HUB_MIN_POOL {
        return Vec::new();
    }
    let mut named: Vec<&String> = Vec::new();
    for list in pool {
        for entity in *list {
            if !named.contains(&entity) {
                named.push(entity);
            }
        }
    }
    named
        .into_iter()
        .filter(|name| {
            let carriers = pool
                .iter()
                .filter(|list| list.iter().any(|entity| &entity == name))
                .count();
            (carriers as f64) >= HUB_SHARE * pool.len() as f64
        })
        .cloned()
        .collect()
}

/// The entities to hop on: those of the `top` candidates, ranked by how many
/// of them name each — agreement between hits beats placement in one — with
/// ties keeping first-seen order, `hubs` and overflow dropped.
fn seeds(top: &[&[String]], hubs: &[String]) -> Vec<String> {
    let mut ranked: Vec<(&String, usize)> = Vec::new();
    for list in top {
        let mut seen: Vec<&String> = Vec::new();
        for entity in *list {
            if seen.contains(&entity) {
                continue;
            }
            seen.push(entity);
            match ranked.iter_mut().find(|(name, _)| *name == entity) {
                Some((_, count)) => *count += 1,
                None => ranked.push((entity, 1)),
            }
        }
    }
    ranked.retain(|(name, _)| !hubs.contains(name));
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    ranked.truncate(SEED_CAP);
    ranked.into_iter().map(|(name, _)| name.clone()).collect()
}

/// Whether a fetched row is a continuation: it names something the page has
/// not already surfaced — an entity that is neither a seed nor a hub. A row
/// of nothing but seeds and hubs re-states what the strongest hits said; a
/// link is a row that hands the agent a name to follow next.
fn leads_onward(entities: &[String], seeds: &[String], hubs: &[String]) -> bool {
    entities
        .iter()
        .any(|entity| !seeds.contains(entity) && !hubs.contains(entity))
}

/// Fold hop rows into the pool: a row already there gains the hop's vote on
/// its fused score, a new one enters carrying only that. Every voted row's
/// id comes back, in hop-rank order.
fn merge(candidates: &mut Vec<Candidate>, rows: Vec<MemoryRecord>) -> Vec<MemoryId> {
    let mut voted = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let vote = contribution(index + 1);
        voted.push(row.id.clone());
        let existing = candidates
            .iter_mut()
            .find(|candidate| matches!(&candidate.hit, Hit::Memory(memory) if memory.id == row.id));
        match existing {
            Some(candidate) => candidate.rrf += vote,
            // A hop row arrives from a filters-only lookup: no vector arm
            // measured it, so it carries no similarity — which also keeps the
            // abstention floor from reading a hop row as evidence either way.
            None => candidates.push(Candidate {
                rrf: vote,
                similarity: None,
                hit: Hit::Memory(Box::new(row)),
            }),
        }
    }
    voted
}

/// The hop arm's RRF term for its `rank`-th row, 1-based: the engine's
/// `1 / (k + rank)` scaled by [`HOP_WEIGHT`], on the same `k` so both sums
/// stay in one currency.
fn contribution(rank: usize) -> f64 {
    HOP_WEIGHT / (repo::RRF_K + rank) as f64
}

/// Give the last slot of a full page to the best hop-voted row a `take(k)`
/// would otherwise cut (issue #43).
///
/// The arm's weakness leaves its rows near the very depth a default `k`
/// cuts — rank 22 in the gate's scenario — and a page that cuts the row the
/// hop fetched re-creates the exact miss the hop exists to fix. So when
/// none of `ranked[..k]` is hop-voted and one below the cut is, the
/// strongest waiting row moves into slot `k`. The page stays sorted — a row
/// from below the cut scores no more than the one it displaces — and a `k`
/// of 1 has no tail: the head of the page is always what the query matched
/// best.
pub(super) fn reserve_tail<T>(ranked: &mut Vec<T>, k: usize, is_hopped: impl Fn(&T) -> bool) {
    if k < 2 || ranked.len() <= k || ranked[..k].iter().any(&is_hopped) {
        return;
    }
    if let Some(waiting) = ranked[k..].iter().position(&is_hopped) {
        let row = ranked.remove(k + waiting);
        ranked.insert(k - 1, row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lists(raw: &[&[&str]]) -> Vec<Vec<String>> {
        raw.iter()
            .map(|list| list.iter().map(ToString::to_string).collect())
            .collect()
    }

    fn slices(owned: &[Vec<String>]) -> Vec<&[String]> {
        owned.iter().map(Vec::as_slice).collect()
    }

    #[test]
    fn seeds_rank_agreement_over_placement_and_keep_first_seen_order() {
        let owned = lists(&[&["atlas", "harbour-crew"], &["harbour-crew", "nadia"], &[]]);
        let all = slices(&owned);
        assert_eq!(
            seeds(&all, &hubs(&all)),
            ["harbour-crew", "atlas", "nadia"],
            "two hits agree on harbour-crew; the rest follow in reading order"
        );
    }

    #[test]
    fn a_repeated_entity_within_one_hit_counts_once() {
        let owned = lists(&[&["atlas", "atlas"], &["nadia"]]);
        let all = slices(&owned);
        assert_eq!(
            seeds(&all, &hubs(&all)),
            ["atlas", "nadia"],
            "a row saying a name twice is not two rows agreeing"
        );
    }

    #[test]
    fn the_cap_drops_overflow_not_the_strongest() {
        let owned = lists(&[&["a", "b", "c", "d", "e", "f"], &["f"]]);
        let all = slices(&owned);
        assert_eq!(
            seeds(&all, &hubs(&all)),
            ["f", "a", "b", "c", "d"],
            "the agreed-on entity survives the cap; the sixth first-seen does not"
        );
    }

    #[test]
    fn a_hub_entity_never_seeds_once_the_pool_can_say_so() {
        let owned = lists(&[
            &["atlas", "nadia"],
            &["atlas"],
            &["atlas"],
            &["atlas"],
            &["atlas"],
            &["atlas"],
            &["atlas"],
            &["atlas"],
        ]);
        let all = slices(&owned);
        assert_eq!(
            seeds(&all[..3], &hubs(&all)),
            ["nadia"],
            "eight of eight carry atlas: that is the topic, not a link"
        );
        assert_eq!(
            seeds(&all[..3], &hubs(&all[..7])),
            ["atlas", "nadia"],
            "under {HUB_MIN_POOL} candidates the share means nothing"
        );
    }

    #[test]
    fn a_row_of_nothing_but_seeds_and_hubs_never_votes() {
        let seeds = lists(&[&["harbour-crew", "security"]]).remove(0);
        let hubs = lists(&[&["atlas"]]).remove(0);
        let rows = lists(&[
            &["harbour-crew", "priya-raman"],
            &["atlas", "security"],
            &["atlas"],
            &[],
        ]);
        assert!(
            leads_onward(&rows[0], &seeds, &hubs),
            "a new name is a link to follow"
        );
        assert!(
            !leads_onward(&rows[1], &seeds, &hubs),
            "a seed beside the topic re-states the page"
        );
        assert!(
            !leads_onward(&rows[2], &seeds, &hubs),
            "the topic alone leads nowhere"
        );
        assert!(
            !leads_onward(&rows[3], &seeds, &hubs),
            "no entities, nowhere to lead"
        );
    }

    #[test]
    fn the_tail_is_reserved_only_when_the_cut_takes_every_hop_row() {
        let hopped = |row: &&str| row.starts_with("hop");

        let mut page = vec!["a", "b", "c", "hop-1", "hop-2"];
        reserve_tail(&mut page, 3, hopped);
        assert_eq!(
            page[..3],
            ["a", "b", "hop-1"],
            "the strongest waiting hop row takes the last slot"
        );

        let mut page = vec!["a", "hop-1", "c", "hop-2"];
        reserve_tail(&mut page, 3, hopped);
        assert_eq!(
            page[..3],
            ["a", "hop-1", "c"],
            "a hop row already on the page reserves nothing"
        );
    }

    #[test]
    fn a_page_nothing_was_cut_from_reserves_nothing() {
        let hopped = |row: &&str| row.starts_with("hop");

        let mut page = vec!["a", "b", "hop-1"];
        reserve_tail(&mut page, 10, hopped);
        assert_eq!(page, ["a", "b", "hop-1"], "shorter than k: nothing was cut");

        let mut page = vec!["a", "b", "c", "d"];
        reserve_tail(&mut page, 2, hopped);
        assert_eq!(page[..2], ["a", "b"], "no hop row below the cut either");
    }

    #[test]
    fn a_single_slot_page_keeps_the_best_match() {
        let mut page = vec!["a", "hop-1"];
        reserve_tail(&mut page, 1, |row: &&str| row.starts_with("hop"));
        assert_eq!(page[..1], ["a"], "k = 1 has no tail to reserve");
    }

    #[test]
    fn a_hop_vote_stays_under_a_mid_page_primary_placing() {
        let mid_page = 1.0 / (repo::RRF_K + 15) as f64;
        assert!(
            contribution(1) < mid_page,
            "the best hop-only row must rank under a fifteenth-place primary hit"
        );
        assert!(
            contribution(1) > contribution(2),
            "earlier hop rows vote more"
        );
    }
}
