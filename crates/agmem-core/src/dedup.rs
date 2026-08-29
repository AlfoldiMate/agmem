//! Duplicate gates for the write path (`docs/design.md` §5.2).
//!
//! Two gates, deliberately different in kind: an *exact* one that costs
//! nothing (blake3 over normalized content, enforced by the store's unique
//! index) and a *semantic* one that costs an embedding (cosine against the
//! nearest live neighbour). The first stops re-runs of the same distillation;
//! the second stops the same claim in different words.

/// Cosine similarity at or above which two memories state the same thing.
///
/// Chosen high on purpose: a false merge silently loses a distinction the
/// agent drew, while a false split only costs a row the `consolidate` flow
/// can offer up later.
pub const NEAR_DUP_THRESHOLD: f64 = 0.95;

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

/// Cosine similarity below which two memories are simply about different
/// things, and a neighbour is not worth mentioning.
///
/// The band between this and [`NEAR_DUP_THRESHOLD`] is where a *correction*
/// lives: close enough to be about the same subject, far enough apart to be
/// saying something else about it — which is exactly the shape of "we moved
/// off black" against "the user formats Python with black". Nothing is decided
/// on that basis; a neighbour in the band is handed back for the agent to
/// judge, the way a near-duplicate already is (issue #38).
pub const CORRECTION_FLOOR: f64 = 0.75;

/// Whether a candidate's nearest live neighbour is close enough to call it a
/// restatement rather than a new memory.
pub fn is_near_duplicate(similarity: f64) -> bool {
    similarity >= NEAR_DUP_THRESHOLD
}

/// Whether a neighbour is a claim the new one might be correcting: same
/// subject, different statement.
///
/// Deliberately exclusive of [`NEAR_DUP_THRESHOLD`] — a near-duplicate is
/// already reported, and reporting it twice under two names would suggest
/// there were two neighbours.
pub fn is_correction_candidate(similarity: f64) -> bool {
    (CORRECTION_FLOOR..NEAR_DUP_THRESHOLD).contains(&similarity)
}

/// Cosine similarity at or above which two *stored* memories are worth
/// offering as one cluster (design §5.5, issue #25).
///
/// Lower than [`NEAR_DUP_THRESHOLD`] on purpose, and that gap is the whole
/// point: the write gate only ever compares a new claim against its nearest
/// live neighbour, so a pair can end up live together at any similarity — two
/// entries of one batch never meet each other, and a `forget` or a
/// supersession can leave a row whose twin was written while it was closed.
/// Consolidation is where those show up, and it can afford a looser bar than
/// the gate because it *proposes* rather than blocks: a false cluster costs
/// the agent a glance, where a false auto-merge would silently lose a
/// distinction.
///
/// There is no ceiling. A pair at 0.99 is the most duplicated thing the store
/// holds and belongs in the same list as one at 0.91.
pub const CLUSTER_THRESHOLD: f64 = 0.90;

/// Whether two live memories are close enough to offer as the same claim.
pub fn is_cluster_candidate(similarity: f64) -> bool {
    similarity >= CLUSTER_THRESHOLD
}

/// Whether two live memories are close enough to be about one subject at all.
///
/// The floor is [`CORRECTION_FLOOR`], the same one the write path uses. There
/// is deliberately **no ceiling**, and that is a measurement rather than a
/// taste: seven contradiction pairs an agent would plausibly hold at once —
/// stdout against stderr, npm against pnpm, Friday deploys against never on a
/// Friday — score 0.919 to 0.974 with BGE-small, while a pair about one
/// subject that merely says two *different* things scores 0.898. An embedding
/// encodes topic, not polarity, so a claim and its negation read as
/// paraphrases of each other, and a ceiling under [`CLUSTER_THRESHOLD`]
/// therefore reported the pairs that agree and hid every pair that disagrees.
///
/// So the two lists `consolidate` returns do not partition, and cannot: above
/// [`CLUSTER_THRESHOLD`] a pair is offered as both a merge candidate and a
/// disagreement, because nothing on this side of the wire can tell those
/// apart. What separates the lists is the question, not the range —
/// `near_duplicates` asks whether one of these could be deleted,
/// `contradictions` asks which of them is true — and the shared entity is what
/// keeps the second list from being a copy of the first.
pub fn is_contradiction_candidate(similarity: f64) -> bool {
    similarity >= CORRECTION_FLOOR
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
        assert!(is_near_duplicate(similarity_from_distance(0.0)));
        assert!(is_near_duplicate(similarity_from_distance(0.05)));
        assert!(!is_near_duplicate(similarity_from_distance(0.051)));
        assert!(!is_near_duplicate(similarity_from_distance(1.0)));
    }

    #[test]
    fn the_two_consolidate_bands_overlap_above_the_cluster_threshold() {
        assert!(is_cluster_candidate(CLUSTER_THRESHOLD));
        assert!(is_contradiction_candidate(0.8999));
        assert!(!is_cluster_candidate(0.8999));
        assert!(is_contradiction_candidate(CORRECTION_FLOOR));
        assert!(!is_contradiction_candidate(CORRECTION_FLOOR - 0.0001));

        // Measured with BGE-small: a real contradiction scores 0.919–0.974,
        // which is cluster territory, and both lists have to be able to hold
        // it. A band that stopped at the cluster threshold contained the
        // control pair — same subject, no disagreement — and nothing else.
        for measured in [0.919, 0.948, 0.974] {
            assert!(is_cluster_candidate(measured));
            assert!(is_contradiction_candidate(measured));
        }

        // A pair the write gate would have blocked is still a cluster — the
        // gate never compared these two to each other.
        assert!(is_cluster_candidate(NEAR_DUP_THRESHOLD));
        assert!(is_cluster_candidate(1.0));
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
