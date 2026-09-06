//! Real calls against a real endpoint (issue #120).
//!
//! Ignored by default: every run costs a network round trip and, on a hosted
//! provider, money. Run deliberately with
//! `cargo test -p agmem-embed --test api -- --ignored`, with `AGMEM_API_URL`
//! (default OpenAI), `AGMEM_API_MODEL` (default `text-embedding-3-small`)
//! and, where the endpoint wants one, `AGMEM_API_KEY` in the environment.

use agmem_embed::{ApiEmbedder, Embedder};

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn connect() -> ApiEmbedder {
    let url =
        std::env::var("AGMEM_API_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());
    let model =
        std::env::var("AGMEM_API_MODEL").unwrap_or_else(|_| "text-embedding-3-small".to_owned());
    let key = std::env::var("AGMEM_API_KEY")
        .ok()
        .filter(|k| !k.is_empty());
    ApiEmbedder::new(&url, &model, key).expect("connect to the endpoint")
}

#[test]
#[ignore = "needs a live endpoint and, usually, a key"]
fn related_sentences_land_closer_than_unrelated_ones() {
    let embedder = connect();
    assert!(embedder.dim() > 0);
    assert!(embedder.model_id().starts_with("api:"));

    let passages = vec![
        "The build breaks when the cache directory is read-only.".to_owned(),
        "A read-only cache dir makes the build fail.".to_owned(),
        "The cat slept in the sun all afternoon.".to_owned(),
    ];
    let vectors = embedder.embed_passages(&passages).expect("embed passages");
    assert_eq!(vectors.len(), 3);
    for vector in &vectors {
        assert_eq!(vector.len(), embedder.dim());
        let norm: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "unit length, got {norm}");
    }

    let query = embedder
        .embed_query("why does the build fail with a read-only cache?")
        .expect("embed query");
    let paraphrase = cosine(&vectors[0], &vectors[1]);
    let unrelated = cosine(&vectors[0], &vectors[2]);
    let hit = cosine(&query, &vectors[0]);
    let miss = cosine(&query, &vectors[2]);
    eprintln!(
        "{}: paraphrase {paraphrase:.3} / unrelated {unrelated:.3}; query hit {hit:.3} / miss \
         {miss:.3}",
        embedder.model_id()
    );
    assert!(paraphrase > unrelated);
    assert!(hit > miss);
}
