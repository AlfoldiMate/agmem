//! Not a tool: `recall`'s per-source occupancy cap (issue #76).
//!
//! One dominant source can fill a page by itself — a long episode whose
//! chunks and distilled claims all match, or a flood of near-identical
//! writes. A bounded occupancy rule is the one memory-poisoning defense with
//! measurements behind it that needs no judge model (arXiv:2608.21230:
//! content filters caught 0 of 360 attacks; provenance *weights* had no
//! usable setting — a hard cap does), and it buys ordinary diversity on the
//! way: the rest of the page goes to the next-ranked hits from elsewhere.
//!
//! The key is **content provenance**, the same string every hit already
//! carries as `source`: a chunk occupies under its episode, a distilled
//! claim under the episode or external origin it came from. Agent-sourced
//! claims carry no key and are never deferred — each was written on its own
//! judgement, and the flood they could mount is already the near-dup gate's
//! problem. The writer/session axis (#75) is deliberately not keyed yet:
//! every store seeded over one connection shares one session id, so a
//! session cap would fire on ordinary pages long before it caught an attack.
//!
//! This is a re-slice, not a filter: rows over quota are deferred below the
//! taken ones, nothing is dropped, and the page still fills. It runs after
//! rescoring and before [`super::hop::reserve_tail`] — the hop arm may still
//! promote one hop-voted row over quota into the last slot, which is
//! accepted as bounded (`cap + 1`, one row, one source) rather than
//! re-creating the miss the hop exists to fix (issue #43).

use std::collections::HashMap;

/// How many of a `k`-row page one source may hold: half, rounded up, and
/// never below 2 — a cap of 1 would forbid a source from even supporting
/// itself, and tiny pages (`k` ≤ 2) are too small to diversify.
pub(super) fn cap(k: usize) -> usize {
    k.div_ceil(2).max(2)
}

/// How many slots of a page verbatim text — every chunk of every episode,
/// together — may hold (issue #137, `docs/eval/documents.md`).
///
/// The per-source cap gives every document its own quota, so a store of
/// twenty plans can fill a page with slices of twenty different plans, each
/// under quota, and push the claims distilled from them off it entirely.
/// Measured on the recorded eval with a real 18-document corpus: three of
/// seven scenarios lost a labelled-relevant claim from the page; this cap
/// alone brought every one back. One slot rather than zero because a slice
/// is how a caller finds the document to `inspect`.
pub(super) const VERBATIM_CAP: usize = 1;

/// The one key every verbatim slice occupies under for [`VERBATIM_CAP`].
pub(super) const VERBATIM_KEY: &str = "verbatim";

/// What a re-slice pushed out of the page, for the answer to admit.
pub(super) struct Resliced {
    /// How many rows left the top `k`.
    pub(super) displaced: usize,
    /// The sources that were over quota, in page order, deduplicated —
    /// the same `episode:<id>` / `external:<origin>` strings the hits carry.
    pub(super) sources: Vec<String>,
}

/// Defer every row past `cap` for its key, pulling the next eligible rows
/// up; `None` when the page's content would not change.
///
/// `ranked` must arrive best-first and leaves best-first: taken rows keep
/// their order, deferred rows follow in theirs, and a deferred row never
/// outranks the one promoted over it. Rows whose `key` is `None` are
/// uncapped. When every over-quota row already sat below the `k` cut the
/// slice is left untouched — the page reads the same, and reporting a cap
/// that changed nothing would be noise.
pub(super) fn apply<T>(
    ranked: &mut Vec<T>,
    k: usize,
    cap: usize,
    key: impl Fn(&T) -> Option<String>,
) -> Option<Resliced> {
    if ranked.len() <= k {
        return None;
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut taken: Vec<usize> = Vec::with_capacity(ranked.len());
    let mut deferred: Vec<usize> = Vec::new();
    for (index, row) in ranked.iter().enumerate() {
        match key(row) {
            Some(source) => {
                let count = counts.entry(source).or_insert(0);
                if *count < cap {
                    *count += 1;
                    taken.push(index);
                } else {
                    deferred.push(index);
                }
            }
            None => taken.push(index),
        }
    }

    let displaced = deferred.iter().take_while(|&&index| index < k).count();
    if displaced == 0 {
        return None;
    }
    let mut sources = Vec::new();
    for &index in deferred.iter().take(displaced) {
        let source = key(&ranked[index]).expect("only keyed rows are deferred");
        if !sources.contains(&source) {
            sources.push(source);
        }
    }

    let order: Vec<usize> = taken.into_iter().chain(deferred).collect();
    let mut slots: Vec<Option<T>> = ranked.drain(..).map(Some).collect();
    *ranked = order
        .into_iter()
        .map(|index| slots[index].take().expect("each index appears once"))
        .collect();
    Some(Resliced { displaced, sources })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keyed(rows: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        rows.iter()
            .map(|(name, key)| ((*name).to_owned(), key.map(str::to_owned)))
            .collect()
    }

    fn names(rows: &[(String, Option<String>)]) -> Vec<&str> {
        rows.iter().map(|(name, _)| name.as_str()).collect()
    }

    fn slice(rows: &mut Vec<(String, Option<String>)>, k: usize, cap: usize) -> Option<Resliced> {
        apply(rows, k, cap, |(_, key)| key.clone())
    }

    #[test]
    fn a_flooding_source_yields_its_slots_to_the_next_ranked_rows() {
        let mut rows = keyed(&[
            ("a1", Some("ep:a")),
            ("a2", Some("ep:a")),
            ("a3", Some("ep:a")),
            ("b1", Some("ep:b")),
            ("c1", Some("ep:c")),
            ("d1", Some("ep:d")),
        ]);
        let resliced = slice(&mut rows, 4, 2).expect("the cap fires");
        assert_eq!(
            names(&rows),
            ["a1", "a2", "b1", "c1", "d1", "a3"],
            "the third ep:a row defers; everything else keeps score order"
        );
        assert_eq!(resliced.displaced, 1);
        assert_eq!(resliced.sources, ["ep:a"]);
    }

    #[test]
    fn unkeyed_rows_are_never_deferred() {
        let mut rows = keyed(&[
            ("a1", Some("ep:a")),
            ("a2", Some("ep:a")),
            ("agent1", None),
            ("agent2", None),
            ("a3", Some("ep:a")),
            ("b1", Some("ep:b")),
        ]);
        let resliced = slice(&mut rows, 5, 2).expect("the cap fires");
        assert_eq!(
            names(&rows),
            ["a1", "a2", "agent1", "agent2", "b1", "a3"],
            "agent-sourced rows hold their slots whatever the counts say"
        );
        assert_eq!(resliced.displaced, 1);
    }

    #[test]
    fn over_quota_rows_already_below_the_cut_change_nothing() {
        let mut rows = keyed(&[
            ("a1", Some("ep:a")),
            ("b1", Some("ep:b")),
            ("a2", Some("ep:a")),
            ("a3", Some("ep:a")),
        ]);
        assert!(
            slice(&mut rows, 2, 2).is_none(),
            "the third ep:a row was never in the page"
        );
        assert_eq!(
            names(&rows),
            ["a1", "b1", "a2", "a3"],
            "and the slice is left untouched"
        );
    }

    #[test]
    fn a_page_the_pool_does_not_fill_is_left_alone() {
        let mut rows = keyed(&[
            ("a1", Some("ep:a")),
            ("a2", Some("ep:a")),
            ("a3", Some("ep:a")),
        ]);
        assert!(
            slice(&mut rows, 3, 1).is_none(),
            "every row is shown anyway; deferring only reshuffles"
        );
    }

    #[test]
    fn two_floods_are_both_named_once_each() {
        let mut rows = keyed(&[
            ("a1", Some("ep:a")),
            ("a2", Some("ep:a")),
            ("b1", Some("ep:b")),
            ("b2", Some("ep:b")),
            ("a3", Some("ep:a")),
            ("b3", Some("ep:b")),
            ("a4", Some("ep:a")),
            ("c1", Some("ep:c")),
        ]);
        let resliced = slice(&mut rows, 6, 2).expect("both caps fire");
        assert_eq!(
            names(&rows)[..6],
            ["a1", "a2", "b1", "b2", "c1", "a3"],
            "each flood keeps its strongest two; the freed slot goes elsewhere"
        );
        assert_eq!(resliced.displaced, 2, "a3 and b3 left the page");
        assert_eq!(resliced.sources, ["ep:a", "ep:b"]);
    }

    #[test]
    fn the_cap_is_half_the_page_and_never_below_two() {
        assert_eq!(cap(10), 5);
        assert_eq!(cap(5), 3);
        assert_eq!(cap(3), 2);
        assert_eq!(cap(1), 2, "tiny pages are too small to diversify");
    }
}
