//! Real inference against the real static model.
//!
//! Ignored by default: the first run downloads ~30 MB from Hugging Face,
//! which is not something CI should do on every push. Run it deliberately
//! with `cargo test -p agmem-embed --features static --test static_m2v -- --ignored`.

#![cfg(feature = "static")]

use agmem_embed::Embedder;
use agmem_embed::static_m2v::{DIM, StaticBackend};

/// Cosine similarity; the backend forces L2-normalization, so this is just a
/// dot product, but spelling it out keeps the test honest if that changes.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b))
}

#[test]
#[ignore = "downloads the model on first run"]
fn related_sentences_land_closer_than_unrelated_ones() {
    // No local bundle: the hub path, cached under HF_HOME's default.
    let embedder = StaticBackend::new(None).expect("load model");
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
