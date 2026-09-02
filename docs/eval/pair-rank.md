# Ranking contradictions without a model (issue #53)

Written before any number was computed. The bar below is the decision; the
results section is filled in afterwards and does not move the bar.

## What is being decided

`consolidate` returns a `contradictions` list: pairs of live claims that share
an entity and sit in the `[0.75, 0.90)` cosine band, sorted by cosine,
capped at 20. On the dogfood store the list is full of paraphrase noise and
holds 0 real disagreements (#54); the store's own corrected pairs score in
the same cosine range as the noise. NLI was the one model-based lever and
it anti-separates (AUC 0.22–0.31, `nli-gate.md`, #84). Relevance rerankers
are out for the same reason: a contradiction is *more* relevant to its
partner, not less.

What is left is the shape of a **factual revision under high overlap**: two
statements of the same fact where a number, date, version or identifier
changed, from a rarely-named subject, written at different times. The
literature agrees that this is the signal and that it is cheap:
VitaminC's edit-distance baseline reaches AUC 71.3 on flagging factual
revisions; numeric mismatch is 29 % and negation 17.6 % of real
contradictions in de Marneffe 2008; Weissman 2015 finds factual drift at
Jaccard ≥ 0.9; Yang 2017's "fact update" edit class is defined the same way.
None of that needs a model, and the store already holds labels: every
superseded row and its successor is a real disagreement, and the shipped
top-20 list is a labelled negative set.

## The bar

The probe computes a **blend** of model-free features and reports it on two
labelled sets:

- **T54**: corrected pairs closed by the #54 instant (22) against the
  contradictions list as of that instant (20). Same sets `nli-gate.md`
  scored, so the numbers are comparable.
- **Current**: every corrected pair in the dump (58 at the last count)
  against the current list (20).

The blend ships only if **all** of these hold:

1. AUC ≥ 0.70 on both sets.
2. Permutation p < 0.05 on both sets (10 000 label shuffles of the blend).
3. Cosine, scored on the same sets as the control, stays ≤ 0.60 — the
   number that says the separation is not something the existing order
   already had.
4. The **acceptance test**, which is the issue's own unpark trigger made
   countable: reorder the live 20-pair list by the blend, hand-label the
   new top 20, and find ≥ 4 real disagreements where cosine's order finds 0.

Anything short of all four closes #53 measured-and-dropped, with this
document as the record, and replaces the trigger with a countable one:
real pairs in the live top 20 stay at 0 across two dumps at least 30 days
apart once the store passes 200 live claims.

## Features

All computed from `content`, `entities` and `created_at` only — the probe
asserts nothing else is read, because `invalid_reason`, `superseded_by`
and `valid_from` are the labels.

| feature | what it measures | expected direction |
|---|---|---|
| entity rarity | the rarest entity the two share, as log(pool / df); a hub entity (carried by ≥ 50 % of the pool, the `HUB_SHARE` rule from `tools/hop.rs`) counts 0 | real pairs share a *specific* subject |
| age gap | log1p of \|Δ created_at\| in days | corrections come later than what they correct; paraphrases arrive in bursts |
| slot change | count of numbers, dates, versions and `#ids` present in one side and not the other, taken only when masked-token Jaccard (slots masked out) ≥ 0.3, else 0 | the fact-update shape |
| cue asymmetry | replacement and negation cues (`no longer`, `instead`, `now`, `turned out`, `not`, `never`, `used to`, `previously`, `rather than`, `superseded`, `stale`) present on exactly one side | corrections announce themselves |
| cosine | the shipped order, as control | must not carry the blend |

The blend is the unweighted mean of per-feature ranks (no fitted weights:
with 22 positives any fit is the noise). Each feature's own AUC is reported
beside it so the blend cannot hide a dead feature or a single live one.

## Guards the probe runs on itself

- **Time-matched negatives.** The age gap may measure the store's write
  history rather than supersession — corrections span the store's life,
  paraphrases were seeded in bursts. The probe re-scores age gap against
  negatives resampled to match the positives' gap distribution; a feature
  that survives that is measuring the pair.
- **DF histogram.** In a single-project space nearly every row carries the
  project entity. The probe reports the share of pairs whose only shared
  entities are hubs; above 80 % the rarity lever is dead in that space and
  the report says so.
- **Slot change alone.** Noise pairs differ in "file lists, counts, dates"
  (#54), which is what this feature counts. If it scores below 0.55 alone it
  is dropped from the blend before the blend is scored.
- **Eviction.** How many of cosine's current top 20 the blend's order evicts,
  so the change in what an agent sees is a number, not an impression.
- **Leakage.** A test asserts the feature functions read only the three
  fields above.

## Inputs

A dump from a *copy* of the store (the daemon holds the live one), as
`nli-gate-probe.py` documents, with three fields added: `created_at`,
`tags`, `writer`.

```
cp -r "~/Library/Application Support/dev.agmem.agmem/agmem.db" /tmp/probe.db
echo 'SELECT record::id(id) AS id, space, kind, content, entities, embedding,
      created_at, valid_from, invalid_at, invalid_reason, superseded_by,
      tags, writer FROM memory;' \
  | surreal sql --endpoint surrealkv:///tmp/probe.db \
      --ns agmem --db main --json --hide-welcome
```

`uv run scripts/pair-rank-probe.py DUMP` prints the report and writes the
reordered live top 20 to `pair-rank-top20.json` for hand labelling.

## Results

Run 2026-09-02 against a copy of the dogfood store: 186 rows, 168 in space
`agmem`, 97 live now and 47 live at the #54 instant. The store had grown
since the doc's counts were written: 23 corrected pairs closed by #54
(nli-gate.md counted 22 — one row's `invalid_at` sits inside the second the
instant falls in) and 78 now, against 20 noise pairs in each list. The band
held 647 candidate pairs at #54 and 3 382 now.

| set | rarity | age gap | slot change | cue asym. | cosine (control) | blend | perm. p |
|---|---|---|---|---|---|---|---|
| T54, 23 vs 20 | 0.543 | 0.817 | 0.703 | 0.490 | **0.943** | 0.870 (age gap + slot change) | 0.0001 |
| current, 78 vs 20 | 0.486 | 0.303 | 0.622 | 0.300 | **0.707** | 0.622 (slot change alone) | 0.011 |

Ungated slot change (no overlap floor) reads 0.276 / 0.193 — backwards, as
#54 predicted: the noise pairs differ in exactly the counts and dates it
counts.

**Verdict: not cleared.** Three of the four bars fail:

1. AUC ≥ 0.70 on both sets — fails on the current set (0.622).
2. Permutation p < 0.05 — passes on both.
3. Cosine control ≤ 0.60 — **fails on both** (0.943, 0.707), and this is the
   finding. The store's corrected pairs are not what an undetected
   disagreement looks like: a correction here is typically a near-duplicate
   with one detail changed, and it sits *above* the paraphrase noise on
   cosine. The shipped order separates these labelled sets already; what it
   cannot do is find a disagreement the store has not yet corrected, and
   that class has no labels. The measurement the bar needed does not exist
   in this store.
4. Hand-labelled acceptance — not run; moot with 1 and 3 failed.

The guards fired as designed:

- **Age gap is write history.** 0.817 at #54 collapses to 0.303 now, and
  against 78 time-matched negatives from the wide band it reads 0.386. The
  corrections that were open at #54 were simply older than the noise seeded
  around it.
- **Rarity is dead in this space.** 96 % of candidate pairs share only hub
  entities; `agmem` sits on 144 of 168 rows, and the next entity down
  (`surrealdb`) on 14.
- **Cue asymmetry** never rose above chance: agmem corrections restate the
  fact rather than announce a change.
- With a single surviving feature the blend evicts 19 of cosine's top 20,
  which is a reorder by slot count, not a ranking.

Per-feature scores: `pair-rank-scores.json` beside the probe's output (not
committed; regenerate with the script).

## Consequences

- #53 closes measured-and-dropped, joining #80, #81 and #84. The
  model-free lever was the last constraint-compatible candidate, and the
  store cannot currently label the thing that lever would have to rank.
- The unpark trigger becomes countable: a dump in which a hand-labelled
  real disagreement sits inside the band but below the cap — crowded out,
  the harm the issue names — or a labelled set that the shipped cosine
  order does not already separate (bar 3 passing), which would mean the
  store has accumulated corrections that are not near-duplicates. Until
  either happens there is nothing to rank against, and the check is cheap:
  re-run the probe on a dump at least 30 days on once the store passes 200
  live claims.
- The entity-normalisation fix (`ctx-flow` versus `context-flow`) stands on
  its own and does not wait on this.

## If it ships

`agmem-core/src/dedup.rs` gains a pure `disagreement_score` with the feature
functions and unit tests; `tools/consolidate.rs` computes entity document
frequency over the pool once, adds a `signals` object to each
`Contradiction` beside the unchanged `similarity`, and sorts by the score.
One protocol test: a rare-entity, time-separated, numeric-conflict pair
outranks a same-burst hub paraphrase. `docs/design.md`'s consolidate
section is updated, and because the `Contradiction` shape changes, the
consolidate scenario pair in `scripts/desc-eval.nu` runs first.

Separately, and regardless of the verdict: `shared_entities` should
normalise entity names (case and `[-_ ]`), since `ctx-flow` and
`context-flow` failing to match is a recall bug, not a ranking one (#54).

## Sources

- VitaminC (Schuster et al. 2021): https://ar5iv.labs.arxiv.org/html/2103.08541
- de Marneffe, Rafferty, Manning 2008, *Finding contradictions in text*: https://aclanthology.org/P08-1118.pdf
- Weissman et al. 2015, factual drift in Wikipedia: https://ar5iv.labs.arxiv.org/html/1406.1143
- Yang et al. 2017, edit intentions incl. "fact update": https://aclanthology.org/D17-1213/
- NevIR reranker results on contrastive pairs: https://arxiv.org/html/2502.13506v2
- Small-n classifier cautions: Beleites 2013, Vabalas 2019, Ojala 2010 (permutation tests)
