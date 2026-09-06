//! Choosing an embedding backend from configuration (`docs/design.md` §5.1).

use std::path::PathBuf;
use std::sync::Arc;

use agmem_embed::{Embedder, LlamaEmbedder, NoopEmbedder};

use crate::config::{Config, EmbedderKind};

/// Build the backend this run will use.
///
/// One arm per [`EmbedderKind`]: the model is `cfg.model`'s choice and the
/// backend is how it runs, so an API-backed backend (issue #120) is one more
/// arm here and nothing a caller holds changes.
///
/// # Errors
/// When the model cannot be fetched or loaded.
pub fn build(cfg: &Config) -> anyhow::Result<Arc<dyn Embedder>> {
    match cfg.embedder {
        EmbedderKind::Llama => Ok(Arc::new(LlamaEmbedder::new(
            cfg.model.into_embed(),
            Some(model_dir(cfg)),
            cfg.accelerator.into_embed(),
        )?)),
        EmbedderKind::None => Ok(Arc::new(NoopEmbedder)),
    }
}

/// Where models live unless `AGMEM_MODEL_DIR` says otherwise: under the
/// data directory, so everything agmem wrote sits in one place to move or
/// delete.
fn model_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("models")
}
