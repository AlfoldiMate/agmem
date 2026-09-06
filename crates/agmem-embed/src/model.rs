//! The models agmem can embed with, and everything that is true of a model
//! rather than of the runtime that loads it.
//!
//! A [`ModelSpec`] is the whole identity of a vector space: the id the store
//! records, the width, how the model marks passages and queries, how its
//! token states pool, the cosine bands its vectors are read against, and
//! where its weights come from. The runtime — [`crate::llama`] — is a way of
//! running a spec, not the other way round. The API backend
//! ([`crate::api`], issue #120) stands outside this table on purpose: its
//! model is a name the user types and its width is learnt from the first
//! answer, so it carries its own identity rather than a [`ModelSpec`].

use std::path::{Path, PathBuf};

use agmem_core::dedup::Thresholds;

use crate::EmbedError;

/// Where a model's weights come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// One GGUF file in a Hugging Face repo, fetched once into the model dir
    /// and run on llama.cpp.
    Gguf {
        /// The repo, `owner/name`.
        repo: &'static str,
        /// The file inside it.
        file: &'static str,
        /// The repo commit the file is fetched at. Pinned so that the
        /// weights behind an id are one set of bytes: a store records this
        /// beside the id (issue #138), and a release that moves the pin is
        /// then a vector-space change the store can see, instead of a
        /// silent drift between what was embedded and what embeds queries.
        revision: &'static str,
    },
}

/// How a model pools its token states into one vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// The first token (`[CLS]`): bge.
    Cls,
    /// The attention-masked mean: EmbeddingGemma.
    Mean,
}

/// Everything true of a model regardless of what runs it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelSpec {
    /// What the store records in `meta.embedder_model`. Names the weights
    /// *and* their quantisation: a Q8_0 GGUF and an int8 ONNX export of the
    /// same model embed into spaces that agree to four decimals, and that is
    /// still two spaces. The prefixes and the pooling are properties of the
    /// id too — a change to either is a new id, not a new field.
    pub id: &'static str,
    /// Vector width; what the store defines its HNSW indexes with.
    pub dim: usize,
    /// How token states become one vector.
    pub pooling: Pooling,
    /// Prepended to every stored text, per the model's training.
    pub passage_prefix: &'static str,
    /// Prepended to every query.
    pub query_prefix: &'static str,
    /// The cosine bands this model's vectors are read against.
    pub thresholds: Thresholds,
    /// Where the weights come from.
    pub source: Source,
}

impl ModelSpec {
    /// The exact weights behind [`Self::id`]: the source's pinned revision.
    /// Recorded in `meta.embedder_revision` next to the id and the width.
    #[must_use]
    pub fn revision(&self) -> &'static str {
        let Source::Gguf { revision, .. } = self.source;
        revision
    }
}

/// The models `--model` can name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Model {
    /// EmbeddingGemma-300M, Q8_0 — the default. 768 dimensions, ~314 MB.
    /// Wins on separation, retrieval and every threshold column over bge
    /// (`docs/eval/embed-models.md`), and on llama.cpp with Metal it is
    /// faster too (`docs/eval/llama-runtime.md`).
    #[default]
    Gemma300M,
    /// bge-small-en-v1.5, Q8_0 — the light option. 384 dimensions, ~36 MB,
    /// a fifth of Gemma's CPU latency; the model every threshold here was
    /// first calibrated on.
    BgeSmall,
}

impl Model {
    /// The spelling `--model` takes: the model family, without the
    /// quantisation, which is agmem's choice rather than the user's.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gemma300M => "embeddinggemma-300m",
            Self::BgeSmall => "bge-small-en-v1.5",
        }
    }

    /// The model a spelling names.
    #[must_use]
    pub fn parse(spelling: &str) -> Option<Self> {
        match spelling {
            "embeddinggemma-300m" => Some(Self::Gemma300M),
            "bge-small-en-v1.5" => Some(Self::BgeSmall),
            _ => None,
        }
    }

    /// The model whose spec carries `id` — what a store's
    /// `meta.embedder_model` names, if this binary can still run it. `None`
    /// for a retired id such as the pre-v0.3 `bge-small-en-v1.5-q` (bge on
    /// ONNX Runtime), which no release since can load.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        [Self::Gemma300M, Self::BgeSmall]
            .into_iter()
            .find(|model| model.spec().id == id)
    }

    /// Everything true of this model.
    #[must_use]
    pub fn spec(self) -> ModelSpec {
        match self {
            Self::Gemma300M => ModelSpec {
                id: "embeddinggemma-300m-q8_0",
                dim: 768,
                pooling: Pooling::Mean,
                // The document and retrieval prompts from the model card; a
                // title-less document is spelled `title: none`.
                passage_prefix: "title: none | text: ",
                query_prefix: "task: search result | query: ",
                thresholds: Thresholds::GEMMA_300M,
                // ggml-org's own conversion: the one hub GGUF that carries
                // the dense_2/dense_3 projections and mean pooling
                // (`docs/eval/llama-runtime.md`).
                source: Source::Gguf {
                    repo: "ggml-org/embeddinggemma-300M-GGUF",
                    file: "embeddinggemma-300M-Q8_0.gguf",
                    revision: "0f741b5a6585bd53aeb15cd1372c56f2a0f65e12",
                },
            },
            Self::BgeSmall => ModelSpec {
                id: "bge-small-en-v1.5-q8_0",
                dim: 384,
                pooling: Pooling::Cls,
                passage_prefix: "passage: ",
                query_prefix: "query: ",
                thresholds: Thresholds::BGE_SMALL,
                source: Source::Gguf {
                    repo: "CompendiumLabs/bge-small-en-v1.5-gguf",
                    file: "bge-small-en-v1.5-q8_0.gguf",
                    revision: "d32f8c040ea3b516330eeb75b72bcc2d3a780ab7",
                },
            },
        }
    }
}

/// The environment variable naming the model directory, which wins over
/// whatever the caller passes: CI points it somewhere unwritable on purpose,
/// so a test that would download a model fails instead.
pub const MODEL_DIR_ENV: &str = "AGMEM_MODEL_DIR";

/// The model directory in force: [`MODEL_DIR_ENV`] when set, else
/// `fallback`, else the platform data dir's `models`.
#[must_use]
pub fn model_dir(fallback: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = std::env::var_os(MODEL_DIR_ENV) {
        return PathBuf::from(dir);
    }
    if let Some(dir) = fallback {
        return dir;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/dev.agmem.agmem")
    } else {
        home.join(".local/share/agmem")
    };
    base.join("models")
}

/// The local path of a spec's weights, fetching them once when the model
/// dir does not hold them.
///
/// The Hugging Face cache layout is kept (`models--owner--name/snapshots/
/// <commit>/<file>`): a second agmem, or any other tool using the hub
/// cache, then shares the download instead of repeating it. The commit is
/// the spec's pinned revision, never the repo's head, so two machines that
/// fetched on different days hold the same bytes.
///
/// # Errors
/// [`EmbedError::Backend`] when the file is not there and cannot be
/// fetched — no network, an unwritable dir, a repo that moved or was
/// force-pushed over the pinned commit.
pub fn fetch(spec: &ModelSpec, model_dir: &Path) -> Result<PathBuf, EmbedError> {
    let Source::Gguf {
        repo,
        file,
        revision,
    } = spec.source;
    let failed = |message: String| EmbedError::Backend {
        backend: spec.id,
        message,
    };
    // Download progress bars are not ours to print: stdout is the MCP wire.
    let api = hf_hub::api::sync::ApiBuilder::new()
        .with_cache_dir(model_dir.to_path_buf())
        .with_progress(false)
        .build()
        .map_err(|e| failed(format!("hub client: {e}")))?;
    let pinned = hf_hub::Repo::with_revision(
        repo.to_owned(),
        hf_hub::RepoType::Model,
        revision.to_owned(),
    );
    api.repo(pinned).get(file).map_err(|e| {
        failed(format!(
            "fetch {repo}/{file} at {revision} into {}: {e}",
            model_dir.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings_round_trip() {
        for model in [Model::Gemma300M, Model::BgeSmall] {
            assert_eq!(Model::parse(model.as_str()), Some(model));
            assert_eq!(Model::from_id(model.spec().id), Some(model));
        }
        assert_eq!(
            Model::parse("bge-small-en-v1.5-q8_0"),
            None,
            "ids are not spellings"
        );
        assert_eq!(
            Model::from_id("bge-small-en-v1.5-q"),
            None,
            "the pre-v0.3 ONNX id is retired: no model here runs it"
        );
    }

    #[test]
    fn the_default_is_gemma_and_every_spec_is_self_consistent() {
        assert_eq!(Model::default(), Model::Gemma300M);
        for model in [Model::Gemma300M, Model::BgeSmall] {
            let spec = model.spec();
            assert!(spec.id.starts_with(model.as_str()), "{}", spec.id);
            assert!(spec.dim > 0);
            assert!(spec.passage_prefix.ends_with(' '));
            assert!(spec.query_prefix.ends_with(' '));
            let Source::Gguf { file, revision, .. } = spec.source;
            assert!(file.ends_with(".gguf"));
            assert_eq!(spec.revision(), revision);
            assert!(
                revision.len() == 40 && revision.bytes().all(|b| b.is_ascii_hexdigit()),
                "a pin is a full commit hash, not a branch name: {revision}"
            );
        }
    }

    #[test]
    fn the_environment_names_the_model_dir() {
        // Read-only on the environment: the fallback path is the arm under
        // test, and the env arm is one `var_os` away from it.
        let dir = model_dir(Some(PathBuf::from("/tmp/agmem-models")));
        match std::env::var_os(MODEL_DIR_ENV) {
            // CI's poisoned dir wins over everything, fallback included.
            Some(set) => {
                assert_eq!(dir, PathBuf::from(&set));
                assert_eq!(model_dir(None), PathBuf::from(set));
            }
            None => {
                assert_eq!(dir, PathBuf::from("/tmp/agmem-models"));
                assert!(model_dir(None).ends_with("models"));
            }
        }
    }
}
