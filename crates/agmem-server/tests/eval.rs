//! The memory-quality eval (issue #32): scripted sessions with known facts,
//! corrections and distractors, replayed through the real MCP surface and
//! scored off what the tools actually returned.
//!
//! The baseline lives as a JSON block in `docs/eval/quality.md` and is
//! asserted with plain equality — the doc *is* the snapshot, so "baseline
//! numbers recorded in docs" and "the harness notices a scoring change" are
//! the same mechanism. An intended change re-records with
//! `cargo test -p agmem-server --test eval -- --ignored record_baseline`
//! and reviews the diff; an unintended one fails here first.
//!
//! Numbers come from [`harness::recorded::RecordedEmbedder`], which replays
//! committed real-BGE vectors: real model semantics, bit-stable, offline.

mod harness;
// A file directly under `tests/` is a target of its own, so the eval's
// modules live one level down and are pathed in.
#[path = "eval/metrics.rs"]
mod metrics;
#[path = "eval/scenario.rs"]
mod scenario;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agmem_embed::NoopEmbedder;
use harness::recorded::RecordedEmbedder;

fn quality_doc_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/eval/quality.md")
}

/// The scorecard block of `docs/eval/quality.md`, bounded by the marker
/// comment so prose elsewhere in the doc can carry its own fences.
const MARKER: &str = "<!-- eval:scorecard -->";

fn recorded_block(doc: &str) -> &str {
    let after_marker = doc
        .split_once(MARKER)
        .expect("quality.md carries the scorecard marker")
        .1;
    after_marker
        .split_once("```json\n")
        .expect("a json fence follows the marker")
        .1
        .split_once("\n```")
        .expect("the fence closes")
        .0
}

#[tokio::test]
async fn quality_matches_the_recorded_baseline() {
    let scenarios = scenario::all();
    let scored = metrics::scorecard(&scenarios, Arc::new(RecordedEmbedder), None).await;
    let doc = std::fs::read_to_string(quality_doc_path()).expect("read docs/eval/quality.md");
    let recorded: metrics::Scorecard =
        serde_json::from_str(recorded_block(&doc)).expect("the recorded scorecard parses");
    assert_eq!(
        serde_json::to_string_pretty(&scored).expect("serialize"),
        serde_json::to_string_pretty(&recorded).expect("serialize"),
        "quality moved against docs/eval/quality.md — if the change is \
         intended, re-record with `cargo test -p agmem-server --test eval -- \
         --ignored record_baseline` and review the diff"
    );
}

/// The sensitivity canary: score retrieval with an embedder that embeds
/// nothing and demand it does strictly worse than the recorded vectors. A
/// scoring change that breaks the semantic arm — the intentionally broken
/// change of the acceptance criteria — collapses this gap before it shows
/// anywhere else, and the baseline assertion above pins the rest.
#[tokio::test]
async fn retrieval_without_vectors_scores_strictly_worse() {
    let scenarios = scenario::all();
    let mut with_vectors = 0;
    let mut without = 0;
    for scenario in &scenarios {
        with_vectors += metrics::retrieval(scenario, Arc::new(RecordedEmbedder), None)
            .await
            .found;
        without += metrics::retrieval(scenario, Arc::new(NoopEmbedder), None)
            .await
            .found;
    }
    assert!(
        without < with_vectors,
        "BM25 alone found {without} of what the recorded vectors' {with_vectors} — \
         the probes no longer exercise the semantic arm"
    );
}

/// Prints the similarity landscape the abstention floor has to separate
/// (issue #77): what the vector arm measured for every probe page — which
/// must answer — and every abstain case — which must not. The floor in
/// `tools/abstain.rs` is picked strictly between the weakest relevant top
/// hit and the strongest abstain-case measurement; if this prints no such
/// gap, the constant is not calibratable on these fixtures and the fixture
/// set needs a scenario built for it, not a fudged threshold.
#[tokio::test]
#[ignore = "diagnostic: prints the similarity spread behind the abstention floor"]
async fn calibrate_abstention() {
    use serde_json::json;
    for scenario in scenario::all() {
        for (kind, query, k) in scenario
            .probes
            .iter()
            .map(|probe| ("probe  ", probe.query.as_str(), probe.k))
            .chain(
                scenario
                    .abstain
                    .iter()
                    .map(|case| ("abstain", case.query.as_str(), case.k)),
            )
        {
            let seeded = scenario::seed(&scenario, Arc::new(RecordedEmbedder)).await;
            let answer = seeded.agmem.recall(json!({ "query": query, "k": k })).await;
            let sims: Vec<String> = answer["hits"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|hit| {
                    format!(
                        "{:.3}",
                        hit["signals"]["similarity"].as_f64().unwrap_or(f64::NAN)
                    )
                })
                .collect();
            let best = answer["cut"]["best_similarity"]
                .as_f64()
                .map_or(String::new(), |best| format!(" cut.best={best:.3}"));
            eprintln!(
                "{kind} [{}] {query:?} -> hits [{}]{best}",
                scenario.name,
                sims.join(", ")
            );
            seeded.shutdown().await;
        }
    }
}

/// The offline fusion sweep (issue #80): the full scorecard at each fixed
/// convex blend — `α` on the min–maxed fulltext score, `1 − α` on cosine
/// similarity — against the RRF baseline the committed scorecard records.
///
/// Decision rule, fixed before any number is read: hard gates are
/// `found` 10/10, `staleness.stale_hits` 0 everywhere,
/// `abstention.false_abstentions` 0, timeline ≥ 3/4 and `context`
/// passed = total; the objective is ΣMRR across the four scenarios (RRF
/// baseline 2.1527), tie-broken by lower Σ`returned`, then by RRF. A blend
/// wins only at ΣMRR ≥ +0.10 that also holds at both neighbouring α —
/// a single-α spike over nine probes is overfit. Anything else closes #80
/// as "RRF kept, measured".
#[tokio::test]
#[ignore = "measurement: runs the scorecard at eleven fusion weights"]
async fn sweep_fusion_weights() {
    let scenarios = scenario::all();
    for step in 0..=10 {
        let alpha = f64::from(step) / 10.0;
        let start = std::time::Instant::now();
        let scored =
            metrics::scorecard(&scenarios, Arc::new(RecordedEmbedder), Some(alpha)).await;
        let elapsed = start.elapsed().as_secs_f64();
        let mut mrr_sum = 0.0;
        let mut returned_sum = 0;
        for (name, score) in &scored.scenarios {
            mrr_sum += score.retrieval.mrr;
            returned_sum += score.retrieval.returned;
            eprintln!(
                "alpha={alpha:.1} [{name}] found {}/{} returned {} mrr {:.4} \
                 timeline {}/{} stale {}/{} abstain {}/{} false {} context {}/{} gate {}/{}",
                score.retrieval.found,
                score.retrieval.expected,
                score.retrieval.returned,
                score.retrieval.mrr,
                score.timeline.passed,
                score.timeline.total,
                score.staleness.stale_hits,
                score.staleness.pages,
                score.abstention.fired,
                score.abstention.expected,
                score.abstention.false_abstentions,
                score.context.passed,
                score.context.total,
                score.gate.correct,
                score.gate.total,
            );
        }
        eprintln!("alpha={alpha:.1} TOTAL mrr_sum {mrr_sum:.4} returned_sum {returned_sum} ({elapsed:.0}s)");
    }
}

/// Rewrites the scorecard block in `docs/eval/quality.md` from a fresh run.
/// Deliberate: run it, read the diff, commit both or neither.
#[tokio::test]
#[ignore = "rewrites the committed baseline in docs/eval/quality.md"]
async fn record_baseline() {
    let scenarios = scenario::all();
    let scored = metrics::scorecard(&scenarios, Arc::new(RecordedEmbedder), None).await;
    let path = quality_doc_path();
    let doc = std::fs::read_to_string(&path).expect("read docs/eval/quality.md");
    let (head, tail) = doc
        .split_once(MARKER)
        .expect("quality.md carries the scorecard marker");
    let (fence_open, rest) = tail
        .split_once("```json\n")
        .expect("a json fence follows the marker");
    let (_, after) = rest.split_once("\n```").expect("the fence closes");
    let block = serde_json::to_string_pretty(&scored).expect("serialize");
    let updated = format!("{head}{MARKER}{fence_open}```json\n{block}\n```{after}");
    std::fs::write(&path, updated).expect("write docs/eval/quality.md");
}
