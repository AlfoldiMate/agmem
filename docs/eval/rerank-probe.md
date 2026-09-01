# Cross-encoder rerank probe (issue #81)

Stage 0 of #81: before any production surface exists — no `Search` field, no
config knob, no `recall.rs` change — the real model
(fastembed's `TextRerank`, jina-reranker-v1-turbo-en, ONNX) scores every
eval-fixture query against every fixture passage, offline, and this document
decides what happens next. The precedent is `fusion-sweep.md`: #80 closed on
its measurement, and "measured and dropped" is an acceptable end for #81 —
the reference prior (ARAGOG) measured reranking flat.

The probe aims at **abstention, not ordering**. Ordering verdicts on these
fixtures are structurally untrustworthy twice over: the probes are
anti-lexical by design (`fusion-sweep.md`), and ΣMRR moves in quanta of
0.167–0.5, so any aggregate threshold is decided by a single probe's rank
flip. Abstention's ground truth — "nothing seeded answers this" — survives
both, has 17 binary units, and carries two *named* failures the single-cosine
floor provably cannot catch (the 0.655–0.691 confusion band recorded in
`quality.md`). A margin signal beyond one cosine is exactly what the #77
calibration said would move `fired` past 6-of-8.

## Decision rule, fixed before any number was read

- **Primary — abstention.** Adopt only if a threshold `t` on
  `sigmoid(logit)` exists with zero false abstentions across all nine probe
  pages **and** `fired ≥ 7/8` (single-cosine baseline: 6/8), with at least
  0.05 margin between the weakest relevant page and the strongest
  unanswerable it catches. A `t` that only reproduces 6/8 closes #81 as
  measured-and-dropped: a 150 MB model must buy more than a 30 MB cosine
  already does.
- **Secondary — ordering.** Report-only at this stage, and Pareto or
  nothing if ever acted on: no scenario's `retrieval.mrr` may fall, no gate
  column may worsen. Even a Pareto win waits on the keyword-shaped probes
  `fusion-sweep.md` names as its reopen condition.
- **Latency.** Warm model load under 3 s (one-off; the daemon amortises
  it), 30-candidate rerank p50 ≤ 150 ms, reported beside the current
  `embed_query` cost so the addition is a fraction of something measured.

## Method

`cargo test -p agmem-embed --features rerank --test rerank -- --ignored
record_rerank_scores --nocapture` walks the committed scenario fixtures,
scores every query (probes, unanswerables, timeline/temporal/context
queries) against every passage of the same scenario (seed contents and
episode texts), commits the full score map to
`crates/agmem-server/tests/fixtures/eval/rerank.json` (so a later
`RecordedReranker` can replay it bit-stably, as `RecordedEmbedder` does for
vectors), and prints the decision table and the latency block this document
records.

## Results

_(recorded after the rule above was committed)_

<!-- rerank:results -->
Run 2026-09-01, `sigmoid` of each page's best logit. Full score map in
`crates/agmem-server/tests/fixtures/eval/rerank.json`.

| page | kind | best |
|---|---|---|
| episode-flood "what happened during the search cluster outage?" | probe | 0.685 |
| user-profile "where is the user based these days?" | probe | 0.432 |
| deploy-migration "…routes external traffic…" | probe | 0.360 |
| user-profile "what animal shares the user's home?" | probe | 0.363 |
| formatter-switch "which tool tidies up our source code layout?" | probe | 0.315 |
| formatter-switch "what does the developer write code in?" | probe | 0.252 |
| deploy-migration "who or what pushes new versions…" | probe | 0.191 |
| **formatter-switch "what are the gym's opening hours…"** | **abstain** | **0.120** |
| user-profile "which herbs grow best…" | abstain | 0.120 |
| user-profile kubernetes ingress | abstain | 0.112 |
| **user-profile "what food restrictions…"** | **probe** | **0.103** |
| **formatter-switch "how are the automated checks executed?"** | **probe** | **0.093** |
| deploy-migration theatre casting | abstain | 0.089 |
| episode-flood chilli cook-off | abstain | 0.060 |
| episode-flood travel expenses | abstain | 0.055 |
| formatter-switch cloud region | abstain | 0.046 |
| deploy-migration conference discount | abstain | 0.038 |

Latency: model load 8.1 s one-off (over the 3 s budget, though the daemon
would amortise it); 30-candidate rerank p50 96–102 ms across batch sizes
8/16/32 (inside the 150 ms budget); `embed_query` beside it is 9 ms.

## Verdict

**Measured and dropped.** No threshold satisfies the primary rule: zero
false abstentions forces `t ≤ 0.093` (the weakest relevant page), and at
that ceiling only 5 of 8 unanswerables fire — *below* the single-cosine
floor's 6 of 8, with no margin anywhere near 0.05. The interleaving is the
finding: the cross-encoder cleanly fixes one of the two cosine confusables
(the chilli cook-off falls from cosine 0.691 to 0.060) but mints two new
ones — the gym query now outscores two genuinely answerable pages whose
probes were written to share no words with their targets. The anti-lexical
probe style that BGE survives on meaning defeats a cross-encoder asked for
literal relevance, which is the ARAGOG-flat prior showing up in this
codebase's own terms.

150 MB of model buys a *different* confusion band here, not a smaller one.
What would reopen #81 is what would reopen #80: keyword-shaped probes, so
that literal-relevance signals are measured on queries that reward them —
plus, per the rule above, an abstention separation the cosine floor cannot
already provide. The score map is committed, so a future `RecordedReranker`
replay needs no model to re-examine any of this.
