//! The static backend: potion-base-8M via model2vec, pure Rust.
//!
//! 256 dimensions, ~30 MB of weights, and no ONNX Runtime — this is the
//! contingency for design risk #1 (`docs/design.md` §9) and the instant
//! cold-start option: inference is a table lookup plus a mean, so there is
//! no session to warm. The model is symmetric — no `passage:`/`query:`
//! markers — so both trait methods embed the bare text.

use std::path::PathBuf;

use model2vec_rs::model::StaticModel;

use crate::{EmbedError, Embedder};

/// What the store records in `meta.embedder_model`.
pub const MODEL_ID: &str = "potion-base-8M";

/// Vector width. The store's HNSW indexes are (re)built to this on
/// `--reindex`; `migrate::ensure_embedder` refuses a store recorded at
/// another width.
pub const DIM: usize = 256;

/// The Hugging Face repository the weights come from.
const REPO: &str = "minishlab/potion-base-8M";

/// Static model2vec embeddings, symmetric, normalized.
pub struct StaticBackend {
    /// `StaticModel::encode` takes `&self`, so unlike the ONNX backend this
    /// needs no lock — concurrent callers just read the embedding table.
    model: StaticModel,
}

/// A loaded table has nothing worth printing; the model identity is the
/// whole story.
impl std::fmt::Debug for StaticBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticBackend")
            .field("model", &MODEL_ID)
            .field("dim", &DIM)
            .finish()
    }
}

impl StaticBackend {
    /// Load the model, downloading it once if nothing local has it.
    ///
    /// When `local_dir` exists it is loaded as-is and the network is never
    /// touched — that is the zero-network bundle path. Otherwise the weights
    /// come from the Hugging Face hub through its own cache, which `HF_HOME`
    /// relocates; there is no fastembed-style cache override to plumb,
    /// because model2vec does not expose one.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when the model cannot be fetched or loaded,
    /// or loads at a width other than [`DIM`].
    pub fn new(local_dir: Option<PathBuf>) -> Result<Self, EmbedError> {
        let source = match local_dir {
            Some(dir) if dir.exists() => dir,
            _ => PathBuf::from(REPO),
        };

        tracing::info!(model = MODEL_ID, dim = DIM, source = %source.display(), "loading embedding model");
        // Normalization is forced on rather than left to the model config:
        // the store's cosine indexes assume unit vectors, and a config edit
        // in a bundled copy must not be able to change the geometry.
        let model = StaticModel::from_pretrained(&source, None, Some(true), None)
            .map_err(|error| failed(format!("{error:#}")))?;

        // The width is a property of the weights, not the code, and the
        // crate exposes no accessor for it — so prove it with one embedding
        // now, rather than during the first `remember`.
        let probe = model.encode_single("dimension probe");
        if probe.len() != DIM {
            return Err(failed(format!(
                "model at {} embeds into {} dimensions, expected {DIM}",
                source.display(),
                probe.len()
            )));
        }
        Ok(Self { model })
    }
}

impl Embedder for StaticBackend {
    fn dim(&self) -> usize {
        DIM
    }

    fn model_id(&self) -> &str {
        MODEL_ID
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        Ok(self.model.encode(passages))
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        Ok(self.model.encode_single(query))
    }
}

/// Errors here are load-time only: `encode` cannot fail (it panics on a
/// corrupt tokenizer, which the load-time probe would already have hit).
fn failed(message: String) -> EmbedError {
    EmbedError::Backend {
        backend: MODEL_ID,
        message,
    }
}
