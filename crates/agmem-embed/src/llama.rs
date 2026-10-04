//! The production backend: a GGUF model on llama.cpp.
//!
//! Metal on Apple Silicon, the CPU everywhere else, chosen by
//! [`Accelerator`]; measured against the ONNX Runtime path it replaced in
//! `docs/eval/llama-runtime.md` (EmbeddingGemma Q8_0 at 11.4 ms per claim on
//! Metal against 72.6 ms on the CPU, vectors within 0.9998 cosine of the
//! fp32 ONNX export; issue #178).
//!
//! `llama-cpp-2`'s context is `!Send` and borrows its model, and this
//! workspace denies `unsafe_code`, so the model, context and batch live on
//! one worker thread and callers hand it work over a channel. That also
//! keeps every embed call serial, which is what a mutex around any other
//! engine would do.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use agmem_core::dedup::Thresholds;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::token::LlamaToken;

use crate::accelerator::{Accelerator, Active};
use crate::model::{self, Model, ModelSpec, Pooling};
use crate::{EmbedError, Embedder};

/// Longest input, in tokens; what the store's chunker is sized for.
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

/// What the worker's channel carries.
enum Msg {
    Embed(Job),
    /// How long the worker keeps the model with nothing to do; `None` keeps
    /// it for good.
    UnloadAfter(Option<Duration>),
}

/// A GGUF model loaded on its own thread.
pub struct LlamaEmbedder {
    spec: ModelSpec,
    accelerator: Active,
    /// `None` only while dropping: the sender goes first so the worker sees
    /// end-of-jobs and tears the model down on its own thread.
    jobs: Option<Mutex<mpsc::Sender<Msg>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

/// Teardown waits for the worker: the model and its context are freed on
/// that thread, and a process that exits while llama.cpp is still releasing
/// Metal buffers aborts in the driver (seen as a SIGABRT after `--doctor`
/// had already printed its report). Joining makes the drop synchronous, so
/// whoever drops the last `Arc` — `main` returning, `doctor` finishing —
/// leaves a clean process behind.
impl Drop for LlamaEmbedder {
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl std::fmt::Debug for LlamaEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlamaEmbedder")
            .field("model", &self.spec.id)
            .field("dim", &self.spec.dim)
            .field("accelerator", &self.accelerator.as_str())
            .finish()
    }
}

impl LlamaEmbedder {
    /// Load `model`, fetching its weights once into the model dir.
    ///
    /// `fallback_model_dir` is used only when [`model::MODEL_DIR_ENV`] is
    /// unset — the environment variable stays authoritative (CI points it
    /// somewhere unwritable on purpose, to catch an unintended download).
    /// `accelerator` settles `auto` here, once, so what this backend
    /// reports is what its thread runs on.
    ///
    /// # Errors
    /// [`EmbedError::Backend`] when the weights cannot be fetched or loaded,
    /// or Metal was asked for by name on a build without it.
    pub fn new(
        model: Model,
        fallback_model_dir: Option<PathBuf>,
        accelerator: Accelerator,
    ) -> Result<Self, EmbedError> {
        let active = accelerator.resolve()?;
        let spec = model.spec();
        let dir = model::model_dir(fallback_model_dir);
        let path = model::fetch(&spec, &dir)?;
        tracing::info!(
            model = spec.id,
            dim = spec.dim,
            accelerator = active.as_str(),
            path = %path.display(),
            "loading embedding model"
        );
        Self::load(spec, &path, active)
    }

    /// Load the GGUF at `path` as `spec`, offloading every layer to Metal
    /// when `accelerator` is [`Active::Metal`] and none otherwise.
    ///
    /// # Errors
    /// The llama.cpp message when the backend, the model or the context will
    /// not come up, or the file's width is not the spec's.
    pub fn load(spec: ModelSpec, path: &Path, accelerator: Active) -> Result<Self, EmbedError> {
        let failed = |message: String| EmbedError::Backend {
            backend: spec.id,
            message,
        };
        let gpu_layers: u32 = match accelerator {
            Active::Metal => 1000,
            Active::Cpu => 0,
        };
        let (jobs, inbox) = mpsc::channel::<Msg>();
        let (ready, loaded) = mpsc::channel::<Result<(), String>>();
        let path = path.to_path_buf();
        let handle = std::thread::Builder::new()
            .name(format!("llama-{}", spec.id))
            .spawn(move || worker(&path, spec.pooling, spec.dim, gpu_layers, &ready, &inbox))
            .map_err(|e| failed(e.to_string()))?;
        loaded
            .recv()
            .map_err(|_| failed("the model thread exited before loading".to_owned()))?
            .map_err(failed)?;
        // What it was asked for is what it reports: there is no fallback from
        // Metal to the CPU on purpose, so a row measured under `metal` ran
        // on it.
        Ok(Self {
            spec,
            accelerator,
            jobs: Some(Mutex::new(jobs)),
            worker: Some(handle),
        })
    }

    /// Everything true of the loaded model.
    #[must_use]
    pub fn spec(&self) -> &ModelSpec {
        &self.spec
    }

    /// Free the model, its context and their Metal buffers after `idle`
    /// with no embed call, and load them again on the next one; `None` keeps
    /// them loaded for good, which is also what a new embedder does.
    ///
    /// A daemon lives for days and embeds for seconds of them, and what it
    /// holds while idle is the model plus a compute buffer sized for a
    /// 2048-token batch (issue #196). The price is one load — about the
    /// warm-up `new` already pays — on the first call after an idle spell.
    pub fn unload_after(&self, idle: Option<Duration>) {
        if let Some(Ok(jobs)) = self.jobs.as_ref().map(Mutex::lock) {
            // A worker that has exited has nothing loaded to free.
            let _ = jobs.send(Msg::UnloadAfter(idle));
        }
    }

    /// Embed already-prefixed texts, in order, L2-normalised.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let failed = |message: String| EmbedError::Backend {
            backend: self.spec.id,
            message,
        };
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let (reply, answer) = mpsc::channel();
        self.jobs
            .as_ref()
            .ok_or_else(|| failed("the model is being dropped".to_owned()))?
            .lock()
            .map_err(|_| failed("the model lock was poisoned by an earlier panic".to_owned()))?
            .send(Msg::Embed(Job {
                texts: texts.to_vec(),
                reply,
            }))
            .map_err(|_| failed("the model thread has exited".to_owned()))?;
        answer
            .recv()
            .map_err(|_| failed("the model thread exited mid-batch".to_owned()))?
            .map_err(failed)
    }
}

impl Embedder for LlamaEmbedder {
    fn dim(&self) -> usize {
        self.spec.dim
    }

    fn model_id(&self) -> &str {
        self.spec.id
    }

    fn revision(&self) -> Option<&str> {
        Some(self.spec.revision())
    }

    fn thresholds(&self) -> Thresholds {
        self.spec.thresholds
    }

    fn accelerator(&self) -> &str {
        self.accelerator.as_str()
    }

    fn embed_passages(&self, passages: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let prefix = self.spec.passage_prefix;
        let texts: Vec<String> = passages
            .iter()
            .map(|passage| format!("{prefix}{passage}"))
            .collect();
        self.embed(&texts)
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        let prefix = self.spec.query_prefix;
        let mut vectors = self.embed(&[format!("{prefix}{query}")])?;
        vectors.pop().ok_or_else(|| EmbedError::Backend {
            backend: self.spec.id,
            message: "model returned no vector for the query".to_owned(),
        })
    }
}

/// The thread body: load, report, then serve jobs until the sender drops —
/// unloading after an idle spell and loading again on the next job, when
/// [`LlamaEmbedder::unload_after`] asked for that.
fn worker(
    path: &Path,
    pooling: Pooling,
    dim: usize,
    gpu_layers: u32,
    ready: &mpsc::Sender<Result<(), String>>,
    inbox: &mpsc::Receiver<Msg>,
) {
    let backend = match backend() {
        Ok(backend) => backend,
        Err(message) => {
            let _ = ready.send(Err(message));
            return;
        }
    };
    let load = Load {
        backend,
        path,
        pooling,
        dim,
        gpu_layers,
    };
    let mut idle = None;
    let mut first = Some(ready);
    let mut pending = None;
    loop {
        if load.serve(first.take(), pending.take(), &mut idle, inbox) == Ended::Closed {
            return;
        }
        // Unloaded: nothing is held until the next job arrives.
        loop {
            match inbox.recv() {
                Err(_) => return,
                Ok(Msg::UnloadAfter(after)) => idle = after,
                Ok(Msg::Embed(job)) => {
                    pending = Some(job);
                    break;
                }
            }
        }
    }
}

/// Why [`Load::serve`] returned.
#[derive(Debug, PartialEq, Eq)]
enum Ended {
    /// Every sender is gone: the embedder was dropped.
    Closed,
    /// The model is freed — idle, or a reload that failed — and the worker
    /// waits for the next job before loading again.
    Unloaded,
}

/// What one load of the model needs.
struct Load<'a> {
    backend: &'static LlamaBackend,
    path: &'a Path,
    pooling: Pooling,
    dim: usize,
    gpu_layers: u32,
}

impl Load<'_> {
    /// Load the model and serve jobs until the channel closes or `idle`
    /// passes with none.
    ///
    /// Model, context and batch are locals here rather than fields of a
    /// struct because the context borrows the model; returning frees all
    /// three. `ready` is the first load's: it is told the outcome after a
    /// warm-up, and a failure there ends the thread as before. A reload has
    /// `pending` instead, the job that woke it, which is its warm-up and
    /// which hears about a failed load in its place.
    fn serve(
        &self,
        ready: Option<&mpsc::Sender<Result<(), String>>>,
        pending: Option<Job>,
        idle: &mut Option<Duration>,
        inbox: &mpsc::Receiver<Msg>,
    ) -> Ended {
        let fail = |message: String| match (ready, pending.as_ref()) {
            (Some(ready), _) => {
                let _ = ready.send(Err(message));
                Ended::Closed
            }
            (None, Some(job)) => {
                tracing::warn!(error = %message, "reloading the embedding model failed");
                let _ = job.reply.send(Err(message));
                Ended::Unloaded
            }
            (None, None) => Ended::Unloaded,
        };
        let model_params = LlamaModelParams::default().with_n_gpu_layers(self.gpu_layers);
        let model = match LlamaModel::load_from_file(self.backend, self.path, &model_params) {
            Ok(model) => model,
            Err(e) => return fail(format!("load {}: {e}", self.path.display())),
        };
        let n_embd = usize::try_from(model.n_embd_out()).unwrap_or(0);
        if n_embd != self.dim {
            return fail(format!(
                "{} is {n_embd}-dimensional, expected {}",
                self.path.display(),
                self.dim
            ));
        }
        let threads =
            i32::try_from(std::thread::available_parallelism().map_or(4, |n| n.get())).unwrap_or(4);
        let batch_tokens = u32::try_from(BATCH_TOKENS).expect("fits");
        let context_params = LlamaContextParams::default()
            .with_embeddings(true)
            .with_pooling_type(self.pooling.llama())
            .with_n_ctx(NonZeroU32::new(batch_tokens))
            .with_n_batch(batch_tokens)
            .with_n_ubatch(batch_tokens)
            .with_n_seq_max(u32::try_from(MAX_SEQUENCES).expect("fits"))
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let mut context = match model.new_context(self.backend, context_params) {
            Ok(context) => context,
            Err(e) => return fail(e.to_string()),
        };
        let mut batch = LlamaBatch::new(BATCH_TOKENS, i32::try_from(MAX_SEQUENCES).expect("fits"));
        let truncate_to =
            MAX_TOKENS.min(usize::try_from(model.n_ctx_train()).unwrap_or(MAX_TOKENS));
        let runtime = Runtime {
            model: &model,
            dim: self.dim,
            truncate_to,
        };
        if let Some(ready) = ready {
            // One warm-up so the first Metal shader compile is paid before
            // any caller waits on it, and a broken parameter set fails at
            // load.
            if let Err(message) = runtime.embed(&mut context, &mut batch, &["warm-up".to_owned()]) {
                let _ = ready.send(Err(message));
                return Ended::Closed;
            }
            let _ = ready.send(Ok(()));
        } else {
            tracing::info!("reloaded the embedding model");
        }
        if let Some(job) = pending {
            let _ = job
                .reply
                .send(runtime.embed(&mut context, &mut batch, &job.texts));
        }
        loop {
            let msg = match *idle {
                Some(after) => match inbox.recv_timeout(after) {
                    Ok(msg) => msg,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        tracing::info!(
                            idle_secs = after.as_secs(),
                            "unloaded the embedding model until the next embed"
                        );
                        return Ended::Unloaded;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ended::Closed,
                },
                None => match inbox.recv() {
                    Ok(msg) => msg,
                    Err(_) => return Ended::Closed,
                },
            };
            match msg {
                Msg::Embed(job) => {
                    let _ = job
                        .reply
                        .send(runtime.embed(&mut context, &mut batch, &job.texts));
                }
                Msg::UnloadAfter(after) => *idle = after,
            }
        }
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

/// L2-normalise; llama.cpp's pooled output is raw, and every cosine band in
/// [`Thresholds`] assumes unit vectors.
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
    fn normalising_yields_a_unit_vector_and_leaves_zero_alone() {
        let unit = normalise(vec![3.0, 4.0]);
        assert!((unit[0] - 0.6).abs() < 1e-6 && (unit[1] - 0.8).abs() < 1e-6);
        assert_eq!(normalise(vec![0.0, 0.0]), vec![0.0, 0.0]);
    }
}
