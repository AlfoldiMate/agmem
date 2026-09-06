# macOS acceleration through the CoreML execution provider (issue #139)

Written before any number was computed. The bar below is the decision; the
Results section is filled in afterwards and does not move the bar.

## What is being decided

Whether registering ONNX Runtime's CoreML execution provider on the embedding
session — same graph, same runtime, one more provider ahead of the CPU one —
buys enough latency or energy on Apple silicon to ship behind a feature.
Nothing else is on the table: no second inference runtime (llama.cpp, candle,
MLX), by the user's decision on 2026-09-03, and no change to the portable
CPU default on any platform.

The provider is already in the binary. pyke's only prebuilt ONNX Runtime for
`aarch64-apple-darwin` (ort-sys 2.0.0-rc.13, ORT 1.28.0) is the `+coreml`
build, and it is what every macOS agmem links today. The `coreml` cargo
feature turns on `ort`'s Rust surface for it; `--accelerator auto|cpu|coreml`
(`AGMEM_ACCELERATOR`) chooses at start-up, `auto` settling to CoreML only on
a build with the feature and a machine where the runtime reports it.

## What Core ML can and cannot take

Core ML's MLProgram op set covers MatMul/Gemm, LayerNormalization, Softmax,
Gelu/Erf — the transformer body — and lacks Gather, Where, Expand, Equal,
Range, the fused `com.microsoft` attention ops, and every dynamic-quantisation
op (`MatMulInteger`, `DynamicQuantizeLinear`). Consequences:

- The embedding lookups and the attention-mask path stay on the CPU, so a
  BERT graph splits into at least 2–3 partitions with a copy at each
  boundary (onnxruntime#28022: 14 partitions turned 27 ms into 42 ms).
- **A quantised graph on the CoreML provider is a CPU graph with extra
  boundaries.** The shipped `bge-small-en-v1.5-q` and the two 768-d `-q` /
  `-int8` candidates can be *run* under `coreml` — and the row is recorded,
  because a slowdown there is what a user of the feature would get — but
  only the fp32 exports (`bge-small-en-v1.5`, `embeddinggemma-300m`,
  `arctic-embed-m-v2.0`) measure what the accelerator does.
- The Neural Engine is fp16-only. `ComputeUnits::All` lets Core ML place
  fp16-safe ops there; EmbeddingGemma's card forbids fp16, so it is a GPU
  candidate at best.
- ORT's own level-2/3 fusions produce CPU-only contrib ops; they run after
  partitioning on the nodes the CPU kept, so they should not steal nodes
  from Core ML — but onnxruntime#28183 measured a 6× slowdown from exactly
  such a fusion. fastembed hard-codes `Level3`; if the CoreML rows are slower
  than the CPU rows on fp32, the first suspect is this, and the check is a
  direct `ort` session at `Level1` (the shape `candidates.rs` already uses
  for Qwen3), not a production change.

## The bar

The feature ships enabled in the macOS release, with `auto` as the default,
only if **both** of these hold, measured on the fp32 export of the shipped
model's architecture (`bge-small-en-v1.5`) on Apple silicon:

1. **Speed or power.** p50 for one 3-sentence claim on `coreml` ≤ 0.7× the
   same model's p50 on `cpu`, **or** energy per claim (`powermetrics`,
   package power over a 60-second loop) ≤ 0.3× the CPU run's.
2. **Drift.** Cosine between the `coreml` vector and the `cpu` vector ≥
   0.999 on every text of the eval fixture (`vectors.json`: all passages
   and queries). fp16 on the Neural Engine is the expected cause of a miss;
   the fallback is `CPUAndGPU`, re-measured against the same bar.

Fails both halves of 1, or fails 2 with the fallback → the issue closes as
measured-and-dropped; the feature stays in the tree, off by default, as a
measurement surface like `candidates`, and the CPU path stays. Passes →
the release build turns the feature on for `aarch64-apple-darwin` and
`doctor` prints the active provider (it does already).

Separately recorded, not part of the bar: the same rows for
`embeddinggemma-300m` and `arctic-embed-m-v2.0`, the two #133 candidates
that failed only on latency and abstention. If the accelerator brings
Gemma's 72.6 ms claim under the #133 bar of 60 ms, that is a #138 input,
not a #139 verdict.

## Method

- `cargo test -p agmem-embed --features candidates,coreml --release --test
  candidates -- --ignored --nocapture latency` with `AGMEM_CANDIDATE=<id>`
  and `AGMEM_ACCELERATOR=cpu|coreml`, for the four `-q`/`-int8` ids and the
  three fp32 ids. Rows go to `docs/eval/embed-models/latency.json`, one per
  (model, accelerator, shape); a re-run replaces. The fp32 `cpu` rows are the
  denominators of bar item 1.
- `cargo test -p agmem-embed --features coreml --release --test fastembed --
  --ignored --nocapture coreml_vectors_match_cpu` is bar item 2.
- `agmem --doctor --accelerator coreml` shows the provider load end-to-end.
- Energy: `sudo powermetrics --samplers cpu_power,gpu_power,ane_power -i 1000
  -n 60` alongside a 60-second embed loop, once per accelerator; by hand,
  since it needs root.

## Results

Run on 2026-09-05/06 on an **Apple M1 Pro** (the issue names an M4 Pro; none
was available), macOS 27, `--release`, ORT 1.28.0 via ort 2.0.0-rc.13 /
fastembed 6.0.2, CoreML provider as `MLProgram` + `ComputeUnits::All`, ORT
graph optimisation at fastembed's `Level3`. Rows in
`docs/eval/embed-models/latency.json`; the CoreML rows for the document
shape are absent because neither run finished (below).

### Bar item 2 — drift: **pass**

`coreml_vectors_match_cpu` over the 90 fixture texts (80 passages, 10
queries) of `bge-small-en-v1.5-q`: min cosine 0.999999, mean 1.000000. The
quantised graph keeps its matmuls on the CPU, so this is the expected
near-identity rather than evidence about fp16 on the Neural Engine; the fp32
drift was not measured separately once item 1 had failed.

### Bar item 1 — speed: **fail**

| Model | Shape | cpu p50 | coreml p50 | ratio (bar ≤ 0.7) |
|---|---|---|---|---|
| `bge-small-en-v1.5` (fp32) | one claim | 11.9 ms | 22.0 ms | **1.85×** |
| `bge-small-en-v1.5` (fp32) | 16 claims | 136.0 ms | 306.6 ms | 2.25× |
| `bge-small-en-v1.5` (fp32) | 60-chunk document | 4850 ms | killed (SIGKILL) | — |
| `bge-small-en-v1.5-q` (shipped) | one claim | 15.7 ms | 25.3 ms | 1.61× |
| `bge-small-en-v1.5-q` (shipped) | 16 claims | 127.8 ms | 175.5 ms | 1.37× |
| `bge-small-en-v1.5-q` (shipped) | 60-chunk document | 3893 ms | killed (SIGKILL) | — |

Load time went from 64 ms to 3.6–5.9 s (the Core ML compile; the model
cache under `<models>/coreml` did not shorten it on the second run, so the
cache key ORT wants in the model's metadata is missing from these exports).

The document shape is the finding that matters beyond the bar: on both
graphs the CoreML session ran past 10 minutes at ~3 GB resident and was
killed by the system. fastembed pads to the longest row of each batch, so
the 60 chunks arrive as a fresh input shape, and Core ML re-specialises the
compiled program per shape; a real `remember` of a long document would hang
the daemon. `with_static_input_shapes` plus padding to fixed buckets would
remove that, at the cost of padding every claim to the bucket — and item 1
already fails on the single-claim shape that would benefit least.

Two incidental observations. On this chip the fp32 `bge-small-en-v1.5` is
*faster* on the CPU than the shipped int8 export for one claim (11.9 vs
15.7 ms) and equal at 16, which is a #138-adjacent note, not a #139 result.
And EmbeddingGemma / arctic were not run: an accelerator that slows the
architecture it was meant to help has nothing to offer the models that
failed #133 on latency.

### Not measured

Energy (`powermetrics`) needs root and was not run. It is the one half of
bar item 1 that could still pass; the reopen condition below covers it.

### Verdict

**Measured and dropped.** The `coreml` feature stays in the tree, off by
default, as a measurement surface like `candidates`; `--accelerator` keeps
its three spellings so the switch is one flag when the runtime changes.
Nothing in a release build changes. Reopen when one of these holds:

- ONNX Runtime's CoreML provider gains the ops that keep BERT's embedding
  and mask path on the CPU (Gather, Where, Expand, Equal), or a
  Core-ML-native export of the model exists for a runtime agmem already
  links.
- A `powermetrics` run on Apple silicon shows package energy per claim
  under 0.3× the CPU run *and* a static-shape session with bucketed padding
  stops the document-shape hang.
