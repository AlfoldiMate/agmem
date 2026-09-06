//! Duplicate gates for the write path (`docs/design.md` §5.2).
//!
//! Two gates, deliberately different in kind: an *exact* one that costs
//! nothing (blake3 over normalized content, enforced by the store's unique
//! index) and a *semantic* one that costs an embedding (cosine against the
//! nearest live neighbour). The first stops re-runs of the same distillation;
//! the second stops the same claim in different words.

/// Fold content down to what identity should depend on: case and whitespace
/// carry no meaning for "have I already stored this?".
///
/// Idempotent — normalizing twice changes nothing.
pub fn normalize(content: &str) -> String {
    content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// blake3 of the normalized content, hex-encoded — the value behind the
/// unique `(space, content_hash)` index.
pub fn content_hash(content: &str) -> String {
    blake3::hash(normalize(content).as_bytes())
        .to_hex()
        .to_string()
}

/// Convert the cosine *distance* SurrealDB's KNN returns into similarity.
///
/// The engine reports distance (0 = identical); every threshold here is
/// stated as similarity, so the conversion lives in exactly one place.
pub fn similarity_from_distance(distance: f64) -> f64 {
    1.0 - distance
}

/// What a write adds over the store's nearest live neighbour (issue #83):
/// `1 − similarity`, clamped to `[0, 1]`.
///
/// Clamped because cosine similarity may be negative — an *opposed* neighbour
/// would otherwise score above 1 — and the value is persisted, so the range
/// is a promise to every reader rather than a hope. Spelled here beside
/// [`similarity_from_distance`] so the write path and any future caller agree
/// on what "novel" means.
pub fn novelty(best_similarity: f64) -> f64 {
    (1.0 - best_similarity).clamp(0.0, 1.0)
}

/// The cosine bands one embedding model's vectors are read against.
///
/// Cosine similarity means something different for every model — BGE-small's
/// unrelated pairs score high, EmbeddingGemma's score near zero — so every
/// bar is data carried by the embedder (issue #138), never a constant a
/// caller reaches for. A backend hands out the table for the model it
/// loaded; the tools ask the embedder rather than this module.
///
/// The bands, all stated as similarity:
///
/// - **`near_dup`** — at or above it two memories state the same thing.
///   Chosen high on purpose: a false merge silently loses a distinction the
///   agent drew, while a false split only costs a row the `consolidate` flow
///   can offer up later.
/// - **`correction_floor`** — below it two memories are simply about
///   different things, and a neighbour is not worth mentioning. The band
///   between it and `near_dup` is where a *correction* lives: close enough
///   to be about the same subject, far enough apart to be saying something
///   else about it — the shape of "we moved off black" against "the user
///   formats Python with black". Nothing is decided on that basis; a
///   neighbour in the band is handed back for the agent to judge (issue #38).
/// - **`cluster`** — at or above it two *stored* memories are worth offering
///   as one cluster (design §5.5, issue #25). Lower than `near_dup` on
///   purpose: the write gate only ever compares a new claim against its
///   nearest live neighbour, so a pair can end up live together at any
///   similarity, and consolidation can afford a looser bar because it
///   *proposes* rather than blocks. No ceiling: a pair at 0.99 belongs in
///   the same list as one at 0.91.
/// - **`abstention`** — below it a recall page's best measured hit is not an
///   answer, and the page comes back empty with a note (issue #77).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Same claim in different words.
    pub near_dup: f64,
    /// Same subject at all.
    pub correction_floor: f64,
    /// Worth offering as one cluster.
    pub cluster: f64,
    /// Below this, a recall has no answer.
    pub abstention: f64,
}

impl Thresholds {
    /// bge-small-en-v1.5, the calibration everything here was first measured
    /// on: the gate and the cluster bar by hand (`docs/design.md` §5.2,
    /// §5.5), the abstention floor with `calibrate_abstention` in the eval
    /// harness — every labelled-relevant probe measures ≥ 0.656 at its best
    /// hit, six of eight unanswerables ≤ 0.599, and 0.62 sits in that gap.
    pub const BGE_SMALL: Self = Self {
        near_dup: 0.95,
        correction_floor: 0.75,
        cluster: 0.90,
        abstention: 0.62,
    };

    /// EmbeddingGemma-300M, measured on the same fixtures in
    /// `docs/eval/embed-models.md` §Thresholds: the 0.95 gate holds (the
    /// paraphrase band tops out at 0.921), the correction floor moves down
    /// to 0.70 (random pairs' p99.9 is 0.69, corrected pairs' p5 is 0.68),
    /// and the abstention floor to 0.14 — Gemma's unrelated pairs sit near
    /// zero where BGE's sit near 0.6. The cluster bar is **unmeasured** for
    /// Gemma and carries BGE's number until `scripts/band-probe.nu` says
    /// otherwise.
    pub const GEMMA_300M: Self = Self {
        near_dup: 0.95,
        correction_floor: 0.70,
        cluster: 0.90,
        abstention: 0.14,
    };

    /// Whether a candidate's nearest live neighbour is close enough to call
    /// it a restatement rather than a new memory.
    #[must_use]
    pub fn is_near_duplicate(self, similarity: f64) -> bool {
        similarity >= self.near_dup
    }

    /// Whether a neighbour is a claim the new one might be correcting: same
    /// subject, different statement.
    ///
    /// Deliberately exclusive of `near_dup` — a near-duplicate is already
    /// reported, and reporting it twice under two names would suggest there
    /// were two neighbours.
    #[must_use]
    pub fn is_correction_candidate(self, similarity: f64) -> bool {
        (self.correction_floor..self.near_dup).contains(&similarity)
    }

    /// Whether two live memories are close enough to offer as the same claim.
    #[must_use]
    pub fn is_cluster_candidate(self, similarity: f64) -> bool {
        similarity >= self.cluster
    }

    /// Whether two live memories are close enough to be about one subject at
    /// all.
    ///
    /// The floor is `correction_floor`, the same one the write path uses.
    /// There is deliberately **no ceiling**, and that is a measurement rather
    /// than a taste: seven contradiction pairs an agent would plausibly hold
    /// at once — stdout against stderr, npm against pnpm, Friday deploys
    /// against never on a Friday — score 0.919 to 0.974 with BGE-small, while
    /// a pair about one subject that merely says two *different* things
    /// scores 0.898. An embedding encodes topic, not polarity, so a claim and
    /// its negation read as paraphrases of each other, and a ceiling under
    /// `cluster` therefore reported the pairs that agree and hid every pair
    /// that disagrees.
    ///
    /// So the two lists `consolidate` returns do not partition, and cannot:
    /// above `cluster` a pair is offered as both a merge candidate and a
    /// disagreement, because nothing on this side of the wire can tell those
    /// apart. What separates the lists is the question, not the range —
    /// `near_duplicates` asks whether one of these could be deleted,
    /// `contradictions` asks which of them is true — and the shared entity is
    /// what keeps the second list from being a copy of the first.
    #[must_use]
    pub fn is_contradiction_candidate(self, similarity: f64) -> bool {
        similarity >= self.correction_floor
    }
}

/// A vector prepared for repeated comparison: scaled to unit length, so
/// cosine similarity is a plain dot product.
///
/// Consolidation compares every live memory against every other one, and
/// recomputing both magnitudes inside that loop triples its cost for an answer
/// that does not change. Normalizing once at the edge also makes the invalid
/// cases unrepresentable: a zero vector has no direction, so it never becomes
/// a `Unit` at all rather than producing a silent NaN half a million
/// comparisons later.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit(Vec<f32>);

impl Unit {
    /// Scale `vector` to length 1, or `None` when it has no direction to
    /// scale — an empty vector, an all-zero one, or one carrying a non-finite
    /// component from a broken embedder.
    #[must_use]
    pub fn new(vector: &[f32]) -> Option<Self> {
        let norm = vector
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return None;
        }
        Some(Self(
            vector
                .iter()
                .map(|value| (f64::from(*value) / norm) as f32)
                .collect(),
        ))
    }

    /// Cosine similarity against another unit vector: 1.0 for the same
    /// direction, 0.0 for orthogonal, negative for opposed.
    ///
    /// Two vectors of different widths are reported as 0.0 rather than
    /// refused. It cannot happen through the store — the HNSW index rejects
    /// any width but its own at write time — and a maintenance read is not
    /// the place to fail a whole call over one impossible row.
    #[must_use]
    pub fn similarity(&self, other: &Self) -> f64 {
        if self.0.len() != other.0.len() {
            return 0.0;
        }
        self.0
            .iter()
            .zip(&other.0)
            .map(|(left, right)| f64::from(*left) * f64::from(*right))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn normalization_folds_case_and_whitespace() {
        assert_eq!(
            normalize("  The\tUser \n prefers  Rust "),
            "the user prefers rust"
        );
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("   \n\t "), "");
    }

    #[test]
    fn the_same_claim_hashes_the_same_however_it_was_typed() {
        assert_eq!(
            content_hash("The user prefers Rust"),
            content_hash("the   user\nprefers rust  ")
        );
        assert_ne!(
            content_hash("the user prefers Rust"),
            content_hash("the user prefers Python")
        );
    }

    #[test]
    fn the_gate_only_fires_at_or_above_the_threshold() {
        let bge = Thresholds::BGE_SMALL;
        assert!(bge.is_near_duplicate(similarity_from_distance(0.0)));
        assert!(bge.is_near_duplicate(similarity_from_distance(0.05)));
        assert!(!bge.is_near_duplicate(similarity_from_distance(0.051)));
        assert!(!bge.is_near_duplicate(similarity_from_distance(1.0)));
    }

    #[test]
    fn the_two_consolidate_bands_overlap_above_the_cluster_threshold() {
        let bge = Thresholds::BGE_SMALL;
        assert!(bge.is_cluster_candidate(bge.cluster));
        assert!(bge.is_contradiction_candidate(0.8999));
        assert!(!bge.is_cluster_candidate(0.8999));
        assert!(bge.is_contradiction_candidate(bge.correction_floor));
        assert!(!bge.is_contradiction_candidate(bge.correction_floor - 0.0001));

        // Measured with BGE-small: a real contradiction scores 0.919–0.974,
        // which is cluster territory, and both lists have to be able to hold
        // it. A band that stopped at the cluster threshold contained the
        // control pair — same subject, no disagreement — and nothing else.
        for measured in [0.919, 0.948, 0.974] {
            assert!(bge.is_cluster_candidate(measured));
            assert!(bge.is_contradiction_candidate(measured));
        }

        // A pair the write gate would have blocked is still a cluster — the
        // gate never compared these two to each other.
        assert!(bge.is_cluster_candidate(bge.near_dup));
        assert!(bge.is_cluster_candidate(1.0));
    }

    #[test]
    fn every_model_keeps_its_bands_ordered() {
        // The correction band must be non-empty and sit under the gate, and
        // the cluster bar must stay inside it, for any model's table.
        for table in [Thresholds::BGE_SMALL, Thresholds::GEMMA_300M] {
            assert!(table.correction_floor < table.cluster);
            assert!(table.cluster <= table.near_dup);
            assert!(table.abstention < table.correction_floor);
            assert!(table.is_correction_candidate(table.correction_floor));
            assert!(!table.is_correction_candidate(table.near_dup));
        }
    }

    #[test]
    fn a_unit_vector_scores_itself_at_one_and_its_opposite_at_minus_one() {
        let east = Unit::new(&[3.0, 0.0]).expect("a direction");
        let north = Unit::new(&[0.0, 0.5]).expect("a direction");
        let west = Unit::new(&[-2.0, 0.0]).expect("a direction");

        assert!((east.similarity(&east) - 1.0).abs() < 1e-6);
        assert!(east.similarity(&north).abs() < 1e-6);
        assert!((east.similarity(&west) + 1.0).abs() < 1e-6);

        // Magnitude is scaled away, so only the angle is left.
        assert!(
            (east.similarity(&Unit::new(&[100.0, 0.0]).expect("a direction")) - 1.0).abs() < 1e-6
        );
    }

    #[test]
    fn a_vector_with_no_direction_is_not_a_unit() {
        assert!(Unit::new(&[]).is_none());
        assert!(Unit::new(&[0.0, 0.0, 0.0]).is_none());
        assert!(Unit::new(&[f32::NAN, 1.0]).is_none());
        assert!(Unit::new(&[f32::INFINITY]).is_none());
    }

    #[test]
    fn widths_that_cannot_be_compared_score_zero_rather_than_panicking() {
        let short = Unit::new(&[1.0, 0.0]).expect("a direction");
        let long = Unit::new(&[1.0, 0.0, 0.0]).expect("a direction");
        assert_eq!(short.similarity(&long), 0.0);
    }

    #[test]
    fn the_unit_dot_product_is_the_similarity_the_engine_reports() {
        // What `nearest_live` hands back is `1 - cosine_distance`, and the
        // consolidate arms compare the same numbers against the same
        // thresholds — so the two spellings have to agree.
        let a = Unit::new(&[1.0, 1.0]).expect("a direction");
        let b = Unit::new(&[1.0, 0.0]).expect("a direction");
        let engine = similarity_from_distance(1.0 - 0.5_f64.sqrt());
        assert!((a.similarity(&b) - engine).abs() < 1e-6);
    }

    proptest! {
        #[test]
        fn a_unit_vector_has_unit_length(
            values in prop::collection::vec(-100.0_f32..100.0, 1..32)
        ) {
            if let Some(unit) = Unit::new(&values) {
                prop_assert!((unit.similarity(&unit) - 1.0).abs() < 1e-5);
            }
        }

        #[test]
        fn similarity_never_leaves_the_cosine_range(
            left in prop::collection::vec(-100.0_f32..100.0, 8..16),
            right in prop::collection::vec(-100.0_f32..100.0, 8..16),
        ) {
            if let (Some(a), Some(b)) = (Unit::new(&left), Unit::new(&right)) {
                let similarity = a.similarity(&b);
                prop_assert!((-1.0 - 1e-5..=1.0 + 1e-5).contains(&similarity), "{similarity}");
            }
        }

        #[test]
        fn normalization_is_idempotent(text in "(?s).{0,2000}") {
            let once = normalize(&text);
            prop_assert_eq!(normalize(&once), once.clone());
        }

        #[test]
        fn hashing_survives_arbitrary_unicode(text in "(?s).{0,2000}") {
            prop_assert_eq!(content_hash(&text).len(), 64);
        }
    }
}
