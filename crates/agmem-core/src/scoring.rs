//! Ranking math: retention, fusion weighting, validity — all pure functions.
//!
//! Decay is *computed here at read time*, never swept by a background job
//! (`docs/design.md` §2.3, §5.5): a memory's rank falls on its own as time
//! passes, and every recall that returns it pushes it back up. The store
//! hands over RRF scores from SurrealDB, this module turns them into the
//! final order and the per-signal breakdown the agent sees.

use jiff::Timestamp;

use crate::model::{DecayClass, MemoryRecord};

/// Weight of the fused retrieval score in the final ranking.
pub const WEIGHT_RRF: f64 = 0.6;
/// Weight of how well the memory has held up since it was last used.
pub const WEIGHT_RETENTION: f64 = 0.25;
/// Weight of the standing importance implied by the decay class.
pub const WEIGHT_IMPORTANCE: f64 = 0.15;

/// Floor on Ebbinghaus stability, so a zero or corrupt `strength` decays fast
/// instead of producing NaN.
const MIN_STABILITY: f64 = 0.01;

/// Seconds in a day; decay rates are per day.
const SECONDS_PER_DAY: f64 = 86_400.0;

impl DecayClass {
    /// Ebbinghaus decay rate per day (`docs/design.md` §2.3).
    pub fn rate(self) -> f64 {
        match self {
            Self::Pinned => 0.0,
            Self::Slow => 0.005,
            Self::Normal => 0.02,
            Self::Fast => 0.15,
        }
    }

    /// Standing importance in `[0, 1]`, independent of retrieval and age.
    ///
    /// This is what keeps a rarely-matched instruction ahead of a well-matched
    /// scratch note: the class the agent chose *is* the priority statement.
    pub fn importance(self) -> f64 {
        match self {
            Self::Pinned => 1.0,
            Self::Slow => 0.75,
            Self::Normal => 0.5,
            Self::Fast => 0.25,
        }
    }
}

/// How much of a memory survives, in `(0, 1]`, at `now`.
///
/// `exp(-Δdays · rate / strength)`: reinforcement raises `strength`, which
/// flattens the curve, so a frequently recalled memory becomes effectively
/// permanent while an untouched one fades out of the ranking.
///
/// ```
/// use agmem_core::{DecayClass, scoring};
/// use jiff::{SignedDuration, Timestamp};
///
/// let now = Timestamp::UNIX_EPOCH + SignedDuration::from_hours(24 * 30);
/// let untouched = scoring::retention(DecayClass::Normal, 1.0, Timestamp::UNIX_EPOCH, now);
/// let recalled = scoring::retention(DecayClass::Normal, 5.0, Timestamp::UNIX_EPOCH, now);
/// assert!(recalled > untouched);
/// assert_eq!(scoring::retention(DecayClass::Pinned, 1.0, Timestamp::UNIX_EPOCH, now), 1.0);
/// ```
pub fn retention(
    decay_class: DecayClass,
    strength: f64,
    last_accessed: Timestamp,
    now: Timestamp,
) -> f64 {
    let rate = decay_class.rate();
    if rate == 0.0 {
        return 1.0;
    }
    let days = days_between(last_accessed, now);
    let stability = strength.max(MIN_STABILITY);
    (-days * rate / stability).exp()
}

/// Elapsed days, clamped at zero so a future `last_accessed` (clock skew, an
/// agent-supplied timestamp) cannot inflate retention above 1.
fn days_between(earlier: Timestamp, later: Timestamp) -> f64 {
    let seconds = later.duration_since(earlier).as_secs_f64();
    (seconds / SECONDS_PER_DAY).max(0.0)
}

/// Whether a memory was live at `instant`: `valid_from ≤ instant < invalid_at`.
///
/// Supersession closes a record rather than deleting it, so this is what makes
/// "what did I believe last Tuesday" answerable.
pub fn is_valid_at(memory: &MemoryRecord, instant: Timestamp) -> bool {
    memory.valid_from <= instant && memory.invalid_at.is_none_or(|closed| instant < closed)
}

/// The three raw signals behind one candidate's rank.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Signals {
    /// Fused retrieval score from SurrealDB's `search::rrf`.
    pub rrf: f64,
    /// Retention at query time; 1.0 for anything that does not decay.
    pub retention: f64,
    /// Standing importance of the decay class.
    pub importance: f64,
}

impl Signals {
    /// Signals for a memory hit, decayed as of `now`.
    pub fn for_memory(rrf: f64, memory: &MemoryRecord, now: Timestamp) -> Self {
        Self {
            rrf,
            retention: retention(
                memory.decay_class,
                memory.strength,
                memory.last_accessed,
                now,
            ),
            importance: memory.decay_class.importance(),
        }
    }

    /// Signals for an episode-chunk hit.
    ///
    /// Verbatim text is ground truth: it neither decays nor carries a priority
    /// the agent set, so it competes on retrieval score alone, at the same
    /// standing importance as an ordinary fact.
    pub fn for_episode_chunk(rrf: f64) -> Self {
        Self {
            rrf,
            retention: 1.0,
            importance: DecayClass::Normal.importance(),
        }
    }
}

/// A candidate's signals plus what ranking made of them.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ranked {
    /// The raw signals.
    pub signals: Signals,
    /// `rrf` divided by the best `rrf` in the pool.
    pub rrf_normalized: f64,
    /// The weighted final score the order is taken from.
    pub score: f64,
}

/// Rank candidates best-first, returning each with its score breakdown.
///
/// `score = 0.6·norm(rrf) + 0.25·retention + 0.15·importance` (design §5.3).
/// RRF scores have no fixed scale — they depend on pool size and how many
/// lists matched — so they are normalised against the best candidate in this
/// pool, which preserves the ratios between candidates instead of stretching
/// the worst one to zero the way min-max would.
///
/// The sort is stable: equal scores keep the order the store returned them in.
pub fn rank<T>(candidates: impl IntoIterator<Item = (T, Signals)>) -> Vec<(T, Ranked)> {
    let candidates: Vec<(T, Signals)> = candidates.into_iter().collect();
    let best = candidates
        .iter()
        .map(|(_, signals)| signals.rrf)
        .fold(0.0_f64, f64::max);

    let mut ranked: Vec<(T, Ranked)> = candidates
        .into_iter()
        .map(|(item, signals)| {
            let rrf_normalized = if best > 0.0 { signals.rrf / best } else { 0.0 };
            let score = WEIGHT_RRF * rrf_normalized
                + WEIGHT_RETENTION * signals.retention
                + WEIGHT_IMPORTANCE * signals.importance;
            (
                item,
                Ranked {
                    signals,
                    rrf_normalized,
                    score,
                },
            )
        })
        .collect();
    ranked.sort_by(|(_, a), (_, b)| b.score.total_cmp(&a.score));
    ranked
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;
    use crate::model::{Kind, MemoryId, SpaceName};

    /// A fixed "now" far enough from the epoch that tests can look backwards.
    fn now() -> Timestamp {
        Timestamp::UNIX_EPOCH + SignedDuration::from_hours(24 * 3650)
    }

    fn days_ago(days: i64) -> Timestamp {
        now() - SignedDuration::from_hours(24 * days)
    }

    fn memory(decay_class: DecayClass, strength: f64, last_accessed: Timestamp) -> MemoryRecord {
        MemoryRecord {
            id: MemoryId::new("01M145SMNH1V44GYMHB5KG5MXJ").unwrap(),
            space: SpaceName::user(),
            kind: Kind::Fact,
            content: "irrelevant".to_owned(),
            content_hash: "deadbeef".to_owned(),
            entities: vec![],
            tags: vec![],
            embedding: None,
            decay_class,
            strength,
            last_accessed,
            access_count: 0,
            valid_from: days_ago(100),
            invalid_at: None,
            invalid_reason: None,
            supersedes: None,
            superseded_by: None,
            source: crate::model::Source::Agent,
            created_at: days_ago(100),
        }
    }

    #[test]
    fn fast_records_fade_below_the_prune_threshold_in_three_weeks() {
        let retained = retention(DecayClass::Fast, 1.0, days_ago(20), now());
        assert!(retained < 0.05, "fast retention after 20 days: {retained}");
        assert!(
            retention(DecayClass::Fast, 1.0, days_ago(1), now()) > 0.8,
            "but a day-old working note is still fresh"
        );
    }

    #[test]
    fn pinned_never_decays() {
        assert_eq!(
            retention(DecayClass::Pinned, 1.0, days_ago(3650), now()),
            1.0
        );
        assert_eq!(
            retention(DecayClass::Pinned, 0.0, days_ago(3650), now()),
            1.0
        );
    }

    #[test]
    fn decay_matches_the_documented_curves() {
        let normal = retention(DecayClass::Normal, 1.0, days_ago(30), now());
        assert!((normal - (-0.6_f64).exp()).abs() < 1e-9, "{normal}");
        let slow = retention(DecayClass::Slow, 1.0, days_ago(365), now());
        assert!((slow - (-1.825_f64).exp()).abs() < 1e-9, "{slow}");
    }

    #[test]
    fn reinforcement_slows_decay() {
        let once = retention(DecayClass::Normal, 1.0, days_ago(60), now());
        let often = retention(DecayClass::Normal, 6.0, days_ago(60), now());
        assert!(often > once, "{often} should beat {once}");
        assert!(often > 0.8, "six recalls make 60 days cheap: {often}");
    }

    #[test]
    fn future_timestamps_cannot_inflate_retention() {
        let ahead = now() + SignedDuration::from_hours(48);
        assert_eq!(retention(DecayClass::Fast, 1.0, ahead, now()), 1.0);
    }

    #[test]
    fn validity_window_is_half_open() {
        let mut record = memory(DecayClass::Normal, 1.0, days_ago(10));
        record.valid_from = days_ago(10);
        record.invalid_at = Some(days_ago(5));

        assert!(!is_valid_at(&record, days_ago(11)));
        assert!(
            is_valid_at(&record, days_ago(10)),
            "valid_from is inclusive"
        );
        assert!(is_valid_at(&record, days_ago(6)));
        assert!(
            !is_valid_at(&record, days_ago(5)),
            "invalid_at is exclusive"
        );

        record.invalid_at = None;
        assert!(
            is_valid_at(&record, now()),
            "live records have no upper bound"
        );
    }

    #[test]
    fn ranking_normalises_rrf_and_weights_the_signals() {
        let fresh = Signals::for_memory(0.03, &memory(DecayClass::Normal, 1.0, now()), now());
        let stale = Signals::for_memory(0.03, &memory(DecayClass::Fast, 1.0, days_ago(30)), now());
        let ranked = rank([("fresh", fresh), ("stale", stale)]);

        assert_eq!(
            ranked[0].0, "fresh",
            "equal retrieval, better retention wins"
        );
        assert_eq!(ranked[0].1.rrf_normalized, 1.0, "best rrf normalises to 1");
        assert!((ranked[0].1.score - (0.6 + 0.25 + 0.15 * 0.5)).abs() < 1e-9);
    }

    #[test]
    fn ranking_is_stable_under_ties() {
        let signals = Signals::for_episode_chunk(0.02);
        let ranked = rank([("a", signals), ("b", signals), ("c", signals)]);
        let order: Vec<&str> = ranked.iter().map(|(name, _)| *name).collect();

        assert_eq!(order, ["a", "b", "c"], "ties keep the store's order");
        assert!(ranked.iter().all(|(_, r)| r.score == ranked[0].1.score));
    }

    #[test]
    fn an_empty_or_unmatched_pool_ranks_without_dividing_by_zero() {
        assert!(rank(Vec::<(&str, Signals)>::new()).is_empty());

        let ranked = rank([("only", Signals::for_episode_chunk(0.0))]);
        assert_eq!(ranked[0].1.rrf_normalized, 0.0);
        assert!((ranked[0].1.score - (0.25 + 0.15 * 0.5)).abs() < 1e-9);
    }
}
