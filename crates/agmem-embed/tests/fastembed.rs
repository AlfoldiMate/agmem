//! Real inference against the real model.
//!
//! Ignored by default: the first run downloads ~30 MB from Hugging Face, which
//! is not something CI should do on every push. Run it deliberately with
//! `cargo test -p agmem-embed --test fastembed -- --ignored`.

use agmem_embed::Embedder;
use agmem_embed::fastembed::{DIM, FastembedBackend};

/// Cosine similarity; the model L2-normalises, so this is just a dot product,
/// but spelling it out keeps the test honest if that ever changes.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b))
}

#[test]
#[ignore = "downloads the model on first run"]
fn related_sentences_land_closer_than_unrelated_ones() {
    // A cache under the temp dir, not `./.fastembed_cache` in whatever
    // directory the test runner happened to start in.
    let cache = std::env::temp_dir().join("agmem-model-cache");
    let embedder = FastembedBackend::new(Some(cache)).expect("load model");
    assert_eq!(embedder.dim(), DIM);

    let passages = vec![
        "The user prefers Rust over Python for systems work.".to_owned(),
        "Rust is this developer's language of choice for low-level code.".to_owned(),
        "The kitchen tap has been dripping since Tuesday.".to_owned(),
    ];
    let vectors = embedder.embed_passages(&passages).expect("embed passages");
    assert_eq!(vectors.len(), 3);
    assert!(vectors.iter().all(|vector| vector.len() == DIM));

    let related = cosine(&vectors[0], &vectors[1]);
    let unrelated = cosine(&vectors[0], &vectors[2]);
    assert!(
        related > unrelated,
        "paraphrase {related} should beat unrelated {unrelated}"
    );

    let query = embedder
        .embed_query("what language does the user like?")
        .expect("embed query");
    assert!(
        cosine(&query, &vectors[0]) > cosine(&query, &vectors[2]),
        "the query must find the language memory, not the tap"
    );
}

/// Regenerates the KNN under-return fixture the store tests probe with
/// (issue #40).
///
/// The bug is **probe-vector dependent** — a stored row's own embedding or a
/// random vector returns the full candidate set, and a real BGE query
/// embedding does not — so the reproduction needs real model output for both
/// the rows and the question, captured once and committed. Everything here is
/// data, not behaviour: nothing is asserted, and the store test that reads the
/// file is where the claim lives.
///
/// Run with `cargo test -p agmem-embed --test fastembed -- --ignored
/// regenerate_knn_fixture`. The rows are the two live memories in the repro
/// store at `~/.local/share/agmem-repro/recall-omits-live-row/`, verbatim.
#[test]
#[ignore = "writes a committed fixture from real model output"]
fn regenerate_knn_fixture() {
    /// Shortest-round-tripping decimal for each component, so f32 → text →
    /// f64 → f32 is exact both ways.
    fn json_vector(vector: &[f32]) -> String {
        let components: Vec<String> = vector.iter().map(|x| format!("{x}")).collect();
        format!("[{}]", components.join(","))
    }

    let cache = std::env::temp_dir().join("agmem-model-cache");
    let embedder = FastembedBackend::new(Some(cache)).expect("load model");

    let rows = [
        "The user formats Python with black.",
        "This project has moved off black for Python code formatting and now uses \
         ruff format instead; black is uninstalled.",
    ];
    // Questions sharing no word with either row: the fulltext arm is empty for
    // them by construction, which is what isolates the vector arm (issue #39).
    let queries = [
        "which tool tidies up source layout automatically",
        "how is our source styled these days",
        "what did we switch our layout helper to",
    ];

    let passages: Vec<String> = rows.iter().map(|row| (*row).to_owned()).collect();
    let vectors = embedder.embed_passages(&passages).expect("embed passages");

    let mut entries: Vec<String> = Vec::new();
    for (row, vector) in rows.iter().zip(&vectors) {
        entries.push(format!(
            "    {{ \"content\": {row:?}, \"embedding\": {} }}",
            json_vector(vector)
        ));
    }
    let mut probes: Vec<String> = Vec::new();
    for query in queries {
        let vector = embedder.embed_query(query).expect("embed query");
        probes.push(format!(
            "    {{ \"text\": {query:?}, \"embedding\": {} }}",
            json_vector(&vector)
        ));
    }

    let document = format!(
        "{{\n  \"model\": \"BGE-small-en-v1.5-q\",\n  \"dim\": {},\n  \
         \"rows\": [\n{}\n  ],\n  \"queries\": [\n{}\n  ]\n}}\n",
        DIM,
        entries.join(",\n"),
        probes.join(",\n")
    );

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agmem-server/tests/fixtures/knn_underreturn.json");
    std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixture dir");
    std::fs::write(&path, document).expect("write fixture");
}

/// Regenerates the eval harness's recorded vectors (issue #32).
///
/// The quality eval in `agmem-server/tests/eval.rs` replays scripted sessions
/// through a `RecordedEmbedder` so its numbers carry real BGE semantics while
/// staying bit-stable and offline. This is the recorder: it walks the eval
/// scenario fixtures, embeds every passage and query they use, and commits
/// the result as one JSON map. Run it after any edit to a scenario file —
/// an unrecorded string makes `RecordedEmbedder` panic, deliberately, rather
/// than fall back to a zero vector that would read as a scoring regression.
///
/// Run with `cargo test -p agmem-embed --test fastembed -- --ignored
/// regenerate_eval_vectors`.
#[test]
#[ignore = "writes a committed fixture from real model output"]
fn regenerate_eval_vectors() {
    use std::collections::BTreeMap;

    /// Shortest-round-tripping decimal for each component, so f32 → text →
    /// f64 → f32 is exact both ways.
    fn json_vector(vector: &[f32]) -> String {
        let components: Vec<String> = vector.iter().map(|x| format!("{x}")).collect();
        format!("[{}]", components.join(","))
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

    let eval_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agmem-server/tests/fixtures/eval");
    let mut scenarios: Vec<std::path::PathBuf> = std::fs::read_dir(eval_dir.join("scenarios"))
        .expect("read the scenario dir")
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    scenarios.sort();
    assert!(!scenarios.is_empty(), "no scenario fixtures to record");

    // BTreeMaps so the committed file is ordered by text, not by scenario —
    // a fixture edit diffs as the strings it touched.
    let mut passages: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    let mut queries: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    for path in &scenarios {
        let raw = std::fs::read_to_string(path).expect("read scenario");
        let scenario: serde_json::Value = serde_json::from_str(&raw).expect("parse scenario");
        for text in strings_of(&scenario["seeds"], "content")
            .into_iter()
            .chain(strings_of(&scenario["seeds"], "episode"))
            .chain(strings_of(&scenario["gate"], "candidate"))
        {
            passages.insert(text, Vec::new());
        }
        for text in strings_of(&scenario["probes"], "query")
            .into_iter()
            .chain(strings_of(&scenario["abstain"], "query"))
            .chain(strings_of(&scenario["temporal"], "query"))
            .chain(strings_of(&scenario["timeline"], "query"))
            .chain(strings_of(&scenario["context"], "query"))
        {
            queries.insert(text, Vec::new());
        }
    }

    let cache = std::env::temp_dir().join("agmem-model-cache");
    let embedder = FastembedBackend::new(Some(cache)).expect("load model");

    let passage_texts: Vec<String> = passages.keys().cloned().collect();
    let vectors = embedder
        .embed_passages(&passage_texts)
        .expect("embed passages");
    for (text, vector) in passage_texts.iter().zip(vectors) {
        passages.insert(text.clone(), vector);
    }
    for (text, slot) in &mut queries {
        *slot = embedder.embed_query(text).expect("embed query");
    }

    let entry = |(text, vector): (&String, &Vec<f32>)| {
        format!(
            "    {}: {}",
            serde_json::to_string(text).expect("encode text"),
            json_vector(vector)
        )
    };
    let passage_entries: Vec<String> = passages.iter().map(entry).collect();
    let query_entries: Vec<String> = queries.iter().map(entry).collect();
    let document = format!(
        "{{\n  \"model\": \"BGE-small-en-v1.5-q\",\n  \"dim\": {},\n  \
         \"passages\": {{\n{}\n  }},\n  \"queries\": {{\n{}\n  }}\n}}\n",
        DIM,
        passage_entries.join(",\n"),
        query_entries.join(",\n")
    );
    std::fs::write(eval_dir.join("vectors.json"), document).expect("write fixture");
}
