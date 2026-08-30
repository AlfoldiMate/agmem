# Memory quality — recorded baseline

The eval harness (`crates/agmem-server/tests/eval.rs`, issue #32) replays
scripted sessions through the real MCP surface and scores what the tools
return. It runs as part of `cargo test --workspace`: no feature flag, no
network, no model download — the embeddings are real BGE-small vectors
recorded once and committed (`tests/fixtures/eval/vectors.json`), so the
numbers below are real-model semantics and bit-stable.

This is a workload eval, not a benchmark. `docs/idea.md` §3.2 is the argument
for why LOCOMO-style rank would not transfer; the fixtures here are the kind
of session agmem actually serves — facts, corrections, distractors —
committed under `tests/fixtures/eval/scenarios/`.

## The scorecard

The block below is the baseline **and** the assertion:
`quality_matches_the_recorded_baseline` compares a fresh run against it with
plain equality. The numbers are honest measurements, not targets — the gate
misses it records are what BGE at the 0.95 threshold actually does with
genuine paraphrases.

<!-- eval:scorecard -->
```json
{
  "scenarios": {
    "deploy-migration": {
      "retrieval": {
        "found": 2,
        "expected": 2,
        "mrr": 0.375
      },
      "timeline": {
        "passed": 1,
        "total": 2
      },
      "gate": {
        "correct": 0,
        "total": 0,
        "false_gates": 0,
        "missed": 0,
        "wrong_original": 0
      },
      "context": {
        "passed": 24,
        "total": 24
      }
    },
    "formatter-switch": {
      "retrieval": {
        "found": 3,
        "expected": 3,
        "mrr": 0.5556
      },
      "timeline": {
        "passed": 2,
        "total": 2
      },
      "gate": {
        "correct": 3,
        "total": 3,
        "false_gates": 0,
        "missed": 0,
        "wrong_original": 0
      },
      "context": {
        "passed": 0,
        "total": 0
      }
    },
    "user-profile": {
      "retrieval": {
        "found": 3,
        "expected": 3,
        "mrr": 0.8333
      },
      "timeline": {
        "passed": 1,
        "total": 2
      },
      "gate": {
        "correct": 3,
        "total": 3,
        "false_gates": 0,
        "missed": 0,
        "wrong_original": 0
      },
      "context": {
        "passed": 8,
        "total": 8
      }
    }
  }
}
```

Reading it:

- **retrieval** — recall@k over the labelled probes (`found` of `expected`
  relevant claims returned), and the mean reciprocal rank of each probe's
  first relevant hit.
- **timeline** — supersession checks: the right claim answers live queries,
  the right one answers `as_of` queries, and a closed claim carries
  `invalid_reason` and `superseded_by`.
- **gate** — the duplicate gate against human-labelled ground truth.
  `correct` requires agreeing with the label and naming the right original;
  `false_gates`, `missed` and `wrong_original` split the failures, because a
  gate that never fires and one that always fires can post the same accuracy.
- **context** — the context-block checklist: section order, budget, no claim
  twice, no superseded or verbatim-episode text, every cited id resolves
  through `inspect`, plus each case's own must/must-not entries.

## Re-running

- The whole eval: `cargo test -p agmem-server --test eval`
- After an *intended* scoring or retrieval change:
  `cargo test -p agmem-server --test eval -- --ignored record_baseline`
  rewrites the block above from a fresh run. Read the diff; commit the change
  and the new baseline together, or neither.
- After editing a scenario fixture:
  `cargo test -p agmem-embed --test fastembed -- --ignored regenerate_eval_vectors`
  re-records the committed vectors (needs the real model; downloads ~30 MB on
  first run). An unrecorded string panics the eval by design — a silent zero
  vector would read as a retrieval regression.

## Sensitivity

Two mechanisms answer "would this harness notice a broken scoring change":

- `retrieval_without_vectors_scores_strictly_worse` runs the same probes with
  an embedder that embeds nothing and demands it finds strictly less than the
  recorded vectors do. A change that severs the semantic arm collapses that
  gap in-suite, with no baseline involved.
- The equality assertion above pins everything else. Verified by hand with an
  intentional break — details and the numbers it moved:

<!-- eval:mutation -->
  Setting `WEIGHT_RRF` (`agmem-core/src/scoring.rs`) from 0.6 to 0.0 —
  ranking on retention and importance alone, relevance ignored — failed the
  baseline assertion with seven fields moved (run 2026-08-30):

  | field | baseline | mutated |
  |---|---|---|
  | deploy-migration retrieval found | 2/2 | 0/2 |
  | deploy-migration retrieval mrr | 0.375 | 0.0 |
  | formatter-switch retrieval found | 3/3 | 2/3 |
  | formatter-switch retrieval mrr | 0.5556 | 0.15 |
  | formatter-switch timeline passed | 2/2 | 1/2 |
  | user-profile retrieval found | 3/3 | 2/3 |
  | user-profile retrieval mrr | 0.8333 | 0.1778 |

## What the baseline says about the system

The imperfect columns are findings, not harness debt:

- **Timeline misses are claim-ranking misses.** With `expect_top` judged on
  the first claim hit, what still fails is retrieval putting an adjacent
  claim first: "how do releases go out" ranks the migrations *lesson* above
  the deploy claim, and "where does the user live" ranks the sister-in-
  Manchester distractor above the Lisbon move. The supersession machinery
  itself — live filtering, `as_of` answers, `invalid_reason`/`superseded_by`
  annotations — passes everywhere it is checked.
- **`as_of` does not filter episode slices.** An episode stored today is
  returned, and ranked first, for an `as_of` a year before it happened —
  episodes carry no validity interval. The timeline metric sidesteps it by
  scoring claims; whether that is worth fixing upstream is an open question.
- **The 0.95 gate went 6-for-6 here**, including two genuine BGE paraphrase
  pairs and two same-shape-different-fact claims it correctly let through.
  Small n; the fixtures should grow adversarial cases before this column is
  trusted.
