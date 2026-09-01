# Fusion-weight sweep (issue #80)

**Verdict: RRF stays.** A fixed convex blend formally beat RRF on the
pre-registered aggregate rule, and the win is deliberately not shipped —
the grounds are below, recorded so the call is cheap to revisit or
overrule.

## Method

One scorecard run (`tests/eval.rs`, `RecordedEmbedder`, the committed
fixtures) per fusion weight α ∈ {0.0, 0.1, …, 1.0}, against the RRF
baseline in `docs/eval/quality.md`. `Some(α)` replaced the engine's
`search::rrf` order with

```
blend = α · norm(bm25_sum)  +  (1 − α) · max(cosine, 0)
```

per candidate — `norm` min–maxed per table's fulltext arm, an arm that did
not return the row contributing 0 — then affine-mapped onto the pool's own
RRF range, so the 0.6/0.25/0.15 rescore (whose min–max is invariant under
the map), the occupancy cap, the hop's additive `0.5/(60+rank)` vote and
the abstention floor all saw the same currency at every α. The candidate
*set* is the fused pool either way; only the order varied.

The instrumentation (`Search.fusion: Option<f64>`, `Candidate.text_score`,
`Harness::start_fused`, the `sweep_fusion_weights` ignored test) lives in
this PR's history at commit `847297b` and is reverted on main: it was
measurement scaffolding, and `None` — the only production value — is the
path that existed before it.

## Decision rule, fixed before the numbers

Hard gates: `found` 10/10, `staleness.stale_hits` 0 everywhere,
`abstention.false_abstentions` 0, timeline no worse than baseline,
`context` passed = total. Objective: ΣMRR over the four scenarios (RRF
baseline **2.1527**), tie-broken by lower Σ`returned`. A win requires
ΣMRR ≥ +0.10 that also holds at both neighbouring α.

## Results (run 2026-09-01)

| α (text weight) | ΣMRR | Σreturned | gates |
|---|---|---|---|
| RRF (baseline) | 2.1527 | 40 | pass |
| 0.0 (pure cosine) | **2.6666** | 40 | pass, timeline +1 |
| 0.1 – 0.5 | 2.2361 | 36–42 | pass |
| 0.6 – 0.9 | **2.4028** | 36 | pass, timeline +1 |
| 1.0 (pure text) | 1.6388 | 32 | **found 7/10 — fails** |

Per-scenario at the α 0.6–0.9 plateau (baseline in parentheses):
deploy-migration mrr 0.625 (0.375), episode-flood 0.5 (0.3333),
formatter-switch 0.6667 (0.6111), **user-profile 0.6111 (0.8333 — a
regression)**. A pre-registered variant re-check — shared-max text
normalisation instead of min–max, so an arm's weakest row is not zeroed —
reproduced every total exactly, so the plateau is not a normalisation
artifact.

## Why the formal win is not shipped

1. **It is not Pareto.** The plateau trades user-profile's mrr down by
   0.22 for gains elsewhere. An aggregate objective over nine probes hides
   a regression in the scenario closest to real personal-memory use.
2. **The fixtures are anti-lexical by design.** The probes were written to
   share as few words as possible with their targets (formatter-switch
   says so verbatim) so the vector arm has to do the work — which is
   exactly the query distribution on which de-weighting BM25 must look
   good. Keyword-shaped queries, the text arm's whole case, are
   structurally under-represented; the BM25-only deployment mode is not
   represented at all.
3. **The plateau is one ordering, not four confirmations.** Every column
   is bit-identical across α 0.6–0.9: with pools this small, orderings
   flip only at discrete α thresholds, so "holds at neighbouring α" —
   the rule's overfit guard — degenerates into a single data point. The
   same degeneracy makes pure cosine (α = 0.0, the best ΣMRR of all at
   2.6666, Pareto-≥ baseline on every scenario) fail the rule as a
   one-α spike. A rule that rejects the best point and accepts a worse
   plateau is measuring its own resolution limit.

Which is the honest summary: **this harness lacks the resolution to
decide a fusion change.** Nine probes with a stated lexical bias can rank
RRF against a blend, but not trustworthily enough to move production
retrieval — the issue's own default ("keep RRF unless beaten") is kept on
the spirit rather than the letter.

## What would reopen this

- Probe growth: a set of keyword-shaped queries (exact identifiers, error
  strings, names) where BM25 must carry the answer, alongside the
  existing semantic probes, so the sweep's objective stops being
  structurally pro-cosine. Then re-run: the scaffold is one revert away.
- The α = 0.0 signal — cosine-only ordering beat RRF everywhere it was
  allowed to compete — is worth carrying into #81 (rerank): if a
  cross-encoder replaces the ordering anyway, fusion weights stop
  mattering above the pool.

Per-query learned routing stays out of scope: the reference measurement
(arXiv:2605.29630) found it recovers nothing, and nothing here contradicts
that.
