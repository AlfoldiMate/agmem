# Novelty prior (issue #83)

The write path already computes each gated claim's nearest-live-neighbour
similarity and, until schema v7, threw it away. v7 persists it as
`memory.novelty = clamp(1 − similarity, 0, 1)` — measured once, against the
store as it stood at write time, never recomputed (not even by `--reindex`,
which would answer a different question). That half of #83 stands on its own:
the measurement is un-backfillable, so keeping it is near-free insurance for
any future rank feature, and `signals.novelty` surfaces it on every recall
hit that carries one.

The other half — using it as a rank feature — was tried and is **held at
weight zero**. The precedent is `fusion-sweep.md` and `rerank-probe.md`:
measured-and-dropped is a normal end.

## The arm as tried

`WEIGHT_NOVELTY · (novelty − pool mean)` at 0.05, a bonus term outside the
unit sum (the `WEIGHT_TEMPORAL` shape), pool-centred because absence is
per-row here — a pre-v7 row, a correction, a chunk — and `unwrap_or(0.0)`
would pin those to the floor forever while centring makes them exactly
neutral. Worst-case swing ±0.03 across the widest measured spread: a tail
reorderer, under one decay-class step.

## Decision rule

The recorded eval baseline (`cargo test -p agmem-server --test eval`),
Pareto or nothing: no scenario's retrieval or context column may fall.

## Result — run 2026-09-01, weight 0.05

- Every retrieval column (`found`, `mrr`, `returned`, abstention): unchanged.
  The predicted null — the fixtures' seeds are hand-written distinct claims,
  so novelty barely spreads within a page.
- `playbook-flood` context: **11/11 → 9/11**. Not noise — a mechanism:

Write-time novelty is highest for the *first* claim on a topic, measured
when the store had not heard of it yet, and lowest for everything that
follows. Inside a same-tag flood the term is therefore **anti-recency**: the
#82 per-tag cap defers the *earliest* three of six `role:reviewer` lessons
by its most-recent-first tie-break, and the novelty term dragged exactly
those earliest lessons back into the briefing. First-mover bias is not a
prior on usefulness; playbooks evolve, and the latest lesson superseding an
old habit is the one worth the budget.

So the verdict is worse than the predicted null: zero on what it hoped to
help, negative where `Signals::for_memory` is shared with `context`.

## Standing

`WEIGHT_NOVELTY = 0.0` (`core::scoring`), term wired, signal persisted and
surfaced. Reopen condition: a rank feature that wants novelty must first
show a scenario where it wins — e.g. distinguishing a re-tread from a first
statement of one fact *within a recall page* — and must answer the
anti-recency mechanism above, not just re-run the same sweep. The #86
outcome counters are the more plausible consumer of the stored field than
this arm was.
