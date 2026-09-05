# Embedding candidates against the store (issue #133)

Written before any number was computed. The bar below is the decision; the
Results section is filled in afterwards and does not move the bar.

## What is being decided

Whether a larger or newer embedding model improves agmem's retrieval and its
override (supersede) behaviour enough to justify the change. Today:
`bge-small-en-v1.5-q` via fastembed 6.0.2 / ort 2.0.0-rc.13, 384 dims, CPU
only. Every threshold in the store — the 0.95 duplicate gate, the 0.75
supersedes floor, the 0.62 abstention floor, the 0.6/0.25/0.15 rescore
weights — was tuned on that model's vector space, so a candidate is judged
on the store's own labelled sets, not on a leaderboard.

## Candidates

| Id | Model | Params | Dims (MRL) | Pooling | Prefixes | Runtime path |
|---|---|---|---|---|---|---|
| `bge-small-en-v1.5-q` | control | 33M | 384 | CLS | `passage: ` / `query: ` | fastembed built-in |
| `embeddinggemma-300m-q` | EmbeddingGemma-300M q8 | 0.3B | 768 → 512/256/128 | in-graph | `title: none \| text: ` / `task: search result \| query: ` | fastembed built-in |
| `arctic-embed-m-v2.0-int8` | snowflake-arctic-embed-m-v2.0 | 0.3B | 768 → 256 | CLS | none / `query: ` | user-defined ONNX |
| `qwen3-embedding-0.6b-int8` | Qwen3-Embedding-0.6B int8 | 0.6B | 1024 → 512/256 | last token | none / `Instruct: … Query: ` | user-defined ONNX, pooled by the harness |

Every candidate runs through the same runtime the daemon uses — ONNX
Runtime on CPU through fastembed — so the vectors measured are the vectors
the store would get. A Python sentence-transformers harness was rejected
for that reason: it measures fp32 torch, a different vector space.

## The bar

A candidate replaces the default only if **all** of these hold, on the live
dogfood store's dump and the eval fixtures:

1. **Separation.** Corrected pairs vs contradictions-list noise, the sets of
   `pair-rank.md` (78 corrected, 20 noise at the last count): cosine AUC ≥
   the current cosine AUC + 0.02 on both sets (current: 0.943 on the #54-world
   set, 0.707 on the current set).
2. **Retrieval.** `recall` nDCG@5 over the eval scenarios ≥ current + 0.03,
   measured with the abstention floor at 0 (pure ranking) so a candidate on
   another cosine scale is not cut by BGE's floor; reported again at the
   candidate's own re-derived floor.
3. **Latency.** p50 embed time for one 3-sentence claim ≤ 60 ms on CPU on the
   M4 Pro (≤ 150 ms on the CI runner class).
4. **Thresholds.** The duplicate gate, supersede floor and abstention floor
   re-derived as a table with the margin of each to the nearest wrong-class
   sample, the shape of #54's analysis; a band that lands inside the noise
   distribution fails the candidate.

A candidate that fails is recorded below with its numbers and the issue
closes as measured-and-dropped, like #53 and #84. A candidate that passes
opens #138 (the migration) — nothing here changes the shipped model.

Caveat on the retrieval column: the eval probe set is anti-lexical by
design (probes share as few words as possible with their targets), so any
nDCG gain here overstates the gain on real queries, where BM25 carries part
of the answer.

## Method

- `crates/agmem-embed/src/candidates.rs` (`--features candidates`) loads each
  candidate behind the `Embedder` trait with its prefixes and pooling.
- `scripts/embed-candidates-fetch.nu` fetches the two user-defined ONNX
  exports and their tokenizer files into the model cache.
- `cargo test -p agmem-embed --features candidates --release --test candidates
  -- --ignored` with `AGMEM_CANDIDATE=<id>`: `embed_dump` re-embeds a store
  dump (`AGMEM_DUMP`) and writes `target/eval/<id>-dump-vectors.json`;
  `latency` times one claim, 16 claims and a 60-chunk document, appending
  rows to `docs/eval/embed-models/latency.json`.
- `AGMEM_CANDIDATE=<id> cargo test -p agmem-embed --features candidates
  --release --test fastembed -- --ignored regenerate_eval_vectors` records the
  eval fixtures with the candidate under `tests/fixtures/eval/candidates/<id>/`
  (gitignored).
- `AGMEM_EVAL_VECTORS_DIR=<that dir> cargo test -p agmem-server --features
  eval-knobs --test eval -- --ignored candidate_scorecard` prints the
  scorecard; `AGMEM_ABSTENTION_FLOOR` sets the floor for the run.
- `uv run scripts/pair-rank-probe.py DUMP --vectors target/eval/<id>-dump-vectors.json`
  reports the candidate's cosine AUC on both labelled sets.
- `uv run scripts/embed-thresholds.py DUMP target/eval/<id>-dump-vectors.json`
  prints the threshold table, with MRL columns by truncation.

Latency rows carry the chip (`sysctl -n machdep.cpu.brand_string`) and an
`accelerator` column so #139's CoreML run appends to the same file.

## Results

Run on 2026-09-05 on an **Apple M1 Pro** (the bar names an M4 Pro; none was
available, so bar 3 is read against the control's own time on this chip as
well as the absolute number), CPU only, `--release`, ORT 2.0.0-rc.13
via fastembed 6.0.2 (Qwen3 through `ort` directly: its export needs
`position_ids` and empty `past_key_values.*` inputs fastembed never feeds).
Dump: 302 rows, 138 corrected pairs, 20 noise pairs, 8 band pairs; the eval
scenarios' 18 labelled-relevant probes and 10 unanswerables. Per-candidate
logs in `docs/eval/embed-models/<id>/` (gitignored); latency rows in
`docs/eval/embed-models/latency.json`.

### The four bar items

| Id | 1. AUC #54 set / current set (bar ≥ 0.961 / 0.671) | 2. nDCG@5 at floor 0 (bar ≥ 0.654) | 3. p50 one claim (bar ≤ 60 ms) | 4. thresholds | Verdict |
|---|---|---|---|---|---|
| `bge-small-en-v1.5-q` (control) | 0.941 / 0.651 | 0.624 | 15.7 ms | shipped | — |
| `embeddinggemma-300m-q` | 0.963 / **0.867** | **0.655** | 72.6 ms | separable below | FAILS 3 (on this chip) |
| `arctic-embed-m-v2.0-int8` | 0.965 / 0.833 | **0.655** | 17.0 ms | abstention floor weak | FAILS 4 (abstention) |
| `qwen3-embedding-0.6b-int8` | 0.963 / 0.767 | 0.615 | ~200 ms (under contention; no clean row) | — | FAILS 2, 3 |

Bar 1 reads: the control's #54-set AUC 0.941 today (0.943 in the issue),
so the bar is 0.961; every candidate clears it. The current-set bar is
0.671 (the issue's 0.707 was on 78 pairs; the store now holds 138), and
every candidate clears that too, EmbeddingGemma by 0.216. Bar 2: the two
768-d candidates tie at 0.6548 with byte-identical scorecards; a verifier
replaced one recording with random unit vectors and the score fell to
0.4166, so the vector arm is live and the tie is genuine agreement on 18
probes — and +0.031 clears the +0.03 bar by the width of a rounding. Qwen3
scores under the control.

### Thresholds (bar 4)

Cosine per labelled class, full width. `corrected` must clear the
supersede floor; `band` (seven contradictions the write gate must let
through, plus one control) must sit under the duplicate gate; `random` is
the unrelated floor.

| Class | bge min / p5 / med / p95 / max | Gemma | arctic | Qwen3 |
|---|---|---|---|---|
| corrected (138) | 0.827 / 0.907 / 0.962 / 0.994 / 0.999 | 0.505 / 0.678 / 0.833 / 0.974 / 0.999 | 0.477 / 0.626 / 0.810 / 0.957 / 0.981 | 0.496 / 0.599 / 0.752 / 0.859 / 0.929 |
| noise (20) | 0.947 / 0.947 / 0.949 / 0.960 / 0.970 | 0.585 / 0.599 / 0.711 / 0.785 / 0.823 | 0.598 / 0.645 / 0.706 / 0.760 / 0.808 | 0.580 / 0.599 / 0.687 / 0.771 / 0.777 |
| random (2000) | 0.697 / 0.776 / 0.858 / 0.915 / 0.970 | 0.089 / 0.214 / 0.387 / 0.558 / 0.743 | 0.083 / 0.219 / 0.372 / 0.542 / 0.706 | 0.289 / 0.380 / 0.485 / 0.611 / 0.733 |
| same-entity (500) | 0.734 / 0.791 / 0.865 / 0.916 / 0.948 | 0.113 / 0.264 / 0.415 / 0.584 / 0.697 | 0.097 / 0.261 / 0.399 / 0.569 / 0.693 | 0.294 / 0.386 / 0.502 / 0.631 / 0.710 |
| band (8) | 0.898 / 0.906 / 0.948 / 0.969 / 0.974 | 0.596 / 0.654 / 0.828 / 0.918 / 0.921 | 0.454 / 0.560 / 0.795 / 0.854 / 0.862 | 0.641 / 0.651 / 0.782 / 0.820 / 0.823 |

What the shipped constants do on each space:

- **Duplicate gate 0.95.** On BGE it blocks 3 of the 8 band contradictions
  (max 0.974) and 67 % of the corrected pairs sit over it — the write gate
  is refusing revisions. On Gemma the band tops out at 0.921 and 11 % of
  corrected pairs exceed 0.95; on arctic 0.862 and 6 %; on Qwen3 0.823 and
  0 %. Every candidate lets every contradiction through the gate.
- **Supersede floor 0.75.** On BGE it admits 100 % of corrected pairs and
  98 % of random ones — it sits inside the unrelated distribution (random
  median 0.858), which is why the contradictions list is paraphrase noise.
  On Gemma it admits 82 % of corrected and 0.00 % of random; the widest
  separable placement is around 0.70 (random p99.9 0.69, corrected p5
  0.68 — a 5 % tail of corrected pairs overlaps the extreme random tail
  on every candidate). On arctic 72 % / 0.00 %; on Qwen3 51 % / 0.00 %.
- **Abstention floor 0.62** (`calibrate_abstention` under the candidate's
  recordings): on Gemma the 10 unanswerables' best similarity runs
  0.044–0.187 and the 14 relevant probes' 0.148–0.749, so a floor of 0.14
  abstains on 6 of 10 with no relevant page lost — the same 6-of-n shape as
  BGE's 0.62 — and 0.19 catches 9 of 10 at the cost of one probe. On
  arctic (unanswerables 0.050–0.224, probes 0.114–0.698) a floor of 0.10
  catches 3 of 10 losing none; catching 8 costs two probes. On the scorecard, Gemma at
  floor 0.14 abstains on 5 of 10 with 0 false abstentions and keeps nDCG@5
  0.6548; arctic at 0.10 fires on 1 of 10 (the floor rule needs the top
  row measured, which drops two of the three low cases) with 0 false and
  0.6548; the control at its shipped 0.62 fires on 6 of 10 with 0 false at
  0.6303.
- **MRL truncation** (thresholds at 512/256/128 in the logs): Gemma's AUC
  goes 0.867 → 0.856 → 0.848 → 0.853, arctic's 0.833 → 0.831 → 0.818 →
  0.791, Qwen3's 0.767 → 0.735 → 0.716 → 0.663. Gemma at 256 d keeps
  0.848, still 0.18 over the bar, at two-thirds of BGE's storage.

### Latency (bar 3)

| Id | one claim p50 / p95 | 16 claims | 60 document chunks |
|---|---|---|---|
| `bge-small-en-v1.5-q` | 15.7 / 16.4 ms | 128 ms | 3.9 s |
| `embeddinggemma-300m-q` | 72.6 / 73.0 ms | 962 ms | 34.0 s |
| `arctic-embed-m-v2.0-int8` | 17.0 / 17.8 ms | 202 ms | 6.1 s |
| `qwen3-embedding-0.6b-int8` | ~193–206 ms (contended run; the clean rerun was cut short) | ~2.5 s | not measured |

One claim, 16 claims and 60 chunks, p50 over 20 warm runs, one process at
a time on an otherwise idle machine. Gemma's batches are the surprise:
dynamic quantisation makes fastembed embed a batch in one ORT call, and
that call scales worse than linearly — a 60-chunk document costs 34 s
against BGE's 3.9 s and arctic's 6.1 s, which `remember` with a document
attached would feel. Arctic sits within 10 % of BGE on the single claim
and within 1.6× on batches.

Qwen3-0.6B at int8 embedded the 318-row dump in 309 s, about one second
per claim; it is out on bar 3 by a factor of three and on bar 2 as well.

### Verdict

**No candidate clears all four bar items, so nothing replaces the default
from this run.**

- **EmbeddingGemma-300M-q** is the quality winner on every axis the store
  cares about — corrected-vs-noise AUC +0.216 over the control on the
  current set, the write gate no longer blocking revisions, a supersede
  floor that finally sits outside the unrelated distribution — and misses
  the latency line: 72.6 ms for one claim on an M1 Pro against a 60 ms bar
  written for an M4 Pro, 4.6× the control on the same chip, and 7–9× the
  control on batches, where its dynamic quantisation forbids sub-batching.
  The single-claim number may pass on the M4 Pro the bar names; the batch
  cost will not. #139 should measure Gemma on the CoreML EP, since an
  accelerator is exactly what a fixed-cost 300M encoder wants.
- **arctic-embed-m-v2.0-int8** passes bars 1, 2 and 3 (17.0 ms for one
  claim, 1.08× the control; 1.6× on batches) and fails bar 4 only on the
  abstention floor: no placement fires on more than 1 of 10 unanswerables
  without losing a relevant page, where BGE fires on 6 and Gemma on 5. Its supersede floor and duplicate gate
  are as clean as Gemma's. If #139 does not rescue Gemma, arctic is the
  fallback to re-measure with more abstain scenarios.
- **Qwen3-Embedding-0.6B int8** is dropped: below the control on retrieval
  and three times over the latency bar on CPU.

The issue closes as measured-and-not-switched; #138 (the migration) stays
open because a switch is now plausible, and #139 measures Gemma and arctic
both on the CoreML EP. Whether arctic's abstention column should block a
model that wins every other column is a call for the issue thread, not
this doc: the column is one the harness already calls weak on the control
(two unanswerables sit inside BGE's relevant band, `abstain.rs`). The re-derived constants for Gemma, if it ships: duplicate gate
0.95 keeps (band max 0.921 — a margin of 0.03, thinner than BGE's but on
the right side), supersede floor 0.70, abstention floor 0.14.
