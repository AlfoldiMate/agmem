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

## Amended 2026-09-02 — two corrections and the decision rule

Written before the 2026-09-16 probe reads a number, so the rule is not fitted
to the result.

### Corrections to the record

- **The monotonic recall count already exists.** `queries::read::REINFORCE`
  does `access_count += 1, last_accessed = time::now()` on every live recall,
  and `MemoryRecord` has carried `access_count` since v0.1.0. The earlier
  sizing ("a `recall_count` incremented by REINFORCE — the one piece of new
  state") was wrong: the *denominator* has been accumulating all along. Only
  the numerator (recalled-then-cited) is missing, and it is derivable at
  read time from `derived_from`.
- **The exposure count is clean in one way and dirty in another.** The
  recall tool reinforces only `Liveness::Live` pages, only rows that survived
  the #76 occupancy cap and the #77 abstention cut, and `context` never
  reinforces — so trimmed rows are not exposures, which is right. But it also
  counts a **filters-only listing** (no query, nothing scored) and an
  **entity-hop promotion** as exposures, and neither is a relevance event.
  When the gate clears, the denominator must be narrowed to *scored*
  exposures. Do **not** narrow `REINFORCE` itself — `strength` and
  `last_accessed` must keep moving on every live recall or a listing stops
  keeping rows alive and the prune horizon shifts. Add a separate
  `search_hits` counter bumped by a second, narrower UPDATE from the facts
  the recall handler already holds (`is_search`, `by_score`, `hopped`).

### Why citations are zero, and what changed

Live sessions run a client-side `/checkpoint` command that, until
2026-09-02, lacked the reflect step the server-side checkpoint prompt carries
(`prompts.rs`, step 4). The description eval put that step at 3/3 cited
versus 0/3 from the `reflect` description alone, so the sessions never
reflected and there was nothing to count. The step now ships in the ritual;
the clock starts 2026-09-02. Re-probe on **2026-09-16** and **2026-10-01**
with `nu scripts/outcome-probe.nu`.

The *recalled-then-* qualifier stays un-backfillable server-side
(`REINFORCE` overwrites `last_accessed`; parallel sessions share one slot).
The planned agmem plugin keeps that log on the client instead: a hook on
each `recall` records the returned ids per session, and the checkpoint
ritual hands the list back to the model to mark what it drew on. That makes
both arms of #86 a by-product of the ritual already measured at 3/3.

### Decision rule for 2026-10-01

1. **Gate unchanged: ≥ 20 live rows carrying `derived_from`.** Under 20 on
   2026-10-01 with the ritual fixed for four weeks closes #86 as "it measured
   `reflect` adoption, not memory worth".
2. **If the gate clears, measure before building.** Cited live rows must
   rank-separate from uncited ones on the free #54-style measurement: AUC
   ≥ 0.65 of `cited` against recall rank on the live store. Below that, the
   citation is not a rank signal here and #86 closes measured-and-dropped
   with this number in the record.
3. **If it separates, the feature is a ratio, not a count.** `cited_by`
   (read-time, one grouped `derived_from CONTAINSANY` plus an index-only
   migration) over `search_hits` (new, scored-only, above), with an evidence
   floor of 10 scored exposures before any weight, a cap like the hop arm,
   and `WEIGHT_CITED = 0.0` report-only first — the #83 precedent. The
   negative arm is dropped as filed; its only live form, a per-writer
   aggregate over rows corrected soon after being recalled, waits for writer
   data and its own gate.
