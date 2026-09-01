//! Choosing an embedding backend from configuration (`docs/design.md` §5.1).

use std::path::PathBuf;
use std::sync::Arc;

use agmem_embed::{Embedder, NoopEmbedder};

use crate::config::{Config, EmbedderKind};

/// Build the backend this run will use.
///
/// # Errors
/// When the backend cannot be loaded, or the build has no such backend.
pub fn build(cfg: &Config) -> anyhow::Result<Arc<dyn Embedder>> {
    match cfg.embedder {
        EmbedderKind::None => Ok(Arc::new(NoopEmbedder)),
        #[cfg(feature = "onnx")]
        EmbedderKind::Fastembed => Ok(Arc::new(agmem_embed::fastembed::FastembedBackend::new(
            Some(model_cache_dir(cfg)),
        )?)),
        #[cfg(not(feature = "onnx"))]
        EmbedderKind::Fastembed => anyhow::bail!(
            "this agmem was built without the ONNX backend; rebuild with the `onnx` feature or run with --embedder none"
        ),
    }
}

/// Where models live unless `FASTEMBED_CACHE_DIR` says otherwise: under the
/// data directory, so everything agmem wrote sits in one place to move or
/// delete.
#[cfg_attr(
    not(feature = "onnx"),
    expect(dead_code, reason = "only the model-loading backend wants a cache dir")
)]
fn model_cache_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("models")
}
