//! llama.cpp as a second embedding runtime (issue #178).
//!
//! Measurement surface only, like [`crate::candidates`]: nothing in the
//! server constructs this. A GGUF candidate runs here, on the CPU or — with
//! the `llama-metal` feature on macOS — llama.cpp's Metal backend, and its
//! vectors are compared with the ORT vectors of the same model
//! (`docs/eval/llama-runtime.md`).
//!
//! `llama-cpp-2`'s context is `!Send` and borrows its model, and this
//! workspace denies `unsafe_code`, so the model, context and batch live on
//! one worker thread and callers hand it work over a channel. That also
//! keeps every embed call serial, which is what the ORT engines do with a
//! mutex.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::token::LlamaToken;

use crate::accelerator::Active;

/// Longest input, in tokens, matching the ORT path's truncation.
const MAX_TOKENS: usize = 512;

/// Tokens one `decode` takes: the batch, the physical batch and the context
/// are all this size, because a non-causal encoder needs every token of a
/// sequence in one physical batch, and one context is enough for the
/// document shape when it is split into groups of this many tokens.
const BATCH_TOKENS: usize = 2048;

/// Sequences one `decode` may carry.
const MAX_SEQUENCES: usize = 64;

/// The process-wide llama.cpp backend: `init` succeeds once per process.
static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();

fn backend() -> Result<&'static LlamaBackend, String> {
    if let Some(backend) = BACKEND.get() {
        return Ok(backend);
    }
    let backend = LlamaBackend::init().map_err(|e| e.to_string())?;
    Ok(BACKEND.get_or_init(|| backend))
}

/// How a GGUF model pools its token states into one vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pooling {
    /// The first token (`[CLS]`): bge.
    Cls,
    /// The attention-masked mean: EmbeddingGemma.
    Mean,
}

impl Pooling {
    fn llama(self) -> LlamaPoolingType {
        match self {
            Self::Cls => LlamaPoolingType::Cls,
            Self::Mean => LlamaPoolingType::Mean,
        }
    }
}

/// One request to the worker: prefixed texts in, unit vectors out.
struct Job {
    texts: Vec<String>,
    reply: mpsc::Sender<Result<Vec<Vec<f32>>, String>>,
}

/// A GGUF model loaded on its own thread.
pub struct LlamaEmbedder {
    id: &'static str,
    dim: usize,
    accelerator: Active,
    jobs: Mutex<mpsc::Sender<Job>>,
}

impl std::fmt::Debug for LlamaEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlamaEmbedder")
            .field("model", &self.id)
            .field("dim", &self.dim)
            .field("accelerator", &self.accelerator.as_str())
            .finish()
    }
}

impl LlamaEmbedder {
    /// Load the GGUF at `path`, offloading every layer to Metal when
    /// `accelerator` is [`Active::Metal`] and none otherwise.
    ///
    /// # Errors
    /// The llama.cpp message when the backend, the model or the context will
    /// not come up, or the model's width is not `dim`.
    pub fn load(
        id: &'static str,
        path: &Path,
        pooling: Pooling,
        dim: usize,
        accelerator: Active,
    ) -> Result<Self, String> {
        let gpu_layers: u32 = match accelerator {
            Active::Metal => 1000,
            Active::Cpu => 0,
            Active::CoreMl => return Err("CoreML is an ONNX Runtime provider".to_owned()),
        };
        let (jobs, inbox) = mpsc::channel::<Job>();
        let (ready, loaded) = mpsc::channel::<Result<(), String>>();
        let path = path.to_path_buf();
        std::thread::Builder::new()
            .name(format!("llama-{id}"))
            .spawn(move || worker(&path, pooling, dim, gpu_layers, &ready, &inbox))
            .map_err(|e| e.to_string())?;
        loaded
            .recv()
            .map_err(|_| "the model thread exited before loading".to_owned())??;
        Ok(Self {
            id,
            dim,
            accelerator,
            jobs: Mutex::new(jobs),
        })
    }

    /// The model's id.
    #[must_use]
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// What the model runs on.
    #[must_use]
    pub fn accelerator(&self) -> Active {
        self.accelerator
    }

    /// Embed already-prefixed texts, in order, L2-normalised.
    ///
    /// # Errors
    /// The llama.cpp message when tokenising or decoding fails.
    pub fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let (reply, answer) = mpsc::channel();
        self.jobs
            .lock()
            .map_err(|_| "the model lock was poisoned by an earlier panic".to_owned())?
            .send(Job {
                texts: texts.to_vec(),
                reply,
            })
            .map_err(|_| "the model thread has exited".to_owned())?;
        answer
            .recv()
            .map_err(|_| "the model thread exited mid-batch".to_owned())?
    }
}

/// The thread body: load, report, then serve jobs until the sender drops.
///
/// Model, context and batch are locals here rather than fields of a struct
/// because the context borrows the model; the loop is the struct.
fn worker(
    path: &Path,
    pooling: Pooling,
    dim: usize,
    gpu_layers: u32,
    ready: &mpsc::Sender<Result<(), String>>,
    inbox: &mpsc::Receiver<Job>,
) {
    let loaded = backend().and_then(|backend| {
        let model_params = LlamaModelParams::default().with_n_gpu_layers(gpu_layers);
        let model = LlamaModel::load_from_file(backend, path, &model_params)
            .map_err(|e| format!("load {}: {e}", path.display()))?;
        let n_embd = usize::try_from(model.n_embd_out()).unwrap_or(0);
        if n_embd != dim {
            return Err(format!(
                "{} is {n_embd}-dimensional, expected {dim}",
                path.display()
            ));
        }
        Ok((backend, model))
    });
    let (backend, model) = match loaded {
        Ok(loaded) => loaded,
        Err(message) => {
            let _ = ready.send(Err(message));
            return;
        }
    };
    let threads =
        i32::try_from(std::thread::available_parallelism().map_or(4, |n| n.get())).unwrap_or(4);
    let batch_tokens = u32::try_from(BATCH_TOKENS).expect("fits");
    let context_params = LlamaContextParams::default()
        .with_embeddings(true)
        .with_pooling_type(pooling.llama())
        .with_n_ctx(NonZeroU32::new(batch_tokens))
        .with_n_batch(batch_tokens)
        .with_n_ubatch(batch_tokens)
        .with_n_seq_max(u32::try_from(MAX_SEQUENCES).expect("fits"))
        .with_n_threads(threads)
        .with_n_threads_batch(threads);
    let mut context = match model.new_context(backend, context_params) {
        Ok(context) => context,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    let mut batch = LlamaBatch::new(BATCH_TOKENS, i32::try_from(MAX_SEQUENCES).expect("fits"));
    let truncate_to = MAX_TOKENS.min(usize::try_from(model.n_ctx_train()).unwrap_or(MAX_TOKENS));
    let runtime = Runtime {
        model: &model,
        dim,
        truncate_to,
    };
    // One warm-up so the first Metal shader compile is paid before any
    // timing, and a broken parameter set fails at load.
    if let Err(message) = runtime.embed(&mut context, &mut batch, &["warm-up".to_owned()]) {
        let _ = ready.send(Err(message));
        return;
    }
    let _ = ready.send(Ok(()));
    while let Ok(job) = inbox.recv() {
        let _ = job
            .reply
            .send(runtime.embed(&mut context, &mut batch, &job.texts));
    }
}

/// What every embed call needs besides the context and the batch.
struct Runtime<'m> {
    model: &'m LlamaModel,
    dim: usize,
    truncate_to: usize,
}

impl Runtime<'_> {
    /// Tokenise with the model's own special tokens (`[CLS]`/`[SEP]`, or
    /// `<bos>`/`<eos>`), truncated to the shorter of 512 and the model's
    /// training length with the end token kept last.
    fn tokenise(&self, text: &str) -> Result<Vec<LlamaToken>, String> {
        let mut tokens = self
            .model
            .str_to_token(text, AddBos::Always)
            .map_err(|e| e.to_string())?;
        if tokens.len() > self.truncate_to {
            let eos = self.model.token_eos();
            tokens.truncate(self.truncate_to - 1);
            tokens.push(eos);
        }
        Ok(tokens)
    }

    /// Embed texts in groups that fit one physical batch, in order.
    fn embed(
        &self,
        context: &mut LlamaContext<'_>,
        batch: &mut LlamaBatch,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, String> {
        let tokenised: Vec<Vec<LlamaToken>> = texts
            .iter()
            .map(|text| self.tokenise(text))
            .collect::<Result<_, _>>()?;

        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
        let mut group: Vec<&[LlamaToken]> = Vec::new();
        let mut group_tokens = 0;
        for tokens in &tokenised {
            let full = group_tokens + tokens.len() > BATCH_TOKENS || group.len() == MAX_SEQUENCES;
            if full && !group.is_empty() {
                vectors.extend(decode_group(context, batch, &group, self.dim)?);
                group.clear();
                group_tokens = 0;
            }
            group.push(tokens);
            group_tokens += tokens.len();
        }
        if !group.is_empty() {
            vectors.extend(decode_group(context, batch, &group, self.dim)?);
        }
        Ok(vectors)
    }
}

/// Run one physical batch of sequences and pool each.
fn decode_group(
    context: &mut LlamaContext<'_>,
    batch: &mut LlamaBatch,
    group: &[&[LlamaToken]],
    dim: usize,
) -> Result<Vec<Vec<f32>>, String> {
    batch.clear();
    context.clear_kv_cache();
    for (seq, tokens) in group.iter().enumerate() {
        batch
            .add_sequence(tokens, i32::try_from(seq).expect("fits"), false)
            .map_err(|e| e.to_string())?;
    }
    context.decode(batch).map_err(|e| e.to_string())?;
    let mut vectors = Vec::with_capacity(group.len());
    for seq in 0..group.len() {
        let pooled = context
            .embeddings_seq_ith(i32::try_from(seq).expect("fits"))
            .map_err(|e| e.to_string())?;
        if pooled.len() != dim {
            return Err(format!(
                "model returned {}-dimensional vectors, expected {dim}",
                pooled.len()
            ));
        }
        vectors.push(normalise(pooled.to_vec()));
    }
    Ok(vectors)
}

/// L2-normalise, as the ORT path does; llama.cpp's pooled output is raw.
fn normalise(mut vector: Vec<f32>) -> Vec<f32> {
    let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut vector {
            *x /= norm;
        }
    }
    vector
}
