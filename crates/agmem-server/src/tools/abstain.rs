//! Not a tool: `recall`'s honest empty page (issue #77).
//!
//! A recall always fills its page — min–max normalisation guarantees a "best"
//! hit whatever the query, and RRF is ordinal, so a page of nothing relevant
//! serialises exactly like a page of answers. Over-retrieval is the failure
//! mode with measurements behind it (PrecisionMemBench: baseline memory
//! systems at precision ≤ 0.22 without abstention), and the fix is
//! deterministic — no model, no training (Adaptive-k, EMNLP 2025).
//!
//! Two independent mechanisms, one verdict:
//!
//! - **The floor abstains.** The vector arms' cosine similarity is the pool's
//!   one absolute relevance signal (min–max `rrf_normalized` cannot say "bad
//!   everywhere" — its best is always 1.0). When the page's best measured
//!   similarity sits under [`MIN_SIMILARITY`], nothing on it is worth acting
//!   on, and an empty page with a note saying so beats ten plausible-looking
//!   misses.
//! - **The knee trims.** Within a page worth keeping, the largest drop in
//!   retrieval quality separates the hits that answered from the tail that
//!   merely ranked — but the gap only says *where* the tail starts, and the
//!   floor says *who* actually falls: a row past the knee keeps its slot
//!   unless its measured similarity is under [`MIN_SIMILARITY`] too. A gap
//!   alone cuts real answers that retrieved weakly (the harness vetoed that
//!   form); a gap plus a failed measurement is a tail worth losing.
//!
//! What never abstains: a row no vector arm measured (`None` similarity — a
//! BM25-only deployment, a hop row, a text-arm-only hit; the absence of a
//! measurement is not evidence of irrelevance), and a page whose *top* hit is
//! unmeasured — a strong exact-keyword match must not be hidden because the
//! measured rows around it are weak. The filters-only path never reaches this
//! module at all: a listing was asked for, not a search.

/// Cosine similarity below which a page's best measured hit is not an answer.
///
/// Calibrated with `calibrate_abstention` in `tests/eval.rs`, not guessed:
/// on the recorded BGE-small fixtures every labelled-relevant probe page
/// measures ≥ 0.656 at its best hit, while six of the eight labelled
/// unanswerables measure ≤ 0.599 — 0.62 sits inside that gap with margin on
/// both sides. The other two unanswerables measure 0.655–0.691, inside the
/// relevant band, and stay wrong on the scorecard rather than moving the
/// floor: BGE-small's unrelated-pair scores run high, and a floor that
/// caught them would abstain on real answers. Module constant, no env knob —
/// the precedent is `hop`'s constants and `occupancy::cap`, and a
/// wrongly-abstaining query has the filters-only path as its documented way
/// out.
pub(super) const MIN_SIMILARITY: f64 = 0.62;

/// The floor in force: [`MIN_SIMILARITY`], or — only when built with the
/// `eval-knobs` feature — whatever `AGMEM_ABSTENTION_FLOOR` says. The #133
/// candidate probe scores embedders on other cosine scales and must not be
/// cut on BGE's; a release build carries no knob (docs/eval/embed-models.md).
fn floor() -> f64 {
    #[cfg(feature = "eval-knobs")]
    {
        static FLOOR: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
        *FLOOR.get_or_init(|| {
            std::env::var("AGMEM_ABSTENTION_FLOOR")
                .ok()
                .map(|raw| raw.parse().expect("AGMEM_ABSTENTION_FLOOR is a float"))
                .unwrap_or(MIN_SIMILARITY)
        })
    }
    #[cfg(not(feature = "eval-knobs"))]
    MIN_SIMILARITY
}

/// The smallest drop in `rrf_normalized` the knee will cut at.
///
/// A floor, not the criterion: min–max normalisation stretches every pool
/// over `[0, 1]`, so on a small pool even a uniform ramp has steps this size
/// — which is what [`DOMINANCE`] exists to reject. This only keeps the knee
/// off a page whose retrieval is close to flat, where the "largest" gap is
/// noise however dominant it is.
pub(super) const MIN_GAP: f64 = 0.10;

/// How much of the page's whole retrieval spread the largest gap must hold
/// (strictly more than `1/DOMINANCE` of it) before it is a knee.
///
/// The criterion that survives normalisation, because it is scale-free: a
/// uniform ramp of `n` rows gives its largest gap `1/(n − 1)` of the spread —
/// never more than half, and exactly half only at `n = 3`, which the strict
/// comparison excludes. A cliff that separates the hits that answered from
/// the tail that merely ranked is the majority of the spread by definition.
pub(super) const DOMINANCE: f64 = 2.0;

/// What the cut did to the page: `kept == 0` is the abstention.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Verdict {
    /// Rows still on the page.
    pub(super) kept: usize,
    /// Rows the page held before the cut.
    pub(super) considered: usize,
    /// The best cosine similarity any vector arm measured on the page;
    /// `None` when nothing was measured.
    pub(super) best_similarity: Option<f64>,
}

/// Cut `page` at the knee, or clear it under the floor; `None` when the page
/// is left exactly as it came.
///
/// `page` arrives best-first by final score. `signals` reads each row's
/// `(similarity, rrf_normalized)`; `protected` marks rows the trim may not
/// cut — the ones placed by policy rather than score (the occupancy cap's
/// promotions, the hop's reserved row). Such a row ranks lower than the page
/// around it *by construction*, so the rank skip under it reads as exactly
/// the cliff the knee looks for, and cutting it would undo the policy that
/// placed it (issues #76, #43). Protected rows still abstain with everything
/// else: no placement policy is justified on a page with no answer on it.
pub(super) fn apply<T>(
    page: &mut Vec<T>,
    signals: impl Fn(&T) -> (Option<f64>, f64),
    protected: impl Fn(&T) -> bool,
) -> Option<Verdict> {
    let considered = page.len();
    let top = page.first()?;

    let best_similarity = page
        .iter()
        .filter_map(|row| signals(row).0)
        .max_by(f64::total_cmp);

    // The floor. Both conditions are deliberate: the best *measured* row
    // decides (a relevant hit buried under a well-retained pin must not be
    // thrown away with the page), and only when the top row itself was
    // measured (an unmeasured top is a text-arm match standing on its own
    // evidence).
    if signals(top).0.is_some() && best_similarity.is_some_and(|best| best < floor()) {
        page.clear();
        return Some(Verdict {
            kept: 0,
            considered,
            best_similarity,
        });
    }

    // The knee. The page is ordered by final score, not by retrieval, so the
    // raw `rrf_normalized` sequence need not be monotone; the prefix minimum
    // is, and equals the raw value wherever the two orders agree. Two rows
    // make one gap, which is always 100% of the spread — no distribution to
    // find a knee in — so the knee needs three.
    if considered < 3 {
        return None;
    }
    let mut envelope = Vec::with_capacity(considered);
    let mut running = f64::INFINITY;
    for row in page.iter() {
        running = running.min(signals(row).1);
        envelope.push(running);
    }
    let (knee, widest) = envelope
        .windows(2)
        .map(|pair| pair[0] - pair[1])
        .enumerate()
        // Earliest index on ties, so the cut is deterministic.
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .expect("two rows make at least one gap");
    let spread = envelope[0] - envelope[considered - 1];
    if widest < MIN_GAP || widest * DOMINANCE <= spread {
        return None;
    }
    // The gap says where the tail starts; the floor says who actually falls.
    // A row past the knee whose measured similarity still clears the floor is
    // a real match that happened to retrieve weakly — the deploy-migration
    // probes lose their labelled answer to a knee that cuts on gap alone, and
    // the harness vetoed exactly that. An unmeasured row is never cut, by the
    // same rule the floor applies: absence of a measurement is not evidence.
    let mut index = 0;
    page.retain(|row| {
        let expendable = signals(row).0.is_some_and(|sim| sim < floor());
        let keep = index <= knee || protected(row) || !expendable;
        index += 1;
        keep
    });
    (page.len() < considered).then_some(Verdict {
        kept: page.len(),
        considered,
        best_similarity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row: name, similarity, `rrf_normalized`, hop-protected.
    type Row = (&'static str, Option<f64>, f64, bool);

    fn cut(rows: &[Row]) -> (Vec<&'static str>, Option<Verdict>) {
        let mut page: Vec<Row> = rows.to_vec();
        let verdict = apply(&mut page, |row| (row.1, row.2), |row| row.3);
        (page.iter().map(|row| row.0).collect(), verdict)
    }

    #[test]
    fn a_page_nothing_measured_well_on_abstains() {
        let (page, verdict) = cut(&[
            ("a", Some(0.45), 1.0, false),
            ("b", Some(0.52), 0.6, false),
            ("hop", None, 0.0, true),
        ]);
        assert!(page.is_empty(), "the hop row abstains with the page");
        let verdict = verdict.expect("the floor fired");
        assert_eq!(verdict.kept, 0);
        assert_eq!(verdict.considered, 3);
        assert_eq!(verdict.best_similarity, Some(0.52));
    }

    #[test]
    fn one_good_hit_anywhere_on_the_page_holds_the_floor_open() {
        // Buried under a well-retained pin by final score, but the best
        // *measured* row is what decides — abstaining here would throw a
        // relevant hit away with the page.
        let (page, verdict) = cut(&[
            ("pin", Some(0.30), 0.4, false),
            ("answer", Some(0.85), 0.38, false),
        ]);
        assert_eq!(page, ["pin", "answer"], "{verdict:?}");
        assert!(verdict.is_none());
    }

    #[test]
    fn an_unmeasured_row_is_not_evidence_of_irrelevance() {
        // BM25-only deployment: no vector arm ran, nothing was measured, and
        // a uniform normalised ramp is what the pool looks like. Min–max
        // stretches its steps to `1/(n − 1)` — past MIN_GAP on a small pool —
        // but no step of a ramp ever dominates the spread, which is the
        // criterion that survives normalisation.
        let ramp: Vec<Row> = (0..8)
            .map(|rank| {
                let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
                (names[rank], None, 1.0 - rank as f64 / 7.0, false)
            })
            .collect();
        let (page, verdict) = cut(&ramp);
        assert_eq!(page.len(), 8, "{verdict:?}");
        assert!(verdict.is_none(), "no measurement, no floor and no knee");
    }

    #[test]
    fn a_strong_keyword_match_on_top_is_never_hidden() {
        // The top row only the text arm returned: it stands on its own
        // evidence, however weak the measured rows around it are.
        let (page, verdict) = cut(&[
            ("exact-keyword", None, 1.0, false),
            ("weak", Some(0.2), 0.9, false),
        ]);
        assert_eq!(page, ["exact-keyword", "weak"], "{verdict:?}");
        assert!(verdict.is_none());
    }

    #[test]
    fn the_knee_cuts_at_the_largest_gap_and_reports_it() {
        let (page, verdict) = cut(&[
            ("a", Some(0.9), 1.00, false),
            ("b", Some(0.8), 0.95, false),
            ("c", Some(0.4), 0.40, false),
            ("d", Some(0.3), 0.35, false),
        ]);
        assert_eq!(page, ["a", "b"], "the 0.55 drop after b is the knee");
        let verdict = verdict.expect("the knee fired");
        assert_eq!(verdict.kept, 2);
        assert_eq!(verdict.considered, 4);
        assert_eq!(verdict.best_similarity, Some(0.9));
    }

    #[test]
    fn a_real_match_past_the_knee_keeps_its_slot() {
        // The gap says where the tail starts; the floor says who falls. `c`
        // retrieved weakly but measures well — cutting it is how a knee loses
        // a labelled answer, which is the form the harness vetoed.
        let (page, verdict) = cut(&[
            ("a", Some(0.9), 1.00, false),
            ("b", Some(0.8), 0.95, false),
            ("c", Some(0.7), 0.40, false),
            ("d", Some(0.3), 0.35, false),
        ]);
        assert_eq!(page, ["a", "b", "c"], "{verdict:?}");
        assert_eq!(verdict.expect("d still falls").kept, 3);
    }

    #[test]
    fn a_protected_hop_row_survives_the_trim() {
        let (page, verdict) = cut(&[
            ("a", Some(0.9), 1.00, false),
            ("b", Some(0.2), 0.40, false),
            ("hop", Some(0.1), 0.01, true),
        ]);
        assert_eq!(
            page,
            ["a", "hop"],
            "the trim cuts b, not the row the hop reserved: {verdict:?}"
        );
        assert_eq!(verdict.expect("the knee fired").kept, 2);
    }

    #[test]
    fn a_trim_every_victim_of_which_is_protected_reports_nothing() {
        let (page, verdict) = cut(&[
            ("a", Some(0.9), 1.00, false),
            ("b", Some(0.8), 0.95, false),
            ("hop", Some(0.1), 0.01, true),
        ]);
        assert_eq!(page, ["a", "b", "hop"]);
        assert!(verdict.is_none(), "the knee fired but cut nothing");
    }

    #[test]
    fn two_rows_have_no_knee() {
        // One gap is always 100% of the spread; there is no distribution to
        // find a knee in, and min–max stretches any two distinct scores to
        // a full-spread cliff whatever their raw distance was.
        let (page, verdict) = cut(&[("a", Some(0.9), 1.0, false), ("b", Some(0.8), 0.0, false)]);
        assert_eq!(page, ["a", "b"], "{verdict:?}");
        assert!(verdict.is_none());
    }

    #[test]
    fn a_score_order_that_disagrees_with_retrieval_still_cuts_once() {
        // Page order is by final score; the raw retrieval sequence rises at
        // `pin`. The prefix-minimum envelope keeps the gap non-negative and
        // the knee lands after the first row — then the floor decides who
        // falls: the well-retained pin and the tail measure badly and go,
        // while `b` measures well and stays.
        let (page, _) = cut(&[
            ("a", Some(0.9), 1.00, false),
            ("pin", Some(0.3), 0.30, false),
            ("b", Some(0.9), 0.95, false),
            ("c", Some(0.2), 0.28, false),
        ]);
        assert_eq!(
            page,
            ["a", "b"],
            "the envelope's one real drop is after a; the rise at b is not a second knee"
        );
    }

    #[test]
    fn degenerate_pages_are_left_alone() {
        let (page, verdict) = cut(&[("only", Some(0.9), 1.0, false)]);
        assert_eq!(page, ["only"]);
        assert!(verdict.is_none(), "one row has no gap");

        let (page, verdict) = cut(&[
            ("a", Some(0.9), 1.0, false),
            ("b", Some(0.9), 1.0, false),
            ("c", Some(0.9), 1.0, false),
        ]);
        assert_eq!(page.len(), 3);
        assert!(verdict.is_none(), "identical scores have a zero gap");

        let (page, verdict) = cut(&[]);
        assert!(page.is_empty());
        assert!(verdict.is_none(), "an empty page has nothing to say");
    }

    #[test]
    fn a_single_bad_hit_still_abstains() {
        let (page, verdict) = cut(&[("only", Some(0.2), 1.0, false)]);
        assert!(page.is_empty(), "the floor needs no gap: {verdict:?}");
        assert_eq!(verdict.expect("the floor fired").kept, 0);
    }
}
