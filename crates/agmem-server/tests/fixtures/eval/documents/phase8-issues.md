## #82 context: cap lessons per tag, and let consolidate flag over-full tags
Priority: medium value. Reflexion's measured result is that a bounded lesson window (≤ 3) beats unbounded accumulation (arXiv:2303.11366) — and agmem has no cap on lessons anywhere. Playbook-style tag conventions (role:<agent>) make unbounded growth the default outcome.

Shape, no LLM: `context` takes top-N lessons per tag; `consolidate` grows a fourth list — tags whose live lesson count exceeds the bound — and the agent merges via `remember(supersedes)`, exactly as with near-duplicates. Harness-gated.

## #83 remember: persist the dup-gate's neighbour distance as a novelty prior
Priority: low value, near-free. The write path already computes each new claim's nearest-neighbour similarity and throws it away. Stored as a novelty score (SAGE-style, arXiv:2605.30711 — measured for write-gating, unproven for ranking), it becomes a free weak rank feature: claims that arrived saying something new against re-treads of what the store held.

Weight it small, like the hop arm; harness-gated; mark experimental.

## #84 consolidate: rank contradiction pairs with a tiny local NLI cross-encoder
Priority: the only constraint-compatible lever found for #53. Cosine cannot separate duplicates from disagreements — real corrected pairs and paraphrase noise share the 0.91–0.99 band (measured at #54) — and the project deliberately has no server-side LLM judge. A tiny NLI cross-encoder (nli-deberta-v3-xsmall class, ONNX via ort) ordering candidate pairs by contradiction-vs-entailment probability keeps the judging with the agent while making the list readable.

Out-of-domain risk is real: gate on the #54 measurement set (real corrected pairs vs paraphrase noise) before shipping; a model that cannot separate those either kills the idea cleanly. Prior art worth reading: Vestige's judge-free contradiction tooling (github.com/samvallad33/vestige). Refs #53.

## #85 remember: a summary kind whose derived_from children expand on demand
Priority: medium value. Checkpoint rituals already produce session summaries; stored as a preferred-when-budgeted kind whose `derived_from` children recall/inspect expand on demand, they buy TiMem-style compression (arXiv:2601.02845 — SOTA at −52% recalled length; the LLM half of the mechanism is the agent's, which agmem already has by design — the server delta is plumbing).

Shape: `kind: summary` (or a reserved tag), preferred by context/recall under budget pressure, expansion one inspect away through the existing derived_from links.

## #86 scoring: outcome-proxy counters as a weak rank feature
Priority: speculative, mark experimental. Per-memory counters — recalled-then-cited-in-`derived_from` (+), recalled-then-superseded-soon (−) — approximate "did this memory actually help" without any judge model. The underlying signal is measured (Memory Worth, arXiv:2604.12007, ρ = 0.89); the proxy mapping is not, which is the experiment. Spectron's trace-derived features are the commercial precedent.

Depends on writer provenance for clean attribution. Weight small, like the hop arm; harness-gated.

## #53 consolidate: contradictions rank duplicates above disagreements
The open half of the #25 band fix: with one shared entity every close pair qualifies for the `contradictions` list, and cosine similarity puts duplicates above genuine disagreements — the pairs an agent should read first sort last.

Nothing measured says how to rank disagreement without an LLM, and the project has none by design.

**Parked on purpose**: unpark when an agent is actually seen acting on the wrong pair. Until then any ranking change is speculation.
