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
    let embedder =
        FastembedBackend::new(Some(cache), agmem_embed::Accelerator::Cpu).expect("load model");
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
    let embedder =
        FastembedBackend::new(Some(cache), agmem_embed::Accelerator::Cpu).expect("load model");

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
/// The fixture documents (issue #137) are recorded to a second file,
/// `documents-vectors.json`, chunked with the same `agmem_core::chunk::chunk`
/// that `remember` applies to an episode — the store asks the embedder for
/// chunk texts, never the whole document, and a chunk the recording lacks
/// panics the replay. Seed episodes are chunked the same way for the same
/// reason. Two files so a scenario edit diffs as the strings it touched
/// rather than rewriting the corpus's ~0.75 MB of vectors.
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

    /// One recording file: passages and queries keyed by text, sorted.
    fn write_recording(
        path: &std::path::Path,
        model: &str,
        dim: usize,
        passages: &BTreeMap<String, Vec<f32>>,
        queries: &BTreeMap<String, Vec<f32>>,
    ) {
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
            "{{\n  \"model\": {},\n  \"dim\": {},\n  \
             \"passages\": {{\n{}\n  }},\n  \"queries\": {{\n{}\n  }}\n}}\n",
            serde_json::to_string(model).expect("encode model"),
            dim,
            passage_entries.join(",\n"),
            query_entries.join(",\n")
        );
        std::fs::create_dir_all(path.parent().expect("fixture dir")).expect("create fixture dir");
        std::fs::write(path, document).expect("write fixture");
    }

    /// Embeds every key of `passages` in place, in one batch.
    fn fill_passages(embedder: &dyn Embedder, passages: &mut BTreeMap<String, Vec<f32>>) {
        let texts: Vec<String> = passages.keys().cloned().collect();
        let vectors = embedder.embed_passages(&texts).expect("embed passages");
        for (text, vector) in texts.iter().zip(vectors) {
            passages.insert(text.clone(), vector);
        }
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
            .chain(strings_of(&scenario["gate"], "candidate"))
            .chain(
                strings_of(&scenario["seeds"], "episode")
                    .iter()
                    .flat_map(|episode| agmem_core::chunk::chunk(episode)),
            )
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

    // The fixture corpus: every chunk of every document the manifest lists.
    let documents_dir = eval_dir.join("documents");
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(documents_dir.join("manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    let mut document_chunks: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    for file in strings_of(&manifest, "file") {
        let text = std::fs::read_to_string(documents_dir.join(&file))
            .unwrap_or_else(|error| panic!("read fixture document {file}: {error}"));
        for chunk in agmem_core::chunk::chunk(&text) {
            document_chunks.insert(chunk, Vec::new());
        }
    }
    assert!(
        !document_chunks.is_empty(),
        "the manifest lists no documents"
    );

    // The shipped model writes the committed fixtures; under the `candidates`
    // feature, `AGMEM_CANDIDATE` records the same texts with a #133 candidate
    // into a gitignored sibling directory the eval reads through
    // `AGMEM_EVAL_VECTORS_DIR` (docs/eval/embed-models.md).
    let (embedder, out_dir, model): (Box<dyn Embedder>, std::path::PathBuf, String) =
        match candidate_recorder() {
            Some((embedder, id)) => (embedder, eval_dir.join("candidates").join(&id), id),
            None => {
                let cache = std::env::temp_dir().join("agmem-model-cache");
                let embedder = FastembedBackend::new(Some(cache), agmem_embed::Accelerator::Cpu)
                    .expect("load model");
                (
                    Box::new(embedder),
                    eval_dir.clone(),
                    "BGE-small-en-v1.5-q".to_owned(),
                )
            }
        };
    let dim = embedder.dim();

    fill_passages(embedder.as_ref(), &mut passages);
    for (text, slot) in &mut queries {
        *slot = embedder.embed_query(text).expect("embed query");
    }
    write_recording(
        &out_dir.join("vectors.json"),
        &model,
        dim,
        &passages,
        &queries,
    );

    fill_passages(embedder.as_ref(), &mut document_chunks);
    write_recording(
        &out_dir.join("documents-vectors.json"),
        &model,
        dim,
        &document_chunks,
        &BTreeMap::new(),
    );
    eprintln!("recorded {model} ({dim}d) into {}", out_dir.display());
}

/// The #133 candidate `AGMEM_CANDIDATE` names, loaded, with its id; `None`
/// when unset or when the feature is off.
#[cfg(feature = "candidates")]
fn candidate_recorder() -> Option<(Box<dyn Embedder>, String)> {
    use agmem_embed::candidates::{Candidate, CandidateBackend, cache_dir};
    let candidate = Candidate::from_env()?;
    let backend = CandidateBackend::load(candidate, &cache_dir(), agmem_embed::Active::Cpu)
        .expect("load candidate");
    Some((Box::new(backend), candidate.id().to_owned()))
}

#[cfg(not(feature = "candidates"))]
fn candidate_recorder() -> Option<(Box<dyn Embedder>, String)> {
    None
}

/// The #139 drift check: the accelerated session must reproduce the CPU
/// vectors closely enough that no threshold moves. Every fixture text —
/// passages and queries alike — is embedded on both providers, and the
/// smallest cosine between the pairs has to clear the bar in
/// `docs/eval/coreml-ep.md`.
#[cfg(feature = "coreml")]
#[test]
#[ignore = "runs the real model on the CoreML execution provider"]
fn coreml_vectors_match_cpu() {
    use agmem_embed::Accelerator;

    const BAR: f32 = 0.999;

    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../agmem-server/tests/fixtures/eval/vectors.json");
    let recording: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&fixture).expect("read vectors.json"))
            .expect("vectors.json parses");
    let texts = |section: &str| -> Vec<String> {
        recording[section]
            .as_object()
            .expect(section)
            .keys()
            .cloned()
            .collect()
    };
    let passages = texts("passages");
    let queries = texts("queries");
    assert!(!passages.is_empty() && !queries.is_empty(), "empty fixture");

    let cache = std::env::temp_dir().join("agmem-model-cache");
    let cpu = FastembedBackend::new(Some(cache.clone()), Accelerator::Cpu).expect("load on cpu");
    let coreml = FastembedBackend::new(Some(cache), Accelerator::CoreMl).expect("load on coreml");
    assert_eq!(coreml.accelerator(), "coreml");

    let mut cosines: Vec<f32> = Vec::with_capacity(passages.len() + queries.len());
    let a = cpu.embed_passages(&passages).expect("cpu passages");
    let b = coreml.embed_passages(&passages).expect("coreml passages");
    cosines.extend(a.iter().zip(&b).map(|(x, y)| cosine(x, y)));
    for query in &queries {
        let x = cpu.embed_query(query).expect("cpu query");
        let y = coreml.embed_query(query).expect("coreml query");
        cosines.push(cosine(&x, &y));
    }

    let min = cosines.iter().copied().fold(f32::INFINITY, f32::min);
    let mean = cosines.iter().sum::<f32>() / cosines.len() as f32;
    eprintln!(
        "coreml vs cpu over {} texts: min cosine {min:.6}, mean {mean:.6}",
        cosines.len()
    );
    assert!(min >= BAR, "min cosine {min:.6} is under the {BAR} bar");
}
