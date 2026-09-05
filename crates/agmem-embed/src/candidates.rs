//! The embedding candidates behind the #133 probe, each loaded behind
//! [`Embedder`] with its own prefixes and pooling.
//!
//! Measurement surface only. Nothing in the server constructs these; the
//! ignored tests in `tests/candidates.rs` and the candidate branch of the
//! eval recorder in `tests/fastembed.rs` are the callers, and
//! `docs/eval/embed-models.md` is where their numbers decide whether the
//! migration (#138) gets built at all.
//!
//! Every candidate runs through the runtime the daemon runs — ONNX Runtime
//! on CPU — so the vectors measured are the vectors a store would get. Two
//! are in fastembed's built-in list; arctic-embed-m-v2.0 is a user-defined
//! fastembed model read from the model cache, which
//! `scripts/embed-candidates-fetch.nu` fills. Qwen3-Embedding is driven
//! through `ort` directly: its export takes a `position_ids` input fastembed
//! never feeds, and it wants last-token pooling fastembed cannot express, so
//! this module tokenises, runs the session and pools by hand.
//!
//! The fp32 siblings of the three encoder candidates exist for #139: Core
//! ML has no kernel for the dynamic-quantisation ops (`MatMulInteger`,
//! `DynamicQuantizeLinear`), so a quantised graph on the CoreML provider is
//! a CPU graph with extra partition boundaries, and only the fp32 exports
//! can measure what the accelerator does.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fastembed::{
    EmbeddingModel, InitOptionsUserDefined, Pooling, QuantizationMode, TextEmbedding,
    TextInitOptions, TokenizerFiles, UserDefinedEmbeddingModel,
};
use ndarray::{Array2, Axis};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Value;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use crate::accelerator::Active;
use crate::{EmbedError, Embedder};

/// The environment variable that names the candidate a test run measures.
pub const CANDIDATE_ENV: &str = "AGMEM_CANDIDATE";

/// Token files the user-defined loaders read from the model cache; the same
/// four fastembed's own hub loader fetches.
const TOKENIZER_FILES: [&str; 4] = [
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
];

/// Qwen3's end-of-sequence token, which its embedding recipe requires as the
/// last real token of every input — the pooled position — and its pad.
const QWEN3_EOS: &str = "<|endoftext|>";

/// Longest input, in tokens, for the models this module tokenises itself.
const MAX_TOKENS: usize = 512;

/// The models under measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Candidate {
    /// The control: what ships today.
    BgeSmallQ,
    /// EmbeddingGemma-300M, 8-bit dynamic quantisation, pooled in-graph.
    Gemma300MQ,
    /// snowflake-arctic-embed-m-v2.0, int8, CLS pooled.
    ArcticMV2Int8,
    /// Qwen3-Embedding-0.6B, int8, last-token pooled.
    Qwen3Embedding06BInt8,
    /// The control's fp32 export (#139: the accelerator measurement).
    BgeSmallF32,
    /// EmbeddingGemma-300M, fp32 (#139).
    Gemma300MF32,
    /// snowflake-arctic-embed-m-v2.0, fp32, CLS pooled (#139).
    ArcticMV2F32,
}

impl Candidate {
    /// Every candidate, control first.
    pub const ALL: [Self; 7] = [
        Self::BgeSmallQ,
        Self::Gemma300MQ,
        Self::ArcticMV2Int8,
        Self::Qwen3Embedding06BInt8,
        Self::BgeSmallF32,
        Self::Gemma300MF32,
        Self::ArcticMV2F32,
    ];

    /// The id a recording, a latency row and `AGMEM_CANDIDATE` spell.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::BgeSmallQ => crate::fastembed::MODEL_ID,
            Self::Gemma300MQ => "embeddinggemma-300m-q",
            Self::ArcticMV2Int8 => "arctic-embed-m-v2.0-int8",
            Self::Qwen3Embedding06BInt8 => "qwen3-embedding-0.6b-int8",
            Self::BgeSmallF32 => "bge-small-en-v1.5",
            Self::Gemma300MF32 => "embeddinggemma-300m",
            Self::ArcticMV2F32 => "arctic-embed-m-v2.0",
        }
    }

    /// The candidate an id names.
    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|candidate| candidate.id() == id)
    }

    /// The candidate [`CANDIDATE_ENV`] names, if it is set.
    ///
    /// # Panics
    /// When the variable names no known candidate — a typo must not measure
    /// the control by accident.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let id = std::env::var(CANDIDATE_ENV).ok()?;
        Some(Self::parse(&id).unwrap_or_else(|| {
            let known: Vec<&str> = Self::ALL.iter().map(|c| c.id()).collect();
            panic!("{CANDIDATE_ENV}={id:?} names no candidate; one of {known:?}")
        }))
    }

    /// Full vector width, before any MRL truncation.
    #[must_use]
    pub fn dim(self) -> usize {
        match self {
            Self::BgeSmallQ | Self::BgeSmallF32 => crate::fastembed::DIM,
            Self::Gemma300MQ | Self::Gemma300MF32 | Self::ArcticMV2Int8 | Self::ArcticMV2F32 => 768,
            Self::Qwen3Embedding06BInt8 => 1024,
        }
    }

    /// What stored text is marked with, per the model's card.
    fn passage_prefix(self) -> &'static str {
        match self {
            Self::BgeSmallQ | Self::BgeSmallF32 => "passage: ",
            Self::Gemma300MQ | Self::Gemma300MF32 => "title: none | text: ",
            Self::ArcticMV2Int8 | Self::ArcticMV2F32 | Self::Qwen3Embedding06BInt8 => "",
        }
    }

    /// What the search side is marked with, per the model's card.
    fn query_prefix(self) -> &'static str {
        match self {
            Self::BgeSmallQ | Self::BgeSmallF32 | Self::ArcticMV2Int8 | Self::ArcticMV2F32 => {
                "query: "
            }
            Self::Gemma300MQ | Self::Gemma300MF32 => "task: search result | query: ",
            Self::Qwen3Embedding06BInt8 => {
                "Instruct: Given a web search query, retrieve relevant passages that answer \
                 the query\nQuery: "
            }
        }
    }

    /// The Hugging Face repo a user-defined candidate's files come from,
    /// and the ONNX file inside it; `None` for fastembed's built-ins.
    #[must_use]
    pub fn user_defined(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::BgeSmallQ | Self::Gemma300MQ | Self::BgeSmallF32 | Self::Gemma300MF32 => None,
            Self::ArcticMV2Int8 => Some((
                "Snowflake/snowflake-arctic-embed-m-v2.0",
                "onnx/model_int8.onnx",
            )),
            Self::ArcticMV2F32 => {
                Some(("Snowflake/snowflake-arctic-embed-m-v2.0", "onnx/model.onnx"))
            }
            Self::Qwen3Embedding06BInt8 => Some((
                "onnx-community/Qwen3-Embedding-0.6B-ONNX",
                "onnx/model_int8.onnx",
            )),
        }
    }

    /// Every file a user-defined candidate needs, relative to its repo.
    #[must_use]
    pub fn files(self) -> Vec<&'static str> {
        let Some((_, onnx)) = self.user_defined() else {
            return Vec::new();
        };
        let mut files = vec![onnx];
        files.extend(TOKENIZER_FILES);
        files
    }

    /// Where a user-defined candidate's files live under the model cache.
    #[must_use]
    pub fn dir(self, cache_dir: &Path) -> Option<PathBuf> {
        self.user_defined()
            .map(|(repo, _)| cache_dir.join("candidates").join(repo))
    }
}

/// The model cache the candidates share: `FASTEMBED_CACHE_DIR` when set,
/// else the platform data dir's `models`, which is where
/// `scripts/embed-candidates-fetch.nu` puts the user-defined files.
#[must_use]
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("FASTEMBED_CACHE_DIR") {
        return PathBuf::from(dir);
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

/// What runs the model.
enum Engine {
    /// fastembed's `embed`: tokenising, the session and CLS/mean/in-graph
    /// pooling all inside the crate.
    Fastembed(Mutex<TextEmbedding>),
    /// An `ort` session driven here, for an export fastembed cannot feed.
    Direct(Mutex<Direct>),
}

/// One loaded candidate.
pub struct CandidateBackend {
    candidate: Candidate,
    engine: Engine,
}

impl std::fmt::Debug for CandidateBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CandidateBackend")
            .field("model", &self.candidate.id())
            .field("dim", &self.candidate.dim())
            .field(
                "engine",
                &match self.engine {
                    Engine::Fastembed(_) => "fastembed",
                    Engine::Direct(_) => "ort",
                },
            )
            .finish()
    }
}

impl CandidateBackend {
    /// Load `candidate` from `cache_dir`: built-ins download once through
    /// fastembed; user-defined ones read the files the fetch script put
    /// there and fail naming the missing one.
    ///
    /// `accelerator` is the execution provider every engine registers, so a
    /// row measured under `coreml` ran on it whichever path loads the model.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when a file is missing or the session will
    /// not build.
    pub fn load(
        candidate: Candidate,
        cache_dir: &Path,
        accelerator: Active,
    ) -> Result<Self, EmbedError> {
        let failed = |message: String| EmbedError::Backend {
            backend: candidate.id(),
            message,
        };
        let providers = accelerator.execution_providers(Some(cache_dir));
        let engine = match candidate {
            Candidate::BgeSmallQ
            | Candidate::Gemma300MQ
            | Candidate::BgeSmallF32
            | Candidate::Gemma300MF32 => {
                let builtin = match candidate {
                    Candidate::BgeSmallQ => EmbeddingModel::BGESmallENV15Q,
                    Candidate::BgeSmallF32 => EmbeddingModel::BGESmallENV15,
                    Candidate::Gemma300MF32 => EmbeddingModel::EmbeddingGemma300M,
                    _ => EmbeddingModel::EmbeddingGemma300MQ,
                };
                let options = TextInitOptions::new(builtin)
                    .with_cache_dir(cache_dir.to_path_buf())
                    .with_show_download_progress(false)
                    .with_execution_providers(providers);
                let model = TextEmbedding::try_new(options).map_err(|e| failed(e.to_string()))?;
                Engine::Fastembed(Mutex::new(model))
            }
            Candidate::ArcticMV2Int8 | Candidate::ArcticMV2F32 => {
                let files = Files::read(candidate, cache_dir)?;
                let tokenizer_files = files.tokenizer_files();
                let quantization = match candidate {
                    Candidate::ArcticMV2Int8 => QuantizationMode::Static,
                    _ => QuantizationMode::None,
                };
                let model = UserDefinedEmbeddingModel::new(files.onnx, tokenizer_files)
                    .with_quantization(quantization)
                    .with_pooling(Pooling::Cls);
                let options = InitOptionsUserDefined::new()
                    .with_max_length(MAX_TOKENS)
                    .with_execution_providers(providers);
                let model = TextEmbedding::try_new_from_user_defined(model, options)
                    .map_err(|e| failed(e.to_string()))?;
                Engine::Fastembed(Mutex::new(model))
            }
            Candidate::Qwen3Embedding06BInt8 => {
                let files = Files::read(candidate, cache_dir)?;
                Engine::Direct(Mutex::new(
                    Direct::load(files, QWEN3_EOS, &providers).map_err(failed)?,
                ))
            }
        };
        tracing::info!(
            model = candidate.id(),
            dim = candidate.dim(),
            accelerator = accelerator.as_str(),
            "loaded candidate"
        );
        Ok(Self { candidate, engine })
    }

    /// The candidate this backend runs.
    #[must_use]
    pub fn candidate(&self) -> Candidate {
        self.candidate
    }

    /// Embed already-prefixed texts, checking the width.
    fn embed_all(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let failed = |message: String| EmbedError::Backend {
            backend: self.candidate.id(),
            message,
        };
        let poisoned = || failed("the model lock was poisoned by an earlier panic".to_owned());
        let vectors = match &self.engine {
            Engine::Fastembed(model) => {
                let mut model = model.lock().map_err(|_| poisoned())?;
                model
                    .embed(texts, None)
                    .map_err(|e| failed(e.to_string()))?
            }
            Engine::Direct(direct) => {
                let mut direct = direct.lock().map_err(|_| poisoned())?;
                direct.embed(&texts).map_err(failed)?
            }
        };

        let dim = self.candidate.dim();
        if let Some(wrong) = vectors.iter().find(|vector| vector.len() != dim) {
            return Err(failed(format!(
                "model returned {}-dimensional vectors, expected {dim}",
                wrong.len()
            )));
        }
        Ok(vectors)
    }
}

impl Embedder for CandidateBackend {
    fn dim(&self) -> usize {
        self.candidate.dim()
    }

    fn model_id(&self) -> &str {
        self.candidate.id()
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let prefix = self.candidate.passage_prefix();
        self.embed_all(
            passages
                .iter()
                .map(|passage| format!("{prefix}{passage}"))
                .collect(),
        )
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        let prefix = self.candidate.query_prefix();
        let mut vectors = self.embed_all(vec![format!("{prefix}{query}")])?;
        vectors.pop().ok_or_else(|| EmbedError::Backend {
            backend: self.candidate.id(),
            message: "model returned no vector for the query".to_owned(),
        })
    }
}

/// A user-defined candidate's files, read from the cache.
struct Files {
    onnx: Vec<u8>,
    tokenizer: Vec<u8>,
    config: Vec<u8>,
    special_tokens_map: Vec<u8>,
    tokenizer_config: Vec<u8>,
}

impl Files {
    fn read(candidate: Candidate, cache_dir: &Path) -> Result<Self, EmbedError> {
        let dir = candidate.dir(cache_dir).expect("user-defined");
        let (_, onnx) = candidate.user_defined().expect("user-defined");
        let read = |file: &str| -> Result<Vec<u8>, EmbedError> {
            std::fs::read(dir.join(file)).map_err(|e| EmbedError::Backend {
                backend: candidate.id(),
                message: format!(
                    "read {}: {e}; run scripts/embed-candidates-fetch.nu",
                    dir.join(file).display()
                ),
            })
        };
        Ok(Self {
            onnx: read(onnx)?,
            tokenizer: read("tokenizer.json")?,
            config: read("config.json")?,
            special_tokens_map: read("special_tokens_map.json")?,
            tokenizer_config: read("tokenizer_config.json")?,
        })
    }

    fn tokenizer_files(&self) -> TokenizerFiles {
        TokenizerFiles {
            tokenizer_file: self.tokenizer.clone(),
            config_file: self.config.clone(),
            special_tokens_map_file: self.special_tokens_map.clone(),
            tokenizer_config_file: self.tokenizer_config.clone(),
        }
    }
}

/// An `ort` session with its tokenizer, for a decoder-style embedder:
/// `input_ids`, `attention_mask` and — when the graph asks — `position_ids`
/// in; `last_hidden_state` out, pooled at each row's last attended token.
struct Direct {
    session: Session,
    tokenizer: Tokenizer,
    wants_position_ids: bool,
    /// The `past_key_values.<n>.key|value` inputs a decoder export carries,
    /// each fed an empty cache of `[rows, kv_heads, 0, head_dim]`.
    past_inputs: Vec<String>,
    kv_heads: usize,
    head_dim: usize,
    /// The graph's own pooled output, when the export carries one.
    pooled_output: Option<String>,
    /// The EOS text to append when the tokenizer's post-processor does not.
    append_eos: Option<&'static str>,
}

impl Direct {
    fn load(
        files: Files,
        eos: &'static str,
        providers: &[ort::ep::ExecutionProviderDispatch],
    ) -> Result<Self, String> {
        let mut tokenizer = Tokenizer::from_bytes(&files.tokenizer).map_err(|e| e.to_string())?;
        let pad_id = tokenizer
            .token_to_id(eos)
            .ok_or_else(|| format!("tokenizer has no {eos} token"))?;
        tokenizer
            .with_padding(Some(PaddingParams {
                strategy: PaddingStrategy::BatchLongest,
                pad_id,
                pad_token: eos.to_owned(),
                ..PaddingParams::default()
            }))
            .with_truncation(Some(TruncationParams {
                max_length: MAX_TOKENS,
                ..TruncationParams::default()
            }))
            .map_err(|e| e.to_string())?;
        let append_eos = (!tokenizer_appends(&files.tokenizer, eos)).then_some(eos);

        let session = Session::builder()
            .map_err(|e| e.to_string())?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| e.to_string())?
            .with_execution_providers(providers)
            .map_err(|e| e.to_string())?
            .commit_from_memory(&files.onnx)
            .map_err(|e| e.to_string())?;
        let wants_position_ids = session.inputs().iter().any(|i| i.name() == "position_ids");
        let past_inputs: Vec<String> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_owned())
            .filter(|name| name.starts_with("past_key_values."))
            .collect();
        let config: serde_json::Value =
            serde_json::from_slice(&files.config).map_err(|e| format!("config.json: {e}"))?;
        let number = |key: &str| -> Result<usize, String> {
            config[key]
                .as_u64()
                .map(|n| n as usize)
                .ok_or_else(|| format!("config.json lacks {key}"))
        };
        let (kv_heads, head_dim) = if past_inputs.is_empty() {
            (0, 0)
        } else {
            let head_dim = match number("head_dim") {
                Ok(dim) => dim,
                Err(_) => number("hidden_size")? / number("num_attention_heads")?,
            };
            (number("num_key_value_heads")?, head_dim)
        };
        let pooled_output = session
            .outputs()
            .iter()
            .map(|o| o.name().to_owned())
            .find(|name| name == "sentence_embedding");
        tracing::info!(
            inputs = ?session.inputs().iter().map(|i| i.name()).collect::<Vec<_>>(),
            outputs = ?session.outputs().iter().map(|o| o.name()).collect::<Vec<_>>(),
            append_eos = append_eos.is_some(),
            past = past_inputs.len(),
            kv_heads,
            head_dim,
            "direct session"
        );
        Ok(Self {
            session,
            tokenizer,
            wants_position_ids,
            past_inputs,
            kv_heads,
            head_dim,
            pooled_output,
            append_eos,
        })
    }

    fn embed(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let inputs: Vec<String> = match self.append_eos {
            Some(eos) => texts.iter().map(|t| format!("{t}{eos}")).collect(),
            None => texts.to_vec(),
        };
        let encodings = self
            .tokenizer
            .encode_batch(inputs, true)
            .map_err(|e| e.to_string())?;
        let rows = encodings.len();
        let len = encodings.first().map_or(0, |e| e.get_ids().len());
        let mut ids = Vec::with_capacity(rows * len);
        let mut mask = Vec::with_capacity(rows * len);
        let mut positions = Vec::with_capacity(rows * len);
        for encoding in &encodings {
            ids.extend(encoding.get_ids().iter().map(|&x| i64::from(x)));
            mask.extend(encoding.get_attention_mask().iter().map(|&x| i64::from(x)));
            positions.extend((0..len as i64).map(|p| p.min(len as i64 - 1)));
        }
        let shape = (rows, len);
        let ids = Array2::from_shape_vec(shape, ids).map_err(|e| e.to_string())?;
        let mask = Array2::from_shape_vec(shape, mask).map_err(|e| e.to_string())?;
        let positions = Array2::from_shape_vec(shape, positions).map_err(|e| e.to_string())?;

        let mut session_inputs = ort::inputs![
            "input_ids" => Value::from_array(ids).map_err(|e| e.to_string())?,
            "attention_mask" => Value::from_array(mask.clone()).map_err(|e| e.to_string())?,
        ];
        if self.wants_position_ids {
            session_inputs.push((
                "position_ids".into(),
                Value::from_array(positions)
                    .map_err(|e| e.to_string())?
                    .into(),
            ));
        }
        for name in &self.past_inputs {
            let empty = ndarray::Array4::<f32>::zeros((rows, self.kv_heads, 0, self.head_dim));
            session_inputs.push((
                name.clone().into(),
                Value::from_array(empty).map_err(|e| e.to_string())?.into(),
            ));
        }
        let outputs = self
            .session
            .run(session_inputs)
            .map_err(|e| e.to_string())?;

        if let Some(name) = &self.pooled_output {
            let pooled = outputs[name.as_str()]
                .try_extract_array::<f32>()
                .map_err(|e| e.to_string())?
                .into_dimensionality::<ndarray::Ix2>()
                .map_err(|e| format!("{name} is not 2-D: {e}"))?;
            return Ok(pooled
                .outer_iter()
                .map(|row| normalise(row.to_vec()))
                .collect());
        }
        let hidden = outputs["last_hidden_state"]
            .try_extract_array::<f32>()
            .map_err(|e| e.to_string())?
            .into_dimensionality::<ndarray::Ix3>()
            .map_err(|e| format!("last_hidden_state is not 3-D: {e}"))?;
        let mut vectors = Vec::with_capacity(rows);
        for (row, attended) in mask.outer_iter().enumerate() {
            let last = attended
                .iter()
                .rposition(|&bit| bit == 1)
                .ok_or_else(|| format!("row {row} attends to no token"))?;
            vectors.push(normalise(
                hidden.index_axis(Axis(0), row).row(last).to_vec(),
            ));
        }
        Ok(vectors)
    }
}

/// Whether a `tokenizer.json` post-processor already appends `token` to
/// every sequence, read off the file rather than assumed: appending it
/// twice would pool the wrong position as surely as leaving it off.
fn tokenizer_appends(tokenizer_json: &[u8], token: &str) -> bool {
    fn mentions(value: &serde_json::Value, token: &str) -> bool {
        match value {
            serde_json::Value::String(s) => s == token,
            serde_json::Value::Array(items) => items.iter().any(|item| mentions(item, token)),
            serde_json::Value::Object(map) => map.values().any(|item| mentions(item, token)),
            _ => false,
        }
    }
    serde_json::from_slice::<serde_json::Value>(tokenizer_json)
        .ok()
        .and_then(|doc| doc.get("post_processor").cloned())
        .is_some_and(|post| mentions(&post, token))
}

fn normalise(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut vector {
            *x /= norm;
        }
    }
    vector
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_round_trips() {
        for candidate in Candidate::ALL {
            assert_eq!(Candidate::parse(candidate.id()), Some(candidate));
        }
        assert_eq!(Candidate::parse("nope"), None);
    }

    #[test]
    fn the_built_in_dimensions_match_fastembed() {
        for (candidate, builtin) in [
            (Candidate::BgeSmallQ, EmbeddingModel::BGESmallENV15Q),
            (Candidate::Gemma300MQ, EmbeddingModel::EmbeddingGemma300MQ),
        ] {
            let info = TextEmbedding::get_model_info(&builtin).expect("model info");
            assert_eq!(info.dim, candidate.dim(), "{}", candidate.id());
        }
    }

    #[test]
    fn a_post_processor_that_appends_eos_is_detected() {
        let with = br#"{"post_processor":{"type":"TemplateProcessing","single":[{"Sequence":{"id":"A"}},{"SpecialToken":{"id":"<|endoftext|>"}}]}}"#;
        let without = br#"{"post_processor":null}"#;
        assert!(tokenizer_appends(with, QWEN3_EOS));
        assert!(!tokenizer_appends(without, QWEN3_EOS));
    }
}
