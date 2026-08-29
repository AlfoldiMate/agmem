//! Real inference against the real model.
//!
//! Ignored by default: the first run downloads ~30 MB from Hugging Face, which
//! is not something CI should do on every push. Run it deliberately with
//! `cargo test -p agmem-embed --test fastembed -- --ignored`.

#![cfg(feature = "onnx")]

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
