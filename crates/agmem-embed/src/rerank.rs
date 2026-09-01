//! The cross-encoder behind the #81 rerank probe: jina-reranker-v1-turbo-en
//! over ONNX, via fastembed's `TextRerank`.
//!
//! Measurement surface only, for now. Nothing in the server constructs this;
//! the ignored recorder in `tests/rerank.rs` is the one caller, and
//! `docs/eval/rerank-probe.md` is where its numbers decide whether a
//! production path gets built at all.

use std::path::PathBuf;
use std::sync::Mutex;

use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

use crate::EmbedError;

/// What a store would record if this model ever wrote one.
pub const MODEL_ID: &str = "jina-reranker-v1-turbo-en";

/// The model fastembed downloads and runs. Unquantized ONNX, ~150 MB — five
/// times BGE-small-q, which is part of what the probe's verdict weighs.
const MODEL: RerankerModel = RerankerModel::JINARerankerV1TurboEn;

/// Local cross-encoder scoring of (query, passage) pairs.
pub struct FastembedReranker {
    /// `TextRerank::rerank` takes `&mut self`, so concurrent callers queue —
    /// the same shape as [`crate::fastembed::FastembedBackend`], for the
    /// same reason.
    model: Mutex<TextRerank>,
}

impl std::fmt::Debug for FastembedReranker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FastembedReranker")
            .field("model", &MODEL_ID)
            .finish()
    }
}

impl FastembedReranker {
    /// Load the model, downloading it once if the cache is empty.
    ///
    /// `fallback_cache_dir` behaves exactly as the embedder's does:
    /// `FASTEMBED_CACHE_DIR` stays authoritative (CI points it somewhere
    /// unwritable to catch an unintended download), and progress bars stay
    /// off — stdout is never ours.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when the model cannot be fetched or loaded.
    pub fn new(fallback_cache_dir: Option<PathBuf>) -> Result<Self, EmbedError> {
        let mut options = RerankInitOptions::new(MODEL).with_show_download_progress(false);
        if std::env::var_os("FASTEMBED_CACHE_DIR").is_none()
            && let Some(dir) = fallback_cache_dir
        {
            options = options.with_cache_dir(dir);
        }
        let model = TextRerank::try_new(options).map_err(|error| EmbedError::Backend {
            backend: MODEL_ID,
            message: error.to_string(),
        })?;
        Ok(Self {
            model: Mutex::new(model),
        })
    }

    /// The raw relevance logit for each passage against `query`, in the
    /// order the passages were given.
    ///
    /// Logits, not probabilities: fastembed hands back the model's head
    /// unsigmoided and unbounded, and the caller calibrating a threshold
    /// should say which unit it chose. `batch` sizes the ONNX batches;
    /// `None` takes fastembed's default.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] for anything ONNX or the tokenizer rejects.
    pub fn scores(
        &self,
        query: &str,
        passages: &[String],
        batch: Option<usize>,
    ) -> Result<Vec<f64>, EmbedError> {
        if passages.is_empty() {
            return Ok(Vec::new());
        }
        let mut model = self.model.lock().map_err(|_| EmbedError::Backend {
            backend: MODEL_ID,
            message: "a previous caller panicked while scoring".to_owned(),
        })?;
        // `rerank` infers one `S` for query and documents both, so the
        // passages travel as `&str` beside the `&str` query.
        let passages: Vec<&str> = passages.iter().map(String::as_str).collect();
        let ranked = model
            .rerank(query, &passages, false, batch)
            .map_err(|error| EmbedError::Backend {
                backend: MODEL_ID,
                message: error.to_string(),
            })?;
        let mut scores = vec![0.0; passages.len()];
        for result in ranked {
            scores[result.index] = f64::from(result.score);
        }
        Ok(scores)
    }
}
