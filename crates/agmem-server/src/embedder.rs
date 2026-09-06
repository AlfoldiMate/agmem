//! Choosing an embedding backend from configuration (`docs/design.md` §5.1).

use std::path::PathBuf;
use std::sync::Arc;

use agmem_embed::{ApiEmbedder, Embedder, LlamaEmbedder, NoopEmbedder};

use crate::config::{Config, EmbedderKind};

/// Build the backend this run will use.
///
/// One arm per [`EmbedderKind`]: for `llama` the model is `cfg.model`'s
/// choice and the backend is how it runs; for `api` the model is whatever
/// `cfg.api` names and the key is [`API_KEY_ENV`], read here and handed to
/// the backend, never into `Config` or a log.
///
/// # Errors
/// When the model cannot be fetched or loaded, or the endpoint refuses the
/// probe.
pub fn build(cfg: &Config) -> anyhow::Result<Arc<dyn Embedder>> {
    match cfg.embedder {
        EmbedderKind::Llama => Ok(Arc::new(LlamaEmbedder::new(
            cfg.model.into_embed(),
            Some(model_dir(cfg)),
            cfg.accelerator.into_embed(),
        )?)),
        EmbedderKind::Api => {
            let key = std::env::var(API_KEY_ENV)
                .ok()
                .filter(|key| !key.is_empty());
            Ok(Arc::new(ApiEmbedder::new(
                &cfg.api.url,
                &cfg.api.model,
                key,
            )?))
        }
        EmbedderKind::None => Ok(Arc::new(NoopEmbedder)),
    }
}

/// The bearer token `--embedder api` sends. An environment variable and not
/// a flag: argv is readable by every process on the host, the daemon is
/// spawned with the session's environment, and a local endpoint needs none.
pub const API_KEY_ENV: &str = "AGMEM_API_KEY";

/// Where models live unless `AGMEM_MODEL_DIR` says otherwise: under the
/// data directory, so everything agmem wrote sits in one place to move or
/// delete.
fn model_dir(cfg: &Config) -> PathBuf {
    cfg.data_dir.join("models")
}
