# Outcome-proxy counters — availability gate (issue #86)

#86 proposes per-memory counters approximating "did this memory actually
help" without a judge model: recalled-then-cited-in-`derived_from` (+),
recalled-then-superseded-soon (−), riding in `signals` at a small weight the
quality harness would have to justify. Before any of that is buildable into a
*measurable* win, the store has to hold observations to count. This is that
check, run 2026-09-01 against a snapshot of the live dogfood store.

**Verdict: parked — zero observations, and the blocker is data, not code.**
No schema change, no new columns, no `signals` field until the trigger below
fires.

## Measured

`scripts/outcome-probe.nu` (snapshot copy, counts through `surreal sql`;
costs nothing). Against the live store at
`~/Library/Application Support/dev.agmem.agmem/agmem.db`:

| count | value |
|---|---|
| `schema_version` | 4 |
| memories / live / superseded | 159 / 92 / 65 |
| rows carrying `derived_from` (live or closed) | **0** |
| rows carrying `writer` | **0** |
| rows carrying `novelty` | **0** |

The recorded JSON is `docs/eval/outcome-counts.json`; re-running the probe
overwrites it.

## Why this parks the issue

- **The positive arm has nothing to count.** A citation already lives on the
  citing row (`derived_from`), so the count is derivable at read time with
  zero new state — but the live store holds none, because the daily-driver
  binary is agmem 0.1.3 at schema v4, from before `reflect` citations,
  `writer` (#75) and `novelty` (#83) recorded anything. Any rank feature
  built now would ship with a guaranteed-null measurement, which the #83
  precedent (novelty measured dead, shipped report-only) says is not worth a
  permanent per-hit schema surface when the signal loses nothing by waiting.
- **The negative arm is structurally inert for ranking.** It penalises a row
  that supersession already closed, and `Liveness::Live` excludes closed rows
  from every live page, `context` included. Its only non-inert form is a
  per-writer aggregate down-weighting a bad session's *other, still-live*
  claims — which needs `writer` data that does not exist yet either.
- **The un-backfillable part is known and bounded.** The counts themselves
  are derivable any time from `derived_from` and the dated supersession
  chain. What cannot be reconstructed later is only the *recalled-then-*
  qualifier: `REINFORCE` overwrites `last_accessed` and there is no access
  log. That is the one thing that would justify new state — and only once
  the arms have observations at all.

What #86 "depends on writer provenance" turns out to need is smaller than
assumed: the server has honoured a per-request `_meta["agmem/session"]`
override since #75 (`tools::writer`), so per-seed attribution in the eval
fixtures is a test-harness change, not server plumbing. The harness now has
`call_as`, and the override has its first test
(`a_meta_session_override_is_what_the_writer_records` in
`tests/protocol.rs`).

## Reopen trigger

Re-run `nu scripts/outcome-probe.nu` after the installed binary catches up
to ≥0.1.6 and summaries/reflections accumulate. The counters come off the
shelf when **≥20 live rows carry `derived_from`** in the dogfood store —
enough observations for the #54-style free measurement to say whether cited
rows actually rank-separate from uncited ones. If months of use never get
there, #86 was measuring `reflect` adoption, not memory worth, and closes on
that finding instead.

Sized-out next steps for that day: read-time cite count → `Signals.cited_by`
at `WEIGHT_CITED = 0.0` (one grouped `derived_from CONTAINSANY` repo query
plus an index-only migration, ~half a day and a baseline re-record), and a
`session` field on the eval `Seed` with a scripted recall-before-reflect
step — in a new scenario, since seeding recalls reinforce and would shift an
existing scenario's strengths.
