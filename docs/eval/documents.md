# Recall precision with documents present (issue #137)

Written before any number was computed. The bar below is the decision; the
results section is filled in afterwards and does not move the bar.

## What is being decided

Since #134 a document — a plan, a review, a research report — is an episode
with a title and a `doc_kind`, chunked at ~1,500 characters and stored
beside the claims distilled from it. `recall` fuses chunk hits into the same
page as claim hits. The corrected-pair work (#53, #54), the abstention floor
(#77) and the occupancy cap (#76) were all measured on a store of short
claims plus a handful of transcripts. A plan is 40–60 chunks of confident
prose that mentions everything the project is about; the question is whether
a store carrying twenty of them makes `recall` return worse pages for the
claims it holds.

The measurement is the existing quality eval run twice: **A**, every
scenario exactly as today, and **B**, the same scenarios with a real corpus
of documents seeded before the scenario's own seeds. The probes, the
labelled-relevant ids and `k` do not change between the runs; only what
else is in the store does.

## The bar

Per scenario, over its probes, with the occupancy cap on (it has no off
switch — key `source`, cap ⌈k/2⌉ ≥ 2, so a single document can already take
at most half a page):

```
ndcg5(A) − ndcg5(B) < 0.02
```

- **nDCG@5** is computed over the page exactly as `recall` returns it: the
  probe's labelled-relevant memory ids carry gain 1, every other hit — chunk
  hits included — gain 0, DCG over the first min(k, 5) hits, ideal DCG from
  min(|relevant|, 5). Scoring the page as returned is the point: a chunk
  that displaces a relevant claim is the cost being measured. The scenario
  value is the mean over its probes, rounded to four decimals like `mrr`.
- The bar is the issue's own number. It is asserted by
  `documents_present_stays_under_the_bar` in `crates/agmem-server/tests/eval.rs`
  against the block recorded below, so a later retrieval change that lets
  documents crowd claims out fails there before anywhere else.
- Reported beside it, not gated: `found`, `returned`, the number of chunk
  hits inside the top 5, and how often the occupancy cap fired.

If the bar fails on any scenario, the fix candidates are measured **one at
a time**, each against A with the fixture unchanged, each a one-line change,
in this order:

1. Documents capped to one hit per page (a second occupancy pass over chunk
   hits, keyed by document, cap 1).
2. Chunk hits weighted 0.8 in the fusion (`rrf × 0.8` on the chunk arm
   before `rank` normalises).
3. `doc_kind: transcript` excluded from default recall. The corpus below
   holds no transcript, so this rung can only be measured on a fixture that
   adds one; if it comes to that, the fixture grows by one transcript and
   the doc says so rather than reporting a zero.

The first rung that brings every scenario back under the bar ships; the
rest are recorded as not needed. If none does, the issue closes
measured-and-adjusted with the best rung and the residual named.

## Inputs

**Scenarios.** The six in `crates/agmem-server/tests/fixtures/eval/scenarios/`
plus one added for this measurement, `agmem-notes.json`, whose claims are
distilled *from* the corpus and whose probes ask about them. Without it the
corpus is off-topic for every probe — the notes are about agmem, the
scenarios about deploys, formatters and a user profile — and chunks compete
with claims only through generic vocabulary, which would let a trivially
small drop pass as a finding. One of its probes is keyword-shaped on
purpose (recorded lesson: probes that are all paraphrase over-reward the
vector arm).

**Corpus.** A redacted copy of the eighteen `legacy-notes` documents from the
dogfood store — the `.claude/notes/` directory the framework retired in
#136, imported as documents on 2026-09-04. Kinds: six reviews, five plans,
two reports, three other, two probes; about 256k characters and ~175
chunks. It is the real case the issue names: confident prose plans and
reviews, written by agents about this project, never distilled (all
eighteen have `cited: 0`). A synthesised corpus would lose exactly that
property.

Provenance and redaction:

- Exported with `agmem doc list --tag legacy-notes` and
  `agmem doc get <id> --raw`; checked in under
  `crates/agmem-server/tests/fixtures/eval/documents/` as one `.md` per
  document beside a `manifest.json` (`title`, `doc_kind`, `tags`, `file`).
- The nineteenth document, `token-analysis-2026-09-03` (97k characters,
  28 % of the corpus, ~65 chunks), is left out: it would add a third of
  the recorded vectors for one document's worth of variety.
- A privacy scan of the export found no e-mail addresses and no API tokens.
  What it did find — the user's home directory in paths and the user's
  name — is replaced with `~` and "the user" before check-in. Nothing else
  is edited; the prose is the prose.

**Vectors.** `regenerate_eval_vectors` in `crates/agmem-embed/tests/fastembed.rs`
chunks each fixture document with the same `agmem_core::chunk::chunk` that
`remember` uses and records a BGE-small vector per chunk, so the
`RecordedEmbedder` replays them bit-stably. The recorded file grows from
~350 KB to ~1.1 MB.

**Seeding.** Documents go in first, then the scenario's seeds, so
`occurred_at` ordering mirrors a real store where the plan predates the
claims distilled from it. Every probe still seeds its own fresh store.

## Results

Run 2026-09-05 on seven scenarios (the six plus `agmem-notes`), 18
documents, 211 recorded chunks. Four measurements: no rung, each of the
first two rungs alone, and — off the ladder, for the record — both
together. Rung 3 could not be measured: the corpus holds no transcript.

**No rung.** The bar fails on four of seven scenarios, and not by a little:

| scenario | ndcg5 without | drop | found | chunk hits in top 5 | cap fired |
|---|---|---|---|---|---|
| agmem-notes | 0.7232 | **0.1250** | 4/4 | 3 | 0 |
| deploy-migration | 0.5308 | **0.2153** | 1/2 | 3 | 0 |
| episode-flood | 0.5706 | 0.0 | 2/2 | 1 | 1 |
| formatter-switch | 0.7103 | **0.2334** | 2/3 | 4 | 0 |
| playbook-flood | — | 0.0 | 0/0 | 0 | 0 |
| session-summary | 1.0 | 0.0 | 4/4 | 0 | 0 |
| user-profile | 0.877 | **0.3334** | 2/3 | 2 | 0 |

Two shapes of failure. On the three off-topic scenarios a labelled
claim leaves the page altogether (`found` 2→1, 3→2, 3→2): a chunk of a
plan about agmem outranks "the user edits in Helix" on a question about
editors, because a 1,500-character slice of confident prose carries some
of every query's vocabulary and its vector sits in the middle of
everything. On the on-topic scenario every claim stays on the page but
chunks take the top slots. The per-source occupancy cap fired on none of
these pages: eighteen documents are eighteen sources, each under quota.

**Rung 1, one verbatim slot per page.** Every lost claim is back
(`found` full on every scenario, at most one chunk in any top 5). Three
scenarios still fail the bar, each by the same mechanism — the one
surviving chunk sits above the scenario's single relevant claim, which is
worth 0.37 of nDCG on a one-relevant probe:

| scenario | drop before | drop after rung 1 | found |
|---|---|---|---|
| agmem-notes | 0.1250 | **0.1250** | 4/4 |
| deploy-migration | 0.2153 | 0.0 | 2/2 |
| formatter-switch | 0.2334 | **0.0898** | 3/3 |
| user-profile | 0.3334 | **0.1230** | 3/3 |

The scorecard without documents does not move.

**Rung 2, chunk `rrf × 0.8`, alone.** Nearly nothing: drops of 0.0923,
0.2334 and 0.3334 remain on the three, and `found` is not recovered. The
reason is structural, not the constant: `rank` min–max normalises `rrf`
across the pool, so a chunk that tops the pool still normalises to 1.0
after any discount that leaves it on top. The rung also moves the
no-document baseline (deploy-migration's own episode chunk falls off its
page, ndcg5 0.5308 → 0.75) and breaks the #134 test that a document and
the claims drawn from it are one source under the cap, because the
discount reorders that page too.

**Rungs 1 and 2 together.** Identical to rung 1 on the three residuals
(0.0923, 0.0898, 0.1230) plus rung 2's baseline shift. Rung 2 adds
nothing where rung 1 leaves a gap.

<!-- eval:documents -->
```json
{
  "documents": 18,
  "scenarios": {
    "agmem-notes": {
      "ndcg5_without": 0.7232,
      "ndcg5_drop": 0.125,
      "found": 4,
      "expected": 4,
      "returned": 20,
      "mrr": 0.4583,
      "ndcg5": 0.5982,
      "chunk_hits_top5": 2,
      "capped_pages": 1
    },
    "deploy-migration": {
      "ndcg5_without": 0.5308,
      "ndcg5_drop": 0.0,
      "found": 2,
      "expected": 2,
      "returned": 10,
      "mrr": 0.375,
      "ndcg5": 0.5308,
      "chunk_hits_top5": 1,
      "capped_pages": 1
    },
    "episode-flood": {
      "ndcg5_without": 0.5706,
      "ndcg5_drop": 0.0,
      "found": 2,
      "expected": 2,
      "returned": 4,
      "mrr": 0.3333,
      "ndcg5": 0.5706,
      "chunk_hits_top5": 1,
      "capped_pages": 1
    },
    "formatter-switch": {
      "ndcg5_without": 0.7103,
      "ndcg5_drop": 0.0898,
      "found": 3,
      "expected": 3,
      "returned": 15,
      "mrr": 0.5,
      "ndcg5": 0.6205,
      "chunk_hits_top5": 1,
      "capped_pages": 1
    },
    "playbook-flood": {
      "ndcg5_without": 0.0,
      "ndcg5_drop": 0.0,
      "found": 0,
      "expected": 0,
      "returned": 0,
      "mrr": 0.0,
      "ndcg5": 0.0,
      "chunk_hits_top5": 0,
      "capped_pages": 0
    },
    "session-summary": {
      "ndcg5_without": 1.0,
      "ndcg5_drop": 0.0,
      "found": 4,
      "expected": 4,
      "returned": 4,
      "mrr": 1.0,
      "ndcg5": 1.0,
      "chunk_hits_top5": 0,
      "capped_pages": 0
    },
    "user-profile": {
      "ndcg5_without": 0.877,
      "ndcg5_drop": 0.123,
      "found": 3,
      "expected": 3,
      "returned": 15,
      "mrr": 0.6667,
      "ndcg5": 0.754,
      "chunk_hits_top5": 1,
      "capped_pages": 1
    }
  }
}
```

**Verdict: measured-and-adjusted.** Rung 1 ships; rungs 2 and 3 do not.
The bar as written is not met on three scenarios, and the residual has one
name: when a query's single relevant claim and a slice of a long document
both match, the slice can still rank first. That is not a crowding
failure — the claim is on the page, at rank 2 — and it is bounded at one
slot by construction. Closing the last 0.09–0.125 would take a rank rule
("verbatim never above the first claim"), which is the `timeline`
scorer's judgement already made once: an episode slice outranking a claim
is a ranking fact with its own column, not one to hide. The recorded block
above is the assertion; the test's bar constant stays at 0.02 and the
three residuals are pinned by name so any change to them shows as a diff.

## Hygiene

Independent of the number above, and shipping either way, `consolidate`
(the shell's `agmem consolidate`; the MCP tool behind `AGMEM_TOOLS=all`)
learns two document reports and no automatic action:

- **Orphans with a grace period.** `orphan_documents` shipped in #134
  listing every document nothing cites, newest first. It now lists only
  those older than 30 days (`ORPHAN_GRACE_DAYS`) and reports each one's
  `age_days`: a document written yesterday has not been ignored, it has
  not been read yet.
- **Churn.** `churning_documents` lists titles that have been rewritten
  more than twice — four or more versions of one title (`CHURN_VERSIONS =
  3`), with the version count, the newest id and the first and latest
  dates. A plan that keeps being rewritten is one to distil once and forget
  the rest of; the report says which, and the agent does it with
  `remember`/`reflect` and `forget --purge`.

`consolidate` does not touch documents.

## Consequences

- `recall` gains a second occupancy pass: verbatim slices, all together,
  hold at most one slot of a page (`occupancy::VERBATIM_CAP`), and the
  `capped` report says when it fired (`verbatim_displaced`,
  `verbatim_cap`). The per-source cap is unchanged. The scorecard in
  `quality.md` did not move under it.
- The `<!-- eval:documents -->` block above is the snapshot
  `documents_present_stays_under_the_bar` asserts. The bar constant is
  0.02; the three scenarios that sit over it are named in the test as the
  known residual, so a change that widens or closes the gap fails there
  first. Re-record only with
  `cargo test -p agmem-server --test eval -- --ignored record_documents`.
- Chunk weighting in the fusion is measured dead in this shape, for the
  normalisation reason above; a future weight would have to act after
  `rank`, not before it.
- The block before any rung is in this file at commit b9aaba7.
- Unmeasured and worth knowing: the corpus is one project's notes and the
  scenarios are mostly about other things. A store where every document
  is on topic for every question (a single-project space with many plans)
  is the `agmem-notes` shape everywhere, where the residual lives.

## Sources

- Issue #137; #134 (documents), #136 (retiring `.claude/notes`), #76
  (occupancy cap), #77 (abstention), #40 (KNN under-return when a predicate
  is pushed into the subquery — why rung 3 filters outside it).
- Järvelin & Kekäläinen 2002, *Cumulated gain-based evaluation of IR
  techniques* — nDCG.
- `docs/eval/quality.md` for the scorecard the A run is, and
  `docs/eval/pair-rank.md` for the shape of this document.
