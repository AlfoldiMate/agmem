//! Choosing an embedding backend from configuration (`docs/design.md` §5.1).

use std::path::PathBuf;
use std::sync::Arc;

use agmem_embed::{Embedder, NoopEmbedder};

use crate::config::{Config, EmbedderKind};

/// Build the backend this run will use.
///
/// # Errors
/// When the model cannot be loaded.
pub fn build(cfg: &Config) -> anyhow::Result<Arc<dyn Embedder>> {
    match cfg.embedder {
        EmbedderKind::Fastembed => Ok(Arc::new(agmem_embed::fastembed::FastembedBackend::new(
            Some(model_cache_dir(cfg)),
            cfg.accelerator.into_embed(),
        )?)),
        EmbedderKind::None => Ok(Arc::new(NoopEmbedder)),
    }
}

/// Where models live unless `FASTEMBED_CACHE_DIR` says otherwise: under the
/// data directory, so everything agmem wrote sits in one place to move or
/// delete.
fn model_cache_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("models")
}
