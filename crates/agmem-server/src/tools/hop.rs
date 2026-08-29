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
//! The arm is deliberately weak ([`HOP_WEIGHT`]): a row that merely
//! *concerns* a mentioned entity may still be off-topic, so it can fill the
//! tail of a page but never displace anything the query itself matched.

use agmem_core::{MemoryRecord, SpaceName};
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

/// How many rows the hop lookup may add.
const HOP_LIMIT: usize = 8;

/// The hop arm's vote relative to a primary arm's `1.0`.
///
/// At `0.5` the best hop-only row scores `0.5 / 61 ≈ 0.0082` — under the
/// `1 / 75` of a row that placed fifteenth in even one primary arm — so the
/// arm reorders the tail of the page and never its head.
const HOP_WEIGHT: f64 = 0.5;

/// Widen `candidates` by one entity hop, in place.
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
) {
    if !filters.entities.is_empty() {
        return;
    }
    let seeds = {
        let pool: Vec<&[String]> = candidates
            .iter()
            .filter_map(|candidate| match &candidate.hit {
                Hit::Memory(memory) => Some(memory.entities.as_slice()),
                Hit::Chunk(_) => None,
            })
            .collect();
        seeds(&pool[..pool.len().min(SEED_HITS)], &pool)
    };
    if seeds.is_empty() {
        return;
    }

    let mut lookup = Lookup::new(spaces.to_vec());
    lookup.filters = Filters {
        kinds: filters.kinds.clone(),
        entities: seeds,
        tags: filters.tags.clone(),
    };
    lookup.liveness = liveness;
    lookup.limit = HOP_LIMIT;
    match repo::direct_lookup(service.db(), &lookup).await {
        Ok(rows) => merge(candidates, rows),
        Err(error) => {
            tracing::warn!(%error, "the hop lookup failed; the primary hits stand");
        }
    }
}

/// The entities to hop on: those of the `top` candidates, ranked by how many
/// of them name each — agreement between hits beats placement in one — with
/// ties keeping first-seen order, hubs and overflow dropped.
///
/// `pool` is every memory candidate's entities and only feeds the hub test;
/// an entity most of the pool carries names the topic, not the next link.
fn seeds(top: &[&[String]], pool: &[&[String]]) -> Vec<String> {
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
    if pool.len() >= HUB_MIN_POOL {
        ranked.retain(|(name, _)| {
            let carriers = pool
                .iter()
                .filter(|list| list.iter().any(|entity| entity == *name))
                .count();
            (carriers as f64) < HUB_SHARE * pool.len() as f64
        });
    }
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    ranked.truncate(SEED_CAP);
    ranked.into_iter().map(|(name, _)| name.clone()).collect()
}

/// Fold hop rows into the pool: a row already there gains the hop's vote on
/// its fused score, a new one enters carrying only that.
fn merge(candidates: &mut Vec<Candidate>, rows: Vec<MemoryRecord>) {
    for (index, row) in rows.into_iter().enumerate() {
        let vote = contribution(index + 1);
        let existing = candidates
            .iter_mut()
            .find(|candidate| matches!(&candidate.hit, Hit::Memory(memory) if memory.id == row.id));
        match existing {
            Some(candidate) => candidate.rrf += vote,
            None => candidates.push(Candidate {
                rrf: vote,
                hit: Hit::Memory(Box::new(row)),
            }),
        }
    }
}

/// The hop arm's RRF term for its `rank`-th row, 1-based: the engine's
/// `1 / (k + rank)` scaled by [`HOP_WEIGHT`], on the same `k` so both sums
/// stay in one currency.
fn contribution(rank: usize) -> f64 {
    HOP_WEIGHT / (repo::RRF_K + rank) as f64
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
            seeds(&all, &all),
            ["harbour-crew", "atlas", "nadia"],
            "two hits agree on harbour-crew; the rest follow in reading order"
        );
    }

    #[test]
    fn a_repeated_entity_within_one_hit_counts_once() {
        let owned = lists(&[&["atlas", "atlas"], &["nadia"]]);
        let all = slices(&owned);
        assert_eq!(
            seeds(&all, &all),
            ["atlas", "nadia"],
            "a row saying a name twice is not two rows agreeing"
        );
    }

    #[test]
    fn the_cap_drops_overflow_not_the_strongest() {
        let owned = lists(&[&["a", "b", "c", "d", "e", "f"], &["f"]]);
        let all = slices(&owned);
        assert_eq!(
            seeds(&all, &all),
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
            seeds(&all[..3], &all),
            ["nadia"],
            "eight of eight carry atlas: that is the topic, not a link"
        );
        assert_eq!(
            seeds(&all[..3], &all[..7]),
            ["atlas", "nadia"],
            "under {HUB_MIN_POOL} candidates the share means nothing"
        );
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
