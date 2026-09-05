//! The #81 stage-0 probe: score every eval-fixture query against every
//! fixture passage with the real cross-encoder, commit the map, and print
//! the two blocks `docs/eval/rerank-probe.md` records — the abstention
//! decision table and the latency budget. The decision rule lives in that
//! document and was committed before this ran.

#![cfg(feature = "rerank")]

use std::collections::BTreeMap;
use std::time::Instant;

use agmem_embed::rerank::{FastembedReranker, MODEL_ID};

fn sigmoid(logit: f64) -> f64 {
    1.0 / (1.0 + (-logit).exp())
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

/// One scenario's material: what may be asked, against what could answer.
struct Material {
    name: String,
    /// `(kind, query)` — `probe` pages must answer, `abstain` must not,
    /// `other` is recorded for a future `RecordedReranker` replay.
    queries: Vec<(&'static str, String)>,
    /// Seed contents and episode texts, the whole candidate surface.
    passages: Vec<String>,
}

fn strings_of(section: &serde_json::Value, field: &str) -> Vec<String> {
    section
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry[field].as_str())
        .map(str::to_owned)
        .collect()
}

fn materials() -> Vec<Material> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agmem-server/tests/fixtures/eval/scenarios");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .expect("read the scenario dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let raw = std::fs::read_to_string(path).expect("read scenario");
            let scenario: serde_json::Value = serde_json::from_str(&raw).expect("parse scenario");
            let mut queries: Vec<(&'static str, String)> = Vec::new();
            for query in strings_of(&scenario["probes"], "query") {
                queries.push(("probe", query));
            }
            for query in strings_of(&scenario["abstain"], "query") {
                queries.push(("abstain", query));
            }
            for section in ["timeline", "temporal", "context"] {
                for query in strings_of(&scenario[section], "query") {
                    if !queries.iter().any(|(_, seen)| *seen == query) {
                        queries.push(("other", query));
                    }
                }
            }
            let mut passages = strings_of(&scenario["seeds"], "content");
            passages.extend(strings_of(&scenario["seeds"], "episode"));
            Material {
                name: scenario["name"].as_str().expect("a name").to_owned(),
                queries,
                passages,
            }
        })
        .collect()
}

/// Runs the real model. Downloads ~150 MB on the first run; deliberate.
#[test]
#[ignore = "downloads and runs the real reranker; writes a committed fixture"]
fn record_rerank_scores() {
    let cache = std::env::temp_dir().join("agmem-model-cache");
    let loading = Instant::now();
    let reranker = FastembedReranker::new(Some(cache.clone())).expect("load the reranker");
    let load_ms = loading.elapsed().as_secs_f64() * 1000.0;

    let materials = materials();
    // BTreeMaps so the committed file diffs by text, as vectors.json does.
    let mut pairs: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    eprintln!("== decision table (sigmoid of the page's best logit) ==");
    for material in &materials {
        for (kind, query) in &material.queries {
            let scores = reranker
                .scores(query, &material.passages, None)
                .expect("score the scenario");
            let best = scores
                .iter()
                .copied()
                .max_by(f64::total_cmp)
                .expect("scenarios have passages");
            if *kind != "other" {
                eprintln!(
                    "{kind} [{}] best {:.4} (logit {:+.3}) {query:?}",
                    material.name,
                    sigmoid(best),
                    best,
                );
            }
            let slot = pairs.entry(query.clone()).or_default();
            for (passage, score) in material.passages.iter().zip(&scores) {
                slot.insert(passage.clone(), *score);
            }
        }
    }

    // The latency block: a 30-candidate page per call, the shape the issue
    // names, against the one-off load and the embedder call a recall
    // already pays.
    let page: Vec<String> = materials
        .iter()
        .flat_map(|material| material.passages.iter().cloned())
        .cycle()
        .take(30)
        .collect();
    let question = "how do releases go out";
    for batch in [8_usize, 16, 32] {
        let runs: Vec<f64> = (0..5)
            .map(|_| {
                let start = Instant::now();
                reranker
                    .scores(question, &page, Some(batch))
                    .expect("score the page");
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect();
        eprintln!(
            "latency: 30 candidates, batch {batch}: p50 {:.0} ms",
            median(runs)
        );
    }
    let embedder =
        agmem_embed::fastembed::FastembedBackend::new(Some(cache), agmem_embed::Accelerator::Cpu)
            .expect("load the embedder");
    let embeds: Vec<f64> = (0..5)
        .map(|_| {
            let start = Instant::now();
            agmem_embed::Embedder::embed_query(&embedder, question).expect("embed");
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    eprintln!(
        "latency: model load {load_ms:.0} ms one-off; embed_query p50 {:.0} ms",
        median(embeds)
    );

    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agmem-server/tests/fixtures/eval/rerank.json");
    let document = serde_json::json!({ "model": MODEL_ID, "pairs": pairs });
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&document).expect("serialize") + "\n",
    )
    .expect("write fixture");
    eprintln!("wrote {}", out.display());
}
