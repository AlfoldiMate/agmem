//! A test double that produces no vectors.
//!
//! Not a supported deployment — agmem is BM25 plus a local model, always. It
//! exists for tests that need a running server where a model download is
//! forbidden (CI): the subprocess tests reach it as the hidden
//! `--embedder none`, and the in-process protocol harness replays recorded
//! real-model vectors instead. Rows written through it carry no vector, and
//! the HNSW indexes skip them, so a later backend costs a re-embed of the old
//! rows but breaks nothing.

use crate::{EmbedError, Embedder};

/// Produces no vectors at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopEmbedder;

/// What [`Embedder::model_id`] reports for this backend, and what the store
/// refuses to record in `meta` — "none" claims no vector space.
pub const MODEL_ID: &str = "none";

impl Embedder for NoopEmbedder {
    fn dim(&self) -> usize {
        0
    }

    fn model_id(&self) -> &str {
        MODEL_ID
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(vec![Vec::new(); passages.len()])
    }

    fn embed_query(&self, _query: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn produces_one_empty_vector_per_passage() {
        let embedder = NoopEmbedder;
        let passages = vec!["a".to_owned(), "b".to_owned()];

        assert_eq!(embedder.dim(), 0);
        assert_eq!(
            embedder.embed_passages(&passages).unwrap(),
            vec![Vec::<f32>::new(), Vec::new()]
        );
        assert!(embedder.embed_query("anything").unwrap().is_empty());
    }

    #[tokio::test]
    async fn works_through_the_blocking_wrappers() {
        let embedder: Arc<dyn Embedder> = Arc::new(NoopEmbedder);

        let passages = crate::embed_passages(Arc::clone(&embedder), vec!["a".to_owned()])
            .await
            .expect("embed passages");
        assert_eq!(passages, vec![Vec::<f32>::new()]);
        assert!(
            crate::embed_query(embedder, "q".to_owned())
                .await
                .expect("embed query")
                .is_empty()
        );
    }
}
