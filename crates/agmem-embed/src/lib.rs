//! agmem embedding backends.
//!
//! A narrow [`Embedder`] trait with one real implementation, fastembed/ONNX,
//! and a no-op test double that produces no vectors. Nothing here touches the
//! network at runtime once the model is cached; see `docs/design.md` §4.
//!
//! Backends are synchronous — ONNX inference is CPU-bound, and pretending
//! otherwise would only hide it. The async wrappers [`embed_passages`] and
//! [`embed_query`] move that work off the runtime with `spawn_blocking`, so
//! the MCP server never stalls its reactor on a model; [`embed_passages`]
//! also slices large batches so one caller cannot hold the model for the
//! whole batch while every other session waits.

use std::sync::Arc;

pub mod accelerator;
#[cfg(feature = "candidates")]
pub mod candidates;
pub mod fastembed;
pub mod noop;
#[cfg(feature = "rerank")]
pub mod rerank;

pub use accelerator::{Accelerator, Active};
pub use noop::NoopEmbedder;

/// Turns text into vectors, one backend at a time.
///
/// Implementations are blocking and must be usable from several tasks at
/// once; the server holds exactly one behind an [`Arc`] for the process.
pub trait Embedder: Send + Sync + 'static {
    /// Width of the vectors this backend produces.
    ///
    /// Zero means the backend produces none at all (the [`NoopEmbedder`] test
    /// double) — callers store `embedding: NONE` and skip the vector half of
    /// retrieval.
    fn dim(&self) -> usize;

    /// Stable model identifier, recorded in `meta` so a later run cannot
    /// silently mix vector spaces.
    fn model_id(&self) -> &str;

    /// The execution provider the model runs on — `cpu` unless a backend
    /// registered another (`docs/design.md` §4; issue #139). Printed by
    /// `doctor` and the startup log; never stored, since the vectors are
    /// the same modulo accelerator drift the fixtures check.
    fn accelerator(&self) -> &str {
        "cpu"
    }

    /// Embed documents for storage, in input order.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when the model fails to load or run.
    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;

    /// Embed a single query.
    ///
    /// Asymmetric models want queries and passages marked differently, so this
    /// is a separate call rather than a one-element batch.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when the model fails to load or run.
    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError>;
}

/// Failures from an embedding backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EmbedError {
    /// The backend could not load its model or run inference.
    #[error("embedder {backend}: {message}")]
    Backend {
        /// Which backend failed.
        backend: &'static str,
        /// What it said.
        message: String,
    },

    /// The blocking embedding task panicked or was cancelled.
    #[error("embedding task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

/// How many passages reach the backend per call.
///
/// Backends serialise on one model, so a caller's batch is sliced here and the
/// backend invoked once per slice — between slices the model is free, and
/// another session's query gets in instead of waiting out the whole batch.
const BATCH: usize = 128;

/// Embed passages on a blocking thread, [`BATCH`] at a time.
///
/// # Errors
/// Whatever the backend reports, or [`EmbedError::Join`] if the task died.
pub async fn embed_passages(
    embedder: Arc<dyn Embedder>,
    passages: Vec<String>,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    tokio::task::spawn_blocking(move || {
        let mut vectors = Vec::with_capacity(passages.len());
        for slice in passages.chunks(BATCH) {
            vectors.extend(embedder.embed_passages(slice)?);
        }
        Ok(vectors)
    })
    .await?
}

/// Embed one query on a blocking thread.
///
/// # Errors
/// Whatever the backend reports, or [`EmbedError::Join`] if the task died.
pub async fn embed_query(
    embedder: Arc<dyn Embedder>,
    query: String,
) -> Result<Vec<f32>, EmbedError> {
    tokio::task::spawn_blocking(move || embedder.embed_query(&query)).await?
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// Records the size of every slice it is handed; each vector encodes the
    /// passage's position in the original batch so order survives slicing.
    struct SliceRecorder {
        slices: Mutex<Vec<usize>>,
    }

    impl Embedder for SliceRecorder {
        fn dim(&self) -> usize {
            1
        }

        fn model_id(&self) -> &str {
            "slice-recorder"
        }

        fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
            self.slices.lock().unwrap().push(passages.len());
            Ok(passages
                .iter()
                .map(|passage| vec![passage.parse::<f32>().unwrap()])
                .collect())
        }

        fn embed_query(&self, _query: &str) -> Result<Vec<f32>, EmbedError> {
            unreachable!("not under test")
        }
    }

    #[tokio::test]
    async fn a_large_batch_reaches_the_backend_in_slices_and_in_order() {
        let embedder = Arc::new(SliceRecorder {
            slices: Mutex::new(Vec::new()),
        });
        let passages: Vec<String> = (0..300).map(|index| index.to_string()).collect();

        let vectors = embed_passages(Arc::clone(&embedder) as Arc<dyn Embedder>, passages)
            .await
            .expect("embed passages");

        assert_eq!(*embedder.slices.lock().unwrap(), vec![128, 128, 44]);
        assert_eq!(vectors.len(), 300);
        for (index, vector) in vectors.iter().enumerate() {
            assert_eq!(vector, &vec![index as f32]);
        }
    }
}
