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

    proptest! {
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
