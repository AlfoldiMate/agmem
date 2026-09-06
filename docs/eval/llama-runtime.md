# llama.cpp with Metal as a second embedding runtime (issue #178)

Written before any number was computed. The bar below is the decision; the
Results section is filled in afterwards and does not move the bar.

## What is being decided

Whether a second inference runtime — llama.cpp through the `llama-cpp-2`
crate, statically linked, with its Metal backend on macOS — makes the two
#133 candidates a GPU can help (`arctic-embed-m-v2.0`, `embeddinggemma-300m`)
fast enough on Apple silicon to become #138 inputs. This reopens the
2026-09-03 one-runtime rule, by the user's decision on 2026-09-06, because
#139 showed ONNX Runtime has no macOS acceleration path: the CoreML
execution provider was 1.6–2.3× slower than the CPU on every shape and hung
on the document shape (`docs/eval/coreml-ep.md`).

The measurement is the same shape as #133 and #139: behind a feature, against
a bar written first, dropped if it misses. ONNX Runtime on the CPU stays what
ships until something clears the bar. `bge-small-en-v1.5` is the control row:
it shows whether a 33M-parameter model even moves on Metal, which is a result,
not a blocker.

## Why llama.cpp

Ranked against candle (pure Rust, Metal slower, no EmbeddingGemma), MLX via
`mlx-rs` (Apple-only, thin bindings) and ORT static-shape buckets (cures the
hang, not the 1.85× single-claim loss): `llama-cpp-2` 0.1.156 (2026-09-02)
vendors llama.cpp, links statically, has `metal` on macOS and is CPU-only by
default on Linux, and exposes the embeddings path (`with_embeddings(true)`,
`with_pooling_type(Mean|Cls|Last)`, `examples/embeddings`). GGUF exports
exist for every model in play.

## The bar

Per model, on Apple silicon, F16 and Q8_0 GGUF each, against that model's ORT
CPU rows in `docs/eval/embed-models/latency.json`:

1. **Speed or power.** p50 for one 3-sentence claim on llama.cpp/Metal ≤ 0.7×
   the same model's ORT CPU row (arctic 17.0 ms → ≤ 11.9 ms; Gemma 72.6 ms →
   ≤ 50.8 ms), **or** package energy per claim (`powermetrics`, package power
   over a 60-second loop) ≤ 0.3× the ORT CPU run's. The 16-claim and 60-chunk
   document shapes are recorded and must not regress past 1.0×.
2. **Drift.** Cosine ≥ 0.999 between the llama.cpp vector and the ORT vector
   of the same model on every text of the eval fixture (all passages and
   queries), the `coreml_vectors_match_cpu` check generalised to a runtime
   pair. Q8_0 may miss this; then F16 carries the row and Q8_0 is noted.
3. **Portability.** The Linux CPU build and the default-feature build are
   unchanged; CI builds the feature on the macOS runner without running a
   model (#119 stays parked).

A model that clears 1 and 2 becomes a #138 input at its measured speed. None
clearing → measured-and-dropped; the feature stays as a measurement surface
like `candidates` and `coreml`.

## Known risks

- **arctic-embed-m-v2.0 is a gte "NewModel" (RoPE) architecture.** llama.cpp
  support for it as an embedder is unconfirmed; the first step is loading an
  existing GGUF or a `convert_hf_to_gguf.py` run. No support → arctic drops
  out and the issue measures Gemma alone.
- **EmbeddingGemma accuracy bug** in llama.cpp (ggml-org/llama.cpp#19040):
  bar item 2 is the detector. A miss there is llama.cpp's, not ours, and the
  row says so.
- **Metal dispatch overhead on small models.** No batch-of-1 embedding
  benchmark exists for llama.cpp on Apple silicon; the bge control row shows
  it.
- **Build.** cmake + clang/libclang in CI; first compile takes minutes.
  Static link keeps the single binary; the release `dist` profile is untouched
  until the bar is read.
- **Tokenisation and prefixes** must match the ORT path exactly (`passage: ` /
  `query: `, Gemma's `title: none | text: `), or bar item 2 fails for the
  wrong reason.

## Method

- The GGUF candidates are four more `AGMEM_CANDIDATE` ids —
  `bge-small-en-v1.5-gguf-f16|q8_0` and `embeddinggemma-300m-gguf-f16|q8_0` —
  and `AGMEM_ACCELERATOR=cpu|metal` says where llama.cpp runs them (`metal`
  offloads every layer; `cpu` none). `scripts/embed-candidates-fetch.nu`
  fetches the files.
- `cargo test -p agmem-embed --features llama-metal --release --test
  candidates -- --ignored --nocapture latency` per id and accelerator; rows go
  to `docs/eval/embed-models/latency.json`, one per (model, accelerator,
  shape), the accelerator column being what the backend reports; a re-run
  replaces. The ORT fp32 `cpu` rows are the denominators of bar item 1.
- `cargo test -p agmem-embed --features llama-metal --release --test fastembed
  -- --ignored --nocapture llama_vectors_match_ort` is bar item 2: the GGUF
  candidate against its fp32 ORT twin on the CPU, over every fixture text.
- Energy: `sudo powermetrics --samplers cpu_power,gpu_power -i 1000 -n 60`
  alongside a 60-second embed loop, once per runtime; by hand, since it needs
  root.

## Results

Run on 2026-09-06 on an **Apple M1 Pro** (macOS 27), `--release`,
llama-cpp-2 0.1.156 (llama.cpp e79e4bf, Metal backend, every layer
offloaded under `metal`, none under `cpu`, all cores as threads), against
ORT 1.28.0 / fastembed 6.0.2 on the CPU. Rows in
`docs/eval/embed-models/latency.json`.

### Architecture support

Checked on 2026-09-06 with Homebrew llama.cpp build 10809 (commit 5266f24)
and `llama-embedding -ngl 99`:

- **arctic-embed-m-v2.0: out.** The only GGUF on the hub
  (`chux0519/snowflake-arctic-embed-m-v2.0-gguf-embeddings-cpp`) fails with
  `unknown model architecture: 'GteModel'`, and upstream
  `convert_hf_to_gguf.py` has no class for the gte "NewModel" family (its
  `bert.py` covers Bert, DistilBert, Roberta, NomicBert, NeoBert, EuroBert,
  XLMRoberta, JinaBertV2, ModernBert). The one gte-multilingual GGUF that
  exists needs the `llama-box` fork. The issue measures Gemma alone, with
  bge as the control.
- **embeddinggemma-300m: loads.** `ggml-org/embeddinggemma-300M-GGUF`
  (Q8_0) has architecture `gemma-embedding`, carries the Sentence-Transformers
  `dense_2`/`dense_3` projections and defaults to mean pooling, which is the
  fix ggml-org/llama.cpp#19040 closed on (a conversion without
  `--sentence-transformers-dense-modules` plus CLS pooling; not a code bug,
  and llama-cpp-2 0.1.156 pins commit e79e4bf of 2026-08-13 regardless). F16
  is made from `unsloth/embeddinggemma-300m-GGUF`'s F32 with `llama-quantize`.
- **bge-small-en-v1.5: loads.** `CompendiumLabs/bge-small-en-v1.5-gguf`,
  F16 and Q8_0, `bert` architecture, CLS pooling.

### Bar item 2 — drift

Over the 90 fixture texts (all passages and queries), GGUF on llama.cpp vs
the same model's fp32 ORT export on the CPU:

| model | accelerator | min cosine | mean | under 0.999 |
|---|---|---|---|---|
| bge-small-en-v1.5-gguf-f16 | cpu | 0.999997 | 1.000000 | 0 |
| bge-small-en-v1.5-gguf-f16 | metal | 0.999998 | 1.000000 | 0 |
| bge-small-en-v1.5-gguf-q8_0 | cpu | 0.999742 | 0.999824 | 0 |
| bge-small-en-v1.5-gguf-q8_0 | metal | 0.999910 | 0.999933 | 0 |
| embeddinggemma-300m-gguf-q8_0 | cpu | 0.999563 | 0.999688 | 0 |
| embeddinggemma-300m-gguf-q8_0 | metal | 0.999818 | 0.999886 | 0 |
| embeddinggemma-300m-gguf-f16 | cpu | −0.095904 | −0.006855 | 90 |
| embeddinggemma-300m-gguf-f16 | metal | −0.096031 | −0.006855 | 90 |

**Pass** for the control on both quantisations and for Gemma Q8_0, on both
accelerators. The control passing shows llama.cpp's tokeniser, pooling and
the L2 normalisation in `src/llama.rs` reproduce the ORT path, so the Gemma
F16 miss is the file's: `unsloth/embeddinggemma-300m-GGUF`'s F32 (and the
F16 quantised from it) carries no `dense_2`/`dense_3` tensors — it is the
conversion mistake ggml-org/llama.cpp#19040 was closed on — so its vectors
live in the pre-projection space and are unrelated to the ORT ones. No
published F16 EmbeddingGemma GGUF has the layers; making one needs
`convert_hf_to_gguf.py --sentence-transformers-dense-modules --outtype f16`
on the gated HF checkpoint. **Q8_0 carries the Gemma row**, as the bar
allowed for the other direction.

### Bar item 1 — speed

p50 in ms, warm, 20 samples; the ratio is against the same model's ORT CPU
row (`bge-small-en-v1.5` fp32: 11.9 / 136.0 / 4850; Gemma's denominator is
the `-q` row from #133: 72.6 / 962 / 34 000, the only Gemma ORT row with all
three shapes).

| model | accelerator | claim | ×ORT | claims-16 | ×ORT | document-60 | ×ORT |
|---|---|---|---|---|---|---|---|
| bge-small-en-v1.5-gguf-f16 | metal | 4.6 | **0.39** | 30.0 | 0.22 | 738 | 0.15 |
| bge-small-en-v1.5-gguf-q8_0 | metal | 4.5 | **0.38** | 31.0 | 0.23 | 761 | 0.16 |
| bge-small-en-v1.5-gguf-q8_0 | cpu | 5.2 | 0.44 | 101.0 | 0.74 | 2668 | 0.55 |
| bge-small-en-v1.5-gguf-f16 | cpu | 46.7 | 3.9 | 577.6 | 4.2 | 19 286 | 4.0 |
| embeddinggemma-300m-gguf-q8_0 | metal | 11.4 | **0.16** | 114.4 | 0.12 | 2338 | 0.07 |
| embeddinggemma-300m-gguf-q8_0 | cpu | 23.4 | 0.32 | 459.7 | 0.48 | 9763 | 0.29 |
| embeddinggemma-300m-gguf-f16 | metal | 10.6 | (0.15) | 108.4 | (0.11) | 2235 | (0.07) |
| embeddinggemma-300m-gguf-f16 | cpu | 99.0 | (1.4) | 354.0 | (0.37) | 7150 | (0.21) |

**Pass** on every Metal row and, unexpectedly, on the llama.cpp CPU Q8_0
rows too. The Gemma F16 rows are in parentheses: the file lacks two small
dense matmuls, so they indicate but do not count. Load times are 90–600 ms
against fastembed's 1–3 s. Neither shape regresses past 1.0× on Metal.

Read across: EmbeddingGemma-300M Q8_0 on llama.cpp/Metal embeds one claim
in **11.4 ms** — under bge-small's shipped ORT number (15.7 ms int8, 11.9 ms
fp32) — and a 60-chunk document in 2.3 s against 34 s. The 300M model that
failed #133 on latency alone is now the fastest thing measured on this
machine. bge-small itself gains 2.6× on Metal (4.6 ms).

Two incidental findings: llama.cpp's CPU path is bad at F16 on this chip
(bge F16 on cpu is 4× slower than ORT, Q8_0 is 2× faster), so a CPU
fallback for the runtime would be Q8_0; and Metal's per-call floor is
~4.5 ms regardless of model size, so bge gains less than Gemma.

Energy (`powermetrics`, root) is not measured: the speed half of bar item 1
is cleared, so it does not decide anything.

### Verdict

**EmbeddingGemma-300M Q8_0 on llama.cpp/Metal clears the bar** — items 1
and 2 on Apple silicon; item 3 (Linux CPU and default-feature builds
untouched, macOS CI clippy on `llama-metal`) holds by construction. It
becomes a #138 input at 11.4 ms per claim, 114 ms per 16 claims and 2.3 s
per 60-chunk document. On Linux the runtime would run Gemma Q8_0 on the CPU
at 23 ms per claim (0.32× the ORT -q row) — slower than bge-small's 5.2 ms
Q8_0 but faster than the 60 ms #133 bar.

Open, for the user: #133's arctic-versus-Gemma quality question is now
answered by availability (arctic cannot run here), so the switch is Gemma
or nothing; and shipping llama.cpp means a cmake-and-clang build in the
release pipeline and a second runtime in the binary, which the 2026-09-03
one-runtime rule was written to avoid. The feature stays off by default
until #138 decides.
