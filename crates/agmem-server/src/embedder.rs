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
        #[cfg(feature = "static")]
        EmbedderKind::Static => Ok(Arc::new(agmem_embed::static_m2v::StaticBackend::new(
            Some(model_cache_dir(cfg).join("potion-base-8M")),
        )?)),
        #[cfg(not(feature = "static"))]
        EmbedderKind::Static => anyhow::bail!(
            "this agmem was built without the static backend; rebuild with the `static` feature or run with --embedder fastembed or none"
        ),
    }
}

/// Where models live unless `FASTEMBED_CACHE_DIR` says otherwise: under the
/// data directory, so everything agmem wrote sits in one place to move or
/// delete. The static backend looks here too, but only for a pre-placed
/// bundle — its hub downloads cache under `HF_HOME` instead.
#[cfg_attr(
    not(any(feature = "onnx", feature = "static")),
    expect(dead_code, reason = "only model-loading backends want a cache dir")
)]
fn model_cache_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("models")
}
