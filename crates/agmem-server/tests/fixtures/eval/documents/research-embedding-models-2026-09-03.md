# Embedding model & Apple-Silicon backend research — 2026-09-03

Scope: agmem embeds 1–3 sentence claims + queries locally through `ort`. Question:
is a bigger model worth it, how do we run it fast on an M4 Pro, and what does a
model swap cost an existing store. Web research only; nothing in the repo was
checked or changed. Numbers are quoted from primary sources where possible; where
a number is from a secondary aggregator or a search snippet it is marked (2ndry).

---

## 1. Retrieval / STS gains by model size

### 1.1 Two benchmark generations — do not mix columns

* **MTEB v1 "English" (56 tasks)**: what the bge / gte / e5 / MiniLM / mxbai /
  arctic-v1 model cards report. Retrieval column = 15 BEIR-ish datasets nDCG@10,
  STS column = Spearman on STS12-17, STS-B, SICK-R, BIOSSES.
* **MTEB English v2 (41 tasks)** / **MMTEB Multilingual v2**: what Qwen3-Embedding,
  EmbeddingGemma, granite-r2 and the current HF leaderboard report. Different task
  set; a v2 "Retrieval" of 61.8 is not comparable to a v1 retrieval of 54.3.
  (MMTEB paper: arxiv 2502.13595; MTEB paper: aclanthology 2023.eacl-main.148.)

### 1.2 Table (v1 columns unless marked v2)

| Model | Params | Dims (MRL) | Ctx | Retrieval | STS | MTEB avg | Licence | ONNX | Prefix / instruction |
|---|---|---|---|---|---|---|---|---|---|
| all-MiniLM-L6-v2 | 22M | 384 | 256 (512 max) | 41.95 | 78.90 | 56.26 | Apache-2.0 | yes (onnx-community, fastembed) | none |
| bge-small-en-v1.5 | 33M | 384 | 512 | 51.68 | 81.59 | 62.17 | MIT | yes (official) | query: "Represent this sentence for searching relevant passages: " (optional, for short-query→long-doc) |
| gte-small | 33M | 384 | 512 | 49.46 | 82.07 | 61.36 | MIT | yes (community) | none |
| snowflake-arctic-embed-xs | 22M | 384 | 512 | 50.15 | – | – | Apache-2.0 | yes (official) | query prefix as bge |
| snowflake-arctic-embed-s | 33M | 384 | 512 | 51.98 | – | – | Apache-2.0 | yes | query prefix as bge |
| granite-embedding-small-english-r2 | 47M | 384 | 8192 | BEIR 50.9; **v2** 61.1 | – | – | Apache-2.0 | yes (official + onnx-community) | none |
| e5-small-v2 | 33M | 384 | 512 | 49.04 | 80.39 | 59.93 | MIT | yes | "query: " / "passage: " |
| bge-base-en-v1.5 | 109M | 768 | 512 | 53.25 | 82.40 | 63.55 | MIT | yes (official) | as small |
| gte-base | 109M | 768 | 512 | 51.14 | 82.30 | 62.39 | MIT | yes | none |
| nomic-embed-text-v1.5 | 137M | 768 (MRL 64–768) | 8192 | 53.25 | – | 62.28 @768 / 61.04 @256 / 59.34 @128 | Apache-2.0 | yes (official) | **required**: "search_query: " / "search_document: " / "clustering: " / "classification: " |
| snowflake-arctic-embed-m (v1) | 110M | 768 | 512 | 54.90 | – | – | Apache-2.0 | yes | query prefix as bge |
| snowflake-arctic-embed-m-v1.5 | 109M | 768 (MRL→256) | 512 | ~55 (2ndry) | – | – | Apache-2.0 | yes | query prefix as bge |
| snowflake-arctic-embed-m-v2.0 | 113M (+embeddings; card ~305M incl. multilingual vocab) | 768 (MRL 256 keeps 99% MTEB-R) | 8192 | MTEB-R 55.4 nDCG | – | – | Apache-2.0 | yes | query prefix as bge |
| granite-embedding-english-r2 | 149M | 768 | 8192 | BEIR 53.1; **v2** 62.8 | – | – | Apache-2.0 | yes | none |
| EmbeddingGemma-300M | 308M | 768 (MRL 512/256/128) | 2048 | – | – | **v2 Eng** 68.36–69.67 @768; 66.66 @128 | Gemma licence | yes (onnx-community; fp32/q8/q4/mixed, **no fp16**) | **required**: "task: search result \| query: …", "title: none \| text: …", "task: sentence similarity \| query: …" |
| nomic-embed-text-v2-moe | 475M total / 305M active | 768 (MRL→256) | 512 | BEIR 52.86 | – | – | Apache-2.0 | not official | **required**: "search_query: " / "search_document: " |
| Qwen3-Embedding-0.6B | 0.6B, 28 layers | 1024 (MRL 32–1024) | 32k | **v2 Eng** 61.83 | **v2 Eng** 86.57 | **v2 Eng** 70.70 | Apache-2.0 | yes (onnx-community + official onnx/ dir; int8/uint8 community) | "Instruct: {task}\nQuery:{q}" on queries only; docs bare; last-token pooling |
| mxbai-embed-large-v1 | 335M | 1024 (MRL + binary) | 512 | 54.39 | **85.00** | 64.68 | Apache-2.0 | yes | query: "Represent this sentence for searching relevant passages: " |
| bge-large-en-v1.5 | 335M | 1024 | 512 | 54.29 | 83.11 | 64.23 | MIT | yes | as small |
| gte-large | 335M | 1024 | 512 | 52.22 | 83.35 | 63.13 | MIT | yes | none |
| e5-large-v2 | 335M | 1024 | 512 | 50.56 | 82.05 | 62.25 | MIT | yes | "query: " / "passage: " |
| snowflake-arctic-embed-l (v1) | 335M | 1024 | 512 | 55.98 | – | – | Apache-2.0 | yes | query prefix |
| snowflake-arctic-embed-l-v2.0 | 303M (568M incl. XLM-R vocab) | 1024 (MRL 256 keeps 98%) | 8192 | MTEB-R 55.6; CLEF 54.1; MIRACL 64.9 | – | – | Apache-2.0 | yes | query prefix |
| jina-embeddings-v3 | 570M (XLM-R + 5 task LoRAs) | 1024 (MRL→32) | 8192 | 53.87 (v1) | 85.80 (v1) | 65.52 (v1); MMTEB 58.37 (2ndry) | CC-BY-NC-4.0 | yes (official) | task adapter selected at inference: retrieval.query / retrieval.passage / text-matching / … |
| Qwen3-Embedding-4B | 4B, 36 layers | 2560 (MRL) | 32k | **v2 Eng** 68.46 | **v2 Eng** 88.72 | **v2 Eng** 74.60 | Apache-2.0 | yes (onnx-community; ~16 GB fp32) | as 0.6B |
| Qwen3-Embedding-8B | 8B, 36 layers | 4096 (MRL) | 32k | **v2 Eng** 69.44 | **v2 Eng** 88.58 | **v2 Eng** 75.22 | Apache-2.0 | yes (onnx-community; ~32 GB fp32) | as 0.6B |
| jina-embeddings-v4 | 3.8B (Qwen2.5-VL-3B) | 2048 single-vector (MRL 128–2048) + 128-d multivector | 32k | not on card (report arxiv 2506.18902) | – | – | **Qwen Research Licence** (non-commercial-ish; card corrects earlier CC-BY-NC label) | GGUF; no ONNX | task adapters: retrieval / text-matching / code |

Sources: bge card (huggingface.co/BAAI/bge-small-en-v1.5), gte card
(huggingface.co/thenlper/gte-large — has MiniLM/e5/gte rows), arctic-xs card
(huggingface.co/Snowflake/snowflake-arctic-embed-xs — arctic v1 + bge/gte/e5/nomic
retrieval rows), Arctic-Embed 2.0 paper (arxiv.org/html/2412.04506v1), nomic v1.5
card, nomic v2-moe card, mxbai card, granite-r2 card, EmbeddingGemma card +
onnx-community/embeddinggemma-300m-ONNX, Qwen3-Embedding-0.6B/8B cards, jina v3
paper (arxiv.org/html/2409.10173 Tables A2/A3), jina v4 card, codesota.com/benchmarks/mteb
(2ndry, MMTEB rows dated 2026-05-17).

### 1.3 What the numbers say

1. **Within one family, size buys ~1–3 points of v1 retrieval and ~1–2 of STS**:
   bge small→base→large = 51.7→53.3→54.3 retrieval, 81.6→82.4→83.1 STS; gte
   49.5→51.1→52.2 / 82.1→82.3→83.4; e5 49.0→50.3→50.6 / 80.4→81.1→82.1. Ten×
   the parameters for ~2.5 retrieval points and ~1.5 STS points.
2. **Training recipe beats size**: arctic-embed-m (110M) at 54.9 retrieval beats
   bge-large (335M) at 54.3; mxbai-large (335M) at 85.0 STS beats e5-large
   (335M) at 82.05; granite-english-r2 (149M) at BEIR 53.1 beats bge-base 46.9 on
   the same (IBM-run) harness. Arctic paper: "data quality matters more than
   quantity".
3. **The real step is the LLM-decoder generation**: on MTEB v2 English,
   Qwen3-Embedding-0.6B scores 70.70 mean / 61.83 retrieval / 86.57 STS,
   EmbeddingGemma-300M 68.4–69.7 mean. Going 0.6B→4B→8B within Qwen3: retrieval
   61.8→68.5→69.4, STS 86.6→88.7→88.6. So **4B is the knee; 8B adds nothing on
   STS and +1 on retrieval** for 2× the compute.
4. **MRL costs little**: nomic v1.5 loses 1.2 pts at 256-d and 2.9 at 128-d;
   EmbeddingGemma loses 0.5 at 512, 1.3 at 256, 3.0 at 128 (v2 Eng mean);
   arctic 2.0 keeps 98–99% of MTEB-R at 256-d; jina v3 retrieval 63.35→62.72 at
   1024→256 and still 58.5 at 64-d.
5. **Quantisation costs little**: EmbeddingGemma q8 −0.2, q4 −0.5 (v2 Eng mean);
   community uint8 Qwen3-0.6B −1.4% nDCG on SciFact vs fp32 (electroglyph card).

### 1.4 Does size help *short-text similarity / near-duplicate detection*?

* STS is the flattest axis. Across v1 models STS spans 78.9 (MiniLM) → 85.8
  (jina v3) while retrieval spans 42 → 56; the STS spread from 33M to 335M inside
  one family is ~1.5 points. On v2, 0.6B→8B Qwen3 moves STS 86.6→88.6 and
  4B≈8B.
* Sentence-T5 (arxiv 2108.08877) is the canonical scaling result for sentence
  embeddings: scaling to 11B gives consistent gains, but the paper's own
  discussion notes the 3B→11B STS gain is smaller than Large→3B, i.e. diminishing
  returns set in early on STS.
* MMTEB (arxiv 2502.13595) measured human performance on 16 datasets incl. STS:
  humans 77.6 vs best model 80.1, with models "near ceiling on some datasets" —
  STS is one of the saturated axes, so leaderboard STS deltas above ~85 are
  partly noise/label-ceiling.
* Jina's v5-text write-up (elastic.co/search-labs/blog/jina-embeddings-v5-text,
  2ndry) and Qwen3's own card both frame the small-model gains as coming from
  instruction-tuning and hard-negative mining, not parameter count.
* Practical reading for agmem: near-duplicate gating ("is this claim already
  stored?") is an STS-shaped task on 1–3 sentence inputs; expect a bge-small →
  Qwen3-0.6B jump of roughly +5 STS points (81.6→86.6, cross-benchmark, so
  ±1) and a further +2 to 4B. Recall of *paraphrased* claims (asking in words,
  not keywords) is retrieval-shaped and gets the bigger jump: 51.7→61.8-ish on
  the mixed scale, i.e. the biggest single win available is small-BERT →
  0.6B-decoder, not 0.6B → 8B.
* No paper found that isolates "near-duplicate detection on 1–3 sentence facts"
  by model size; STS-B / SICK-R and the MTEB PairClassification tasks
  (TwitterSemEval, SprintDuplicateQuestions) are the closest proxies.

---

## 2. Apple Silicon execution

### 2.1 ONNX Runtime CoreML EP (what `ort` can reach)

Source: onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html and
the gh-pages markdown (ORT 1.28 tracked by ort rc.13).

Provider options: `ModelFormat` = `NeuralNetwork` (default) | `MLProgram`
(Core ML 5+, macOS 12+); `MLComputeUnits` = `ALL` (default) | `CPUOnly` |
`CPUAndGPU` | `CPUAndNeuralEngine`; `RequireStaticInputShapes` 0/1 ("dynamic
shapes may negatively impact performance"); `EnableOnSubgraphs`;
`SpecializationStrategy` Default|FastPrediction; `ProfileComputePlan` (logs which
hardware each op was dispatched to); `AllowLowPrecisionAccumulationOnGPU`;
`ModelCacheDirectory` (skips recompiling the CoreML model each launch — matters
for a CLI that starts per invocation).

**Which unit runs it — ORT does not decide.** ORT hands CoreML a compiled
program; Core ML picks ANE/GPU/CPU per op within the allowed `MLComputeUnits`.
`CPUAndNeuralEngine` *forbids* the GPU; it does not force the ANE. The only way to
see the placement is `ProfileComputePlan=1` or Xcode's performance report.

**MLProgram op list (verbatim, ORT gh-pages):** Add, Argmax, AveragePool, Cast,
Clip, Concat, Conv, ConvTranspose, DepthToSpace, Div, Erf, Gemm (B constant),
Gelu, GlobalAveragePool, GlobalMaxPool, GridSample, GroupNormalization,
InstanceNormalization, LayerNormalization, LeakyRelu, MatMul (transA==0,
alpha==beta==1), MaxPool, Max, Mul, Pow (fp32 only), PRelu, Reciprocal,
ReduceSum, ReduceMean, ReduceMax, Relu, Reshape, Resize, Round, Shape, Slice,
Split, Sub, Sigmoid, Softmax, Sqrt, Squeeze, Tanh, Transpose, Unsqueeze.
**Absent from MLProgram: Gather, Where, Expand, Equal, Range, Attention,
com.microsoft:QuickGelu / SkipLayerNorm / EmbedLayerNorm.** Gather is on the
NeuralNetwork list only ("scalar indices not supported").

Consequences for a BERT/Qwen3 embedding graph:

* Token + position + type embedding lookups are `Gather` → CPU. The attention
  mask path (Where/Expand/Equal/Cast) → CPU. Everything between (MatMul/Gemm,
  LayerNorm, Softmax, Gelu/Erf) → CoreML. That is at least 2–3 partitions; the
  ORT log prints "number of partitions supported by CoreML: N".
* Every partition boundary is a CPU↔accelerator round-trip. Issue #28022
  (M3 Max, ORT 1.24.4): 14 CPU-fallback partitions turned a 27 ms model into 42
  ms. Issue #28183: running ORT's own `EXTENDED` graph optimiser (which fuses
  into QuickGelu) made the CoreML path 6× *slower* (9→55 ms) because the fused
  op isn't supported. **Set graph optimisation to `Basic` when using CoreML EP,
  or export a plain ONNX graph.**
* Qwen3 additionally needs RoPE (Sin/Cos/Neg/Concat, maybe fine) and a causal or
  bidirectional mask built with Where/Range — more CPU islands. KV-cache
  export variants add Gather/ScatterND; use the no-cache encoder export.
* Dynamic sequence length is allowed but "may negatively impact performance";
  CoreML on the ANE wants static shapes (issue #14212 history; Apple's
  ane_transformers guidance). For 1–3 sentence claims, pad to a fixed bucket
  (e.g. 64/128) and set `RequireStaticInputShapes=1`.
* Precision: NeuralNetwork format silently runs fp16 on GPU
  (ym2132.github.io/ONNX_MLProgram_NN_exploration); MLProgram keeps explicit
  dtypes. The **ANE is fp16-only** (Apple, ane_transformers). EmbeddingGemma
  "activations do not support fp16" → it is not an ANE candidate through any
  route; on CoreML EP force MLProgram and expect GPU/CPU.
* Older reports (#9433, #16934) of "CoreML EP runs on CPU" were partly
  hard-coded CPUOnly at predict time (fixed long ago) and partly the partition
  problem above (still true).

**`ort` crate (docs.rs/ort 2.0.0-rc.13, module `ort::ep::coreml`, feature
`coreml`)**: `CoreML::default().with_model_format(ModelFormat::MLProgram)
.with_compute_units(ComputeUnits::…).with_static_input_shapes(bool)
.with_subgraphs(bool).with_specialization_strategy(SpecializationStrategy::FastPrediction)
.with_profile_compute_plan(bool).with_low_precision_accumulation_on_gpu(bool)
.with_model_cache_dir(path).with_arbitrary_config(k, v).build()`. ort tracks
ORT 1.28.0. Whether pyke's prebuilt `download-binaries` for
aarch64-apple-darwin include the CoreML EP could not be confirmed from a
fetchable page (ort.pyke.io returned 403); the 1.x docs said only CUDA/TensorRT
were prebuilt. **Verify by building with `coreml` on and checking
`CoreML::default().is_available()`; otherwise ORT must be built from source or
linked via `ORT_LIB_LOCATION`.** fastembed-rs (Anush008) sits on ort, lists
bge/MiniLM/nomic/gte/arctic/EmbeddingGemma ONNX variants with `Q` (quantised)
suffixes, but documents only DirectML as a GPU EP — no CoreML path exercised.

Bottom line on ANE-via-ORT: **possible but not the design point.** ORT's CoreML EP
gives partial offload with CPU islands; nobody has published a BERT/Qwen3
embedding latency for it on M-series. The tools that *do* hit the ANE for
embeddings bypass ORT: Apple's `ane_transformers` (DistilBERT 3.47 ms @ seq 128
on iPhone 13, 10× faster / 14× less memory than baseline, fp16, channels-first
4D layout, Linear→Conv2d), eeANE (PyPI `eeane`: BERT / XLM-R / ModernBERT
encoders on ANE, "~13,600 padding-excluded tokens/s on an M2 Mac mini, 2–3× the
PyTorch-MPS baseline, GPU idle"), ANEForge (github.com/sbryngelson/ANEForge,
research, private `aned` symbols: all-MiniLM-L6-v2 **0.53 ms / 2.4 mJ per
encode on M5 Pro**, cosine 1.0000 vs reference), smpanaro/ModernBERT-
AppleNeuralEngine. A hand-rolled coremltools conversion of MiniLM
(jano.dev/swift/2025/09/29/minilm-coreml.html: fp16, fixed seq 512, MLProgram,
ComputeUnit.ALL) landed on the **GPU at 0.3–0.4 W vs 5–13 W on CPU**, with the
ANE declining attention/GELU as exported.

### 2.2 Metal / MPS paths (GPU, never ANE)

* **candle** (huggingface/candle, `Device::Metal`): has `bert` (used by the
  official bert/e5 examples), `xlm-roberta`, `modernbert`, `gemma3`, `qwen3`
  decoders in candle-transformers; Qwen3-Embedding = Qwen3 decoder + last-token
  pooling + normalise — feasible, not a shipped example. Known wart: issue
  #1062 (open) — BERT example slower than HF transformers on M1 at larger
  batches; Metal kernels for masked SDPA are not fused. GarthDB/metal-candle
  advertises "25.9× faster embeddings than MLX" in its README but its
  BENCHMARKS.md shows MLX 5–13× faster on LoRA matmuls and contains no embedding
  benchmark — treat that README claim as unverified.
* **mistral.rs** (EricLBuehler): embeddings supported (EMBEDDINGS.md, Metal
  and CUDA backends, ISQ quantisation) for **EmbeddingGemma and Qwen3-Embedding**;
  BGE/BERT not listed. Rust crate + HTTP; heavy dependency for a CLI.
* **llama.cpp** (`llama-embedding`, `--embd-normalize`, `--pooling`): GGUFs exist
  for bge-small/base/large, bge-m3, nomic v1.5/v2-moe, arctic, Qwen3-Embedding
  0.6B/4B/8B (official Qwen GGUFs), jina v4. Metal works. GGUF discussion #8 on
  the Qwen3-0.6B repo: early builds returned token embeddings instead of pooled —
  use `--pooling last`. Rust bindings: `llama-cpp-2` crate (utilityai). Metal
  offloads matmuls only; small encoders are launch-overhead-bound at batch 1.
* **MLX**: Python `mlx-embeddings` (Blaizzy) — BERT, XLM-R, ModernBERT, Qwen3,
  Gemma; 4-bit variants (e.g. all-MiniLM-L6-v2-4bit). Rust: `mlx-rs`
  (oxiglade, unofficial, tracks mlx-c; `nn::Embedding` exists, no packaged
  embedding-model zoo) — not production-ready for shipping in a CLI.
* **coremltools direct**: convert PyTorch → MLProgram, load through
  `objc2-core-ml` or a small Swift shim. Reaches ANE for BERT-shaped encoders
  only after the ane_transformers rewrite (channels-first, Conv2d, static seq);
  a naive trace lands on the GPU (jano.dev result). Adds a macOS-only code path
  outside ort.

### 2.3 Published M-series numbers (sparse; none for CoreML-EP-through-ORT)

| Setup | Chip | Number | Source |
|---|---|---|---|
| all-MiniLM-L6-v2 on ANE (ANEForge, fp16) | M5 Pro | 0.53 ms/encode, 2.4 mJ | github.com/sbryngelson/ANEForge |
| DistilBERT seq128 b1 on ANE (ane_transformers) | A15 (iPhone 13) | 3.47 ms; 10× vs baseline | machinelearning.apple.com/research/neural-engine-transformers |
| BERT/XLM-R embedders on ANE (eeANE) | M2 Mac mini | ~13,600 tok/s; 2–3× PyTorch-MPS; 2.6–3.8× energy/token | pypi.org/project/eeane (search snippet) |
| BERT cross-encoder reranker on ANE | M5 Pro | ~0.8 ms/pair | eeANE/ANEForge (snippet) |
| MiniLM CoreML MLProgram fp16, naive conversion | M-series (unspecified) | GPU 0.3–0.4 W vs CPU 5–13 W; ANE refused attention/GELU | jano.dev blog |
| bge-m3, MLX, batch 64 × 512 tok | M5 Max 128 GB | ~4,800 passages/s; MLX ≈ +50% vs llama.cpp on embedding (prefill-bound) | contracollective.com blog (2ndry, snippet only — page 404'd on fetch) |
| llama3.2-1B on ANE (CoreML-LLM) | M4 Pro | 62 tok/s @ ~2.8 W | brightcoding.dev / john-rocky (LLM, not embedding) |
| CoreML-EP with CPU fallback partitions | M3 Max | 14 partitions: 42 ms vs 27 ms fused | ORT issue #28022 |

Take-away: at batch 1 on 1–3 sentence inputs, a 22–110M encoder is ~1–5 ms on
CPU already; the ANE gets that to ~0.5 ms and 10× less energy, the GPU saves
power but not much latency. A 0.6B decoder (28 layers, 1024-d) at fp16 is
~1.2 GB of weights; at seq 64 batch 1 it is bandwidth-bound: ~1.2 GB / 273 GB/s
(M4 Pro) ≈ 4–5 ms floor on GPU, realistically 10–25 ms; on CPU fp32 int8
30–100 ms. 4B fp16 ≈ 8 GB → ~30 ms floor, realistically 60–150 ms on GPU;
CPU-only is multi-hundred ms. (Estimates, not measurements.)

---

## 3. Recommendation candidates for an M4 Pro (24–48 GB), interactive latency

Ranked; quality deltas quoted against bge-small-en-v1.5 (v1: 51.7 R / 81.6 STS).

1. **Qwen3-Embedding-0.6B, MRL-truncated to 512 or 256-d, int8/uint8 ONNX on
   ORT CPU (portable) with optional CoreML EP.** Best quality-per-byte in the
   open set: v2 Eng 70.7 mean, STS 86.6. ~600 MB int8; community uint8 loses
   ~1.4% nDCG. Instruction on queries only; documents bare; last-token pooling
   (ORT graph must not include mean pooling). Expected delta vs bge-small: ~+5
   STS, ~+10 retrieval-equivalent, i.e. the largest jump available. Cost: ~10–20×
   the compute of bge-small; interactive at batch 1 (tens of ms on CPU int8,
   single-digit-to-low-tens on Metal/CoreML GPU). Apache-2.0.
2. **EmbeddingGemma-300M, 768-d or MRL 256, q8 ONNX (onnx-community).** v2 Eng
   68.4–69.7; fp32/q8/q4 only (no fp16 → never ANE, GPU via MLProgram fp32 at
   best). Requires task prefixes on *both* sides ("task: sentence similarity |
   query:" is the right one for dedupe). ~300 MB q8. Gemma licence (usage
   restrictions — check before bundling). Delta vs bge-small ≈ +4 STS, big
   retrieval gain; ~half the compute of Qwen3-0.6B.
3. **Stay small but modern: snowflake-arctic-embed-m-v2.0 (MRL 256) or
   granite-embedding-english-r2 (149M).** Both Apache-2.0, official ONNX,
   8192 ctx, BERT-shaped (CoreML EP handles all but Gather/mask; eeANE-style
   ANE path exists). Retrieval +2–4 over bge-small on v1-scale; STS gain small.
   Choose this if latency/size must stay where it is.

Skip: Qwen3-Embedding-4B/8B for this workload — 4B adds +2 STS / +6.6 retrieval
(v2) over 0.6B for 7× the weights; 8B adds nothing on STS. jina v3/v4 for
licence (CC-BY-NC / Qwen Research). nomic-v2-moe: MoE export to ONNX is
awkward and it is not ahead of arctic-m-v2.0 on BEIR.

Backend on macOS, in order: (a) ORT CPU int8 with the pooled/normalised head
kept *outside* the graph — works today; (b) add `CoreML` EP with
`MLProgram`, `CPUAndGPU` first (fp32-safe), `ModelCacheDirectory`, graph
optimisation ≤ Basic, static-shape bucketed inputs, and measure with
`ProfileComputePlan` — expect GPU offload of matmuls with 2–3 CPU islands; try
`ALL` and check whether anything lands on ANE (for fp16 BERT-shaped models it
may, for Qwen3/Gemma assume not); (c) if the GPU path is wanted without ORT's
partition tax, llama.cpp via `llama-cpp-2` with the official Qwen3-Embedding
GGUF and `--pooling last` is the least-code Metal route, at the price of a
second inference stack in the binary.

Portable fallback (Linux/Windows): same int8 ONNX on ORT CPU is the baseline
(Ryzen: uint8 ~25% faster than fp32 per the electroglyph card); CUDA EP via
ort's `cuda` feature where present (note fastembed's warning that some
CPU-optimised quantised graphs — ConvInteger/DynamicQuantizeLinear — fail on
GPU EPs, so ship a static-int8 or fp16 graph for the GPU path and the dynamic
one for CPU); DirectML on Windows is the third option. Keep dims fixed across
platforms (MRL truncation in agmem code, not in the graph) so vectors are
identical modulo quantisation noise.

---

## 4. Migration when the embedding model changes

Principles (Qdrant migration guide; levelop.dev "Vector Embedding Models:
Generation, Versioning, and Drift"; hackernoon "Your Embedding Model Will
Deprecate"; open-webui discussion #7604; OpenViking issue #1066):

1. **Vectors from different models are incompatible** — even same-dims,
   same-family. No mixing in one ANN index; a query embedded with model B
   against model-A vectors is noise. MRL truncation of the *same* model is the
   one exception (prefix of the same vector).
2. **Embeddings are derived data**: source text + model id + model revision +
   dims + prefix/instruction template + pooling/normalisation + quantisation =
   the cache key. Store all of these per vector (or per vector-set), plus
   `embedded_at`. A content hash makes re-embedding idempotent and lets a
   background job find "still on old model" rows.
3. **Model-id tagging**: at minimum `embedding_model` (e.g.
   `qwen3-embedding-0.6b@<hf-revision>`), `dims`, `prefix_schema`. Detect on
   startup: stored model ≠ configured model → refuse to search across, and
   schedule re-embed.
4. **Dims change** forces a new table/index (sqlite-vec `vec0` is fixed-dim per
   virtual table; Qdrant makes you create a new collection or a new *named
   vector*). Pattern: `vectors_<model_slug>` table per model; the live one is
   named by a pointer row (alias). Blue-green: write new vectors alongside old,
   cut over when 100% re-embedded, drop old after a grace period.
5. **Re-embedding cost is bounded by store size**: an agmem store is thousands
   of 1–3 sentence claims, not millions of chunks — one-shot re-embed on first
   run of the new binary is seconds to a couple of minutes even on CPU; do it
   eagerly at upgrade rather than lazily, so recall never sees a mixed index.
   Dual-write only matters if two binaries of different versions can write the
   same store concurrently (worktrees sharing one store do).
6. **Prefix/instruction drift is a silent model change**: changing the
   instruction string on Qwen3, or the `task:` prefix on EmbeddingGemma, or
   adding the bge query prefix, moves the vectors; hash the template into the
   model id.
7. **Validation before cutover**: re-embed, run a fixed set of recall queries
   against both indexes, compare top-k overlap and a handful of known
   duplicate pairs' cosine; only then flip the alias.

---

## Open questions / not verified

* Whether pyke's prebuilt ORT binaries for aarch64-apple-darwin include the
  CoreML EP in 2.0 (site 403'd). Check `CoreML::default().is_available()`.
* No published number for a BERT or Qwen3 encoder through **ORT's** CoreML EP
  on any M-series chip; every ANE figure above is via coremltools /
  ane_transformers / eeANE / ANEForge.
* contracollective M5 Max embedding tables were only visible through search
  snippets (page 404 on fetch); the "MLX ≈ +50% over llama.cpp for embedding"
  and "bge-m3 ~4,800 passages/s @ batch 64" figures are from those snippets.
* jina-embeddings-v4 benchmark scores were not pulled (report arxiv
  2506.18902); licence alone rules it out for bundling.
