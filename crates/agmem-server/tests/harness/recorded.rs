//! An embedder that replays committed real-BGE vectors (issue #32).
//!
//! The quality eval wants real model semantics — whether "which tool tidies
//! up our source code layout?" lands nearer the ruff claim than the pizza
//! order is exactly what it measures — but a live model download in a scoring
//! test would make the baseline numbers network-dependent and, across ONNX
//! releases, drift-prone. So the vectors are recorded once from the real
//! model (`regenerate_eval_vectors` in `agmem-embed/tests/fastembed.rs`) and
//! committed, the same pattern as `fixtures/knn_underreturn.json`: real
//! semantics, bit-stable, offline.

use std::collections::HashMap;
use std::sync::OnceLock;

use agmem_embed::{EmbedError, Embedder};
use serde::Deserialize;

/// The command that refreshes `vectors.json` after a scenario edit.
const REGENERATE: &str =
    "cargo test -p agmem-embed --test fastembed -- --ignored regenerate_eval_vectors";

/// The committed output of `regenerate_eval_vectors`: every passage and query
/// the eval scenarios use, embedded by the real model. Passages and queries
/// are separate maps because BGE embeds them differently (queries carry the
/// model's query prefix), so the same text can legitimately have two vectors.
#[derive(Deserialize)]
struct Recording {
    model: String,
    dim: usize,
    passages: HashMap<String, Vec<f32>>,
    queries: HashMap<String, Vec<f32>>,
}

fn recording() -> &'static Recording {
    static RECORDING: OnceLock<Recording> = OnceLock::new();
    RECORDING.get_or_init(|| {
        serde_json::from_str(include_str!("../fixtures/eval/vectors.json"))
            .expect("fixtures/eval/vectors.json parses")
    })
}

/// Text that was never recorded is a fixture bug, not a retrieval result — a
/// silent zero vector here would score as a retrieval regression and send
/// whoever reads the diff hunting through the ranking instead of the fixture.
fn replay(kind: &str, map: &'static HashMap<String, Vec<f32>>, text: &str) -> Vec<f32> {
    map.get(text)
        .unwrap_or_else(|| panic!("no recorded {kind} vector for {text:?}; run `{REGENERATE}`"))
        .clone()
}

/// Replays the recorded vectors; panics on any text the recording lacks.
#[derive(Debug, Clone, Copy)]
pub struct RecordedEmbedder;

impl Embedder for RecordedEmbedder {
    fn dim(&self) -> usize {
        recording().dim
    }

    fn model_id(&self) -> &str {
        &recording().model
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(passages
            .iter()
            .map(|text| replay("passage", &recording().passages, text))
            .collect())
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(replay("query", &recording().queries, query))
    }
}
