//! The default backend: BGE small English v1.5, quantized, via ONNX Runtime.
//!
//! 384 dimensions, ~30 MB of weights, no network after the first fetch. The
//! model is asymmetric — it was trained with `passage:` and `query:` markers
//! — and fastembed does not add them, so this module does (`docs/design.md`
//! §4.1).

use std::path::PathBuf;
use std::sync::Mutex;

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use crate::{EmbedError, Embedder};

/// What the store records in `meta.embedder_model`.
pub const MODEL_ID: &str = "bge-small-en-v1.5-q";

/// Vector width. Must equal the `DIMENSION` in the store's HNSW indexes.
pub const DIM: usize = 384;

/// The model fastembed downloads and runs.
const MODEL: EmbeddingModel = EmbeddingModel::BGESmallENV15Q;

/// Marks stored text, per BGE's training.
const PASSAGE_PREFIX: &str = "passage: ";
/// Marks the search side, per BGE's training.
const QUERY_PREFIX: &str = "query: ";

/// Local ONNX inference over a quantized BGE model.
pub struct FastembedBackend {
    /// `TextEmbedding::embed` takes `&mut self`, so concurrent callers queue.
    /// Embedding is CPU-bound anyway; parallelism belongs in the batch, not
    /// in the number of loaded models.
    model: Mutex<TextEmbedding>,
}

/// `TextEmbedding` is not `Debug`, and a loaded session has nothing worth
/// printing anyway — the model identity is the whole story.
impl std::fmt::Debug for FastembedBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FastembedBackend")
            .field("model", &MODEL_ID)
            .field("dim", &DIM)
            .finish()
    }
}

impl FastembedBackend {
    /// Load the model, downloading it once if the cache is empty.
    ///
    /// `fallback_cache_dir` is used only when `FASTEMBED_CACHE_DIR` is unset —
    /// the environment variable stays authoritative (CI points it somewhere
    /// unwritable on purpose, to catch an unintended download), and without
    /// either the crate would cache into `./.fastembed_cache`, relative to
    /// whatever directory the agent happened to launch the server from.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when the model cannot be fetched or loaded.
    pub fn new(fallback_cache_dir: Option<PathBuf>) -> Result<Self, EmbedError> {
        // Download progress bars are not ours to print: stdout is the MCP wire.
        let mut options = TextInitOptions::new(MODEL).with_show_download_progress(false);
        if std::env::var_os("FASTEMBED_CACHE_DIR").is_none()
            && let Some(dir) = fallback_cache_dir
        {
            options = options.with_cache_dir(dir);
        }

        tracing::info!(model = MODEL_ID, dim = DIM, "loading embedding model");
        let model = TextEmbedding::try_new(options).map_err(failed)?;
        Ok(Self {
            model: Mutex::new(model),
        })
    }

    /// Embed already-prefixed texts, checking the width the store depends on.
    fn embed_all(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut model = self.model.lock().map_err(|_| EmbedError::Backend {
            backend: MODEL_ID,
            message: "the model lock was poisoned by an earlier panic".to_owned(),
        })?;
        let vectors = model.embed(texts, None).map_err(failed)?;
        drop(model);

        if let Some(wrong) = vectors.iter().find(|vector| vector.len() != DIM) {
            return Err(EmbedError::Backend {
                backend: MODEL_ID,
                message: format!(
                    "model returned {}-dimensional vectors, expected {DIM}",
                    wrong.len()
                ),
            });
        }
        Ok(vectors)
    }
}

impl Embedder for FastembedBackend {
    fn dim(&self) -> usize {
        DIM
    }

    fn model_id(&self) -> &str {
        MODEL_ID
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        self.embed_all(
            passages
                .iter()
                .map(|passage| format!("{PASSAGE_PREFIX}{passage}"))
                .collect(),
        )
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        let mut vectors = self.embed_all(vec![format!("{QUERY_PREFIX}{query}")])?;
        vectors.pop().ok_or_else(|| EmbedError::Backend {
            backend: MODEL_ID,
            message: "model returned no vector for the query".to_owned(),
        })
    }
}

/// fastembed's error type is `#[non_exhaustive]`; keep the text, drop the type.
fn failed(error: fastembed::Error) -> EmbedError {
    EmbedError::Backend {
        backend: MODEL_ID,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Metadata only — no download, so this runs everywhere, including CI.
    #[test]
    fn the_declared_dimension_matches_the_model() {
        let info = TextEmbedding::get_model_info(&MODEL).expect("model info");
        assert_eq!(info.dim, DIM, "schema HNSW dimension would be wrong");
    }
}
