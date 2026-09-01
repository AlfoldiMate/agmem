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
    let scored = metrics::scorecard(&scenarios, Arc::new(RecordedEmbedder)).await;
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
        with_vectors += metrics::retrieval(scenario, Arc::new(RecordedEmbedder))
            .await
            .found;
        without += metrics::retrieval(scenario, Arc::new(NoopEmbedder))
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

/// Prints every temporal check's full page — rank, score, temporal fit,
/// label-ish content — for tuning issue #78's checks and weight. Diagnostic
/// only, like `calibrate_abstention`.
#[tokio::test]
#[ignore = "diagnostic: prints the pages behind the temporal checks"]
async fn calibrate_temporal() {
    use serde_json::json;
    for scenario in scenario::all() {
        for check in &scenario.temporal {
            let seeded = scenario::seed(&scenario, Arc::new(RecordedEmbedder)).await;
            let mut arguments = json!({ "query": check.query, "k": 10 });
            for (field, value) in [
                ("since", &check.since),
                ("until", &check.until),
                ("changed_since", &check.changed_since),
            ] {
                if let Some(stamp) = value {
                    arguments[field] = json!(stamp);
                }
            }
            if check.include_invalidated {
                arguments["include_invalidated"] = json!(true);
            }
            let answer = seeded.agmem.recall(arguments).await;
            eprintln!(
                "[{}] {:?} since={:?} until={:?} changed={:?} expect_top={}",
                scenario.name,
                check.query,
                check.since,
                check.until,
                check.changed_since,
                check.expect_top
            );
            for hit in answer["hits"].as_array().into_iter().flatten() {
                eprintln!(
                    "  score {:.4} rrf_n {:.3} sim {:?} fit {:?} imp {:.2} | {}",
                    hit["score"].as_f64().unwrap_or(f64::NAN),
                    hit["signals"]["rrf_normalized"]
                        .as_f64()
                        .unwrap_or(f64::NAN),
                    hit["signals"]["similarity"].as_f64(),
                    hit["signals"]["temporal"].as_f64(),
                    hit["signals"]["importance"].as_f64().unwrap_or(f64::NAN),
                    hit["content"].as_str().unwrap_or("?"),
                );
            }
            if !answer["cut"].is_null() {
                eprintln!("  cut: {}", answer["cut"]);
            }
            seeded.shutdown().await;
        }
    }
}

/// Rewrites the scorecard block in `docs/eval/quality.md` from a fresh run.
/// Deliberate: run it, read the diff, commit both or neither.
#[tokio::test]
#[ignore = "rewrites the committed baseline in docs/eval/quality.md"]
async fn record_baseline() {
    let scenarios = scenario::all();
    let scored = metrics::scorecard(&scenarios, Arc::new(RecordedEmbedder)).await;
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
