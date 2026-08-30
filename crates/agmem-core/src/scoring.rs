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
pub const MIN_STABILITY: f64 = 0.01;

/// Ceiling on Ebbinghaus stability (issue #52).
///
/// Reinforcement raised `strength` without bound, and since the prune horizon
/// scales linearly with it, a `fast` note recalled fifty times survived about
/// three years — outliving the class it was filed under by orders of
/// magnitude. The cap bounds what use can buy: five times the class's own
/// horizon, which keeps a hot working note alive for months, not years.
///
/// One ceiling for every class, not one per class: the class already sets the
/// timescale through its rate, and `strength` is the use-multiplier on top of
/// it — a per-class cap would be a second copy of what `rate()` encodes. A
/// hard cap rather than a saturating curve for the same reason the curve
/// lives in one module: the clamp is one expression, and it is repeated
/// verbatim where the engine evaluates it (`REINFORCE`, `PRUNE_EXPIRED`), so
/// the three spellings cannot drift apart quietly.
pub const MAX_STABILITY: f64 = 5.0;

/// Retention below which a `fast` record is closed at startup (`docs/design.md`
/// §2.3, §5.5).
///
/// The one place decay is acted on rather than merely computed: working
/// context nothing has touched in weeks is not a low-ranking memory, it is a
/// finished one, and leaving it live is how a memory store grows without
/// bound.
pub const PRUNE_RETENTION: f64 = 0.05;

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
/// flattens the curve, so a frequently recalled memory outlasts an untouched
/// one — up to [`MAX_STABILITY`], past which more use buys nothing and the
/// class's own timescale reasserts itself.
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
    let stability = strength.clamp(MIN_STABILITY, MAX_STABILITY);
    (-days * rate / stability).exp()
}

/// How long a memory of `decay_class` and **unit strength** may sit untouched
/// before [`retention`] falls to `threshold`, in seconds.
///
/// The inverse of [`retention`]: `days = −ln(threshold) · strength / rate`, so
/// a particular row's horizon is this scaled by its own `strength` (clamped to
/// `[MIN_STABILITY, MAX_STABILITY]`, exactly as `retention` clamps it).
///
/// `None` when there is no such time — a class that never decays never reaches
/// a threshold below 1, and a threshold outside `(0, 1)` is not one the curve
/// crosses.
///
/// This exists so the startup prune can select rows with one comparison the
/// engine can evaluate — `last_accessed + horizon · strength < now` — instead
/// of reading every candidate back to score it here. The curve stays in this
/// module either way.
///
/// ```
/// use agmem_core::{DecayClass, scoring};
/// use jiff::{SignedDuration, Timestamp};
///
/// let horizon = scoring::decay_horizon_secs(DecayClass::Fast, 0.05).unwrap();
/// let strength = 3.0;
/// let idle = SignedDuration::from_secs((horizon * strength) as i64);
/// let then = Timestamp::UNIX_EPOCH;
///
/// let left = scoring::retention(DecayClass::Fast, strength, then, then + idle);
/// assert!((left - 0.05).abs() < 1e-6, "the horizon is where retention is 0.05: {left}");
/// assert_eq!(scoring::decay_horizon_secs(DecayClass::Pinned, 0.05), None);
/// ```
pub fn decay_horizon_secs(decay_class: DecayClass, threshold: f64) -> Option<f64> {
    let rate = decay_class.rate();
    if rate == 0.0 || threshold <= 0.0 || threshold >= 1.0 {
        return None;
    }
    Some(-threshold.ln() / rate * SECONDS_PER_DAY)
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
    /// `rrf` scaled across the pool: 1.0 for the best match, 0.0 for the
    /// weakest — or 0.0 throughout when nothing was retrieved at all.
    pub rrf_normalized: f64,
    /// The weighted final score the order is taken from.
    pub score: f64,
}

/// Rank candidates best-first, returning each with its score breakdown.
///
/// `score = 0.6·norm(rrf) + 0.25·retention + 0.15·importance` (design §5.3),
/// where `norm` is min–max across the pool.
///
/// RRF barely spreads: `1/(60 + rank)` differs by 3% between the first hit and
/// the fourth, so dividing by the best candidate — the obvious normalisation,
/// and the one that shipped first — left the 0.6 retrieval term varying by
/// 0.02 across a pool while the 0.15 importance term varied by 0.075. Decay
/// class decided the order and the match never did (issue #34). Stretching the
/// pool over the whole range gives each signal the weight it is documented to
/// have. The price is that the weakest candidate always scores zero on
/// retrieval, which is the honest answer for the least relevant thing the pool
/// contains.
///
/// Two kinds of pool have no spread to stretch. When nothing was retrieved —
/// the filters-only path, where every `rrf` is 0 — normalisation stays 0
/// throughout and the order is retention and importance alone. When every
/// candidate tied at a positive score, they all normalise to 1.
///
/// The sort is stable: equal scores keep the order the store returned them in.
pub fn rank<T>(candidates: impl IntoIterator<Item = (T, Signals)>) -> Vec<(T, Ranked)> {
    let candidates: Vec<(T, Signals)> = candidates.into_iter().collect();
    let (worst, best) = candidates
        .iter()
        .map(|(_, signals)| signals.rrf)
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), rrf| {
            (lo.min(rrf), hi.max(rrf))
        });
    let spread = best - worst;

    let mut ranked: Vec<(T, Ranked)> = candidates
        .into_iter()
        .map(|(item, signals)| {
            let rrf_normalized = if best <= 0.0 {
                0.0
            } else if spread > 0.0 {
                (signals.rrf - worst) / spread
            } else {
                1.0
            };
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
            supersedes: Vec::new(),
            superseded_by: None,
            source: crate::model::Source::Agent,
            derived_from: vec![],
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
        let often = retention(DecayClass::Normal, MAX_STABILITY, days_ago(60), now());
        assert!(often > once, "{often} should beat {once}");
        assert!(
            often > 0.75,
            "recalls at the cap make 60 days cheap: {often}"
        );
    }

    #[test]
    fn strength_saturates_at_the_cap() {
        // Fifty recalls and five buy the same curve: past the cap, more use
        // is worth nothing, and the class's own timescale decides (issue #52).
        let capped = retention(DecayClass::Fast, MAX_STABILITY, days_ago(120), now());
        let hot = retention(DecayClass::Fast, 51.0, days_ago(120), now());
        assert_eq!(hot, capped, "strength above the cap changes nothing");
        assert!(
            hot < PRUNE_RETENTION,
            "a fast note is prunable within months no matter how hot it ran: {hot}"
        );
    }

    #[test]
    fn future_timestamps_cannot_inflate_retention() {
        let ahead = now() + SignedDuration::from_hours(48);
        assert_eq!(retention(DecayClass::Fast, 1.0, ahead, now()), 1.0);
    }

    #[test]
    fn the_horizon_is_exactly_where_retention_reaches_the_threshold() {
        // Strengths inside the clamp: the horizon–retention agreement only
        // holds where the curve actually reads the value, and the prune
        // selector applies the same clamp before scaling.
        for class in [DecayClass::Fast, DecayClass::Normal, DecayClass::Slow] {
            for strength in [0.5, 1.0, MAX_STABILITY] {
                let horizon = decay_horizon_secs(class, PRUNE_RETENTION).expect("a decaying class");
                let idle = SignedDuration::from_secs((horizon * strength) as i64);
                let at = retention(class, strength, now() - idle, now());
                assert!(
                    (at - PRUNE_RETENTION).abs() < 1e-6,
                    "{class:?} at strength {strength}: {at}"
                );
                // A second either side of it decides the prune, so the
                // comparison the store makes has to be the strict one.
                let inside = retention(
                    class,
                    strength,
                    now() - idle + SignedDuration::from_secs(1),
                    now(),
                );
                assert!(inside > PRUNE_RETENTION, "{class:?}: {inside}");
            }
        }
    }

    #[test]
    fn a_class_that_never_decays_has_no_horizon() {
        assert_eq!(
            decay_horizon_secs(DecayClass::Pinned, PRUNE_RETENTION),
            None
        );
        // Thresholds the curve never crosses: retention is in (0, 1], so 0 is
        // reached at infinity and 1 only at zero idle time — a horizon of
        // either would expire every row or none of them.
        assert_eq!(decay_horizon_secs(DecayClass::Fast, 0.0), None);
        assert_eq!(decay_horizon_secs(DecayClass::Fast, 1.0), None);
        assert_eq!(decay_horizon_secs(DecayClass::Fast, -1.0), None);
    }

    #[test]
    fn the_fast_horizon_is_about_three_weeks_and_reinforcement_extends_it() {
        let horizon = decay_horizon_secs(DecayClass::Fast, PRUNE_RETENTION).expect("fast decays");
        let days = horizon / 86_400.0;
        assert!((19.0..21.0).contains(&days), "fast horizon in days: {days}");
        assert!(
            decay_horizon_secs(DecayClass::Slow, PRUNE_RETENTION).expect("slow decays") > horizon,
            "the slower the class, the longer it survives untouched"
        );
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
        assert!(
            ranked.iter().all(|(_, r)| r.rrf_normalized == 1.0),
            "a pool with no spread normalises to 1, not to 0: {ranked:?}"
        );
    }

    #[test]
    fn an_empty_or_unmatched_pool_ranks_without_dividing_by_zero() {
        assert!(rank(Vec::<(&str, Signals)>::new()).is_empty());

        let ranked = rank([("only", Signals::for_episode_chunk(0.0))]);
        assert_eq!(ranked[0].1.rrf_normalized, 0.0);
        assert!((ranked[0].1.score - (0.25 + 0.15 * 0.5)).abs() < 1e-9);
    }

    /// What `search::rrf` hands back for a hit at `position` in one list.
    fn rrf_at(position: f64) -> f64 {
        1.0 / (60.0 + position)
    }

    #[test]
    fn a_matching_fact_outranks_a_pinned_instruction_it_beat_on_retrieval() {
        // The case #34 was filed for: the answer to the query came back at
        // rank 1 and was rescored last, because max-normalisation left the
        // pinned class worth several times the entire retrieval spread.
        let fact = Signals::for_memory(rrf_at(1.0), &memory(DecayClass::Normal, 1.0, now()), now());
        let instruction =
            Signals::for_memory(rrf_at(3.0), &memory(DecayClass::Pinned, 1.0, now()), now());
        let ranked = rank([("instruction", instruction), ("fact", fact)]);

        assert_eq!(
            ranked[0].0, "fact",
            "the match decides the order, not the pin: {ranked:?}"
        );
    }

    #[test]
    fn normalisation_spans_the_pool_so_retrieval_keeps_its_weight() {
        let ranked = rank([
            ("first", Signals::for_episode_chunk(rrf_at(1.0))),
            ("second", Signals::for_episode_chunk(rrf_at(2.0))),
            ("third", Signals::for_episode_chunk(rrf_at(3.0))),
        ]);

        assert_eq!(ranked[0].1.rrf_normalized, 1.0, "{ranked:?}");
        assert_eq!(ranked[2].1.rrf_normalized, 0.0, "{ranked:?}");
        assert!(
            ranked[0].1.score - ranked[2].1.score > 0.5,
            "a weight of 0.6 has to be worth about 0.6 across the pool: {ranked:?}"
        );
    }
}
