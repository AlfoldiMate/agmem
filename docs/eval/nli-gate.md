# NLI contradiction-ranking gate (issue #84)

The gate #84 set for itself, run before any production surface exists: does
an nli-deberta-v3-xsmall-class cross-encoder separate the #54 measurement
set — the store's real corrected pairs (genuine disagreements) from the
paraphrase noise that fills `consolidate`'s contradictions list? The issue
fixed the decision rule when it was filed: *"a model that cannot separate
those either kills the idea cleanly."*

**Verdict: killed — the model anti-separates.** Every contradiction-based
score ranks the noise *above* the real corrections (AUC 0.22–0.31, where
0.5 is random and the gate needed meaningfully above it). This is not "no
signal"; it is the signal pointing backwards.

## Method

`scripts/nli-gate-probe.py` (self-contained via `uv run`; Python because the
scoring needs a transformer runtime — the shipped path would have been ONNX
via `ort`, and the same weights produce the same scores, so the gate verdict
transfers). Inputs come from a snapshot copy of the live dogfood store
(2026-09-01, 148 claims, all embedded), dumped with `surreal sql` — the pair
*texts*, since NLI scores text, not vectors:

- **Corrected pairs** (should score high): `invalid_reason = "superseded"`
  rows joined to their `superseded_by` successor. 22 pairs existed as of the
  #54 measurement instant (2026-08-30T19:42:18Z; #54 counted 20 with a
  both-sides-embedded restriction), 58 by 2026-09-01.
- **Noise pairs** (should score low): the #53 list reconstructed per the
  shipped definition — claims live at the measurement instant in space
  `agmem`, sharing a lowercased entity, not supersedes-linked, top-20 by
  cosine. Reconstructed cosines 0.908–0.946 against 0.910–0.954 reported on
  #53; the sets match to within reconstruction drift.
- Model: `cross-encoder/nli-deberta-v3-xsmall` (the class the issue named),
  both pair directions, softmax over contradiction/entailment/neutral,
  truncation at 512 tokens.

## Results

Against the #54-world sets (22 corrected vs 20 noise):

| score | AUC |
|---|---|
| max p(contradiction) | 0.305 |
| mean p(contradiction) | 0.250 |
| min p(contradiction) | 0.232 |
| max p(c) − p(e) | 0.307 |
| mean p(c) − p(e) | 0.250 |
| min p(c) − p(e) | 0.223 |
| −max p(entailment) | 0.282 |
| mean p(neutral) | 0.618 |

Medians: corrected pairs p(contradiction) 0.300, noise 0.974. Ranked by NLI
contradiction probability, a 20-slot list takes 8 real pairs (P@20 = 0.40)
against 10.5 expected from shuffling — and the full current sets
(58 corrected vs top-20 current noise) read the same way (AUC 0.375).
Per-pair scores: `scores.json` beside the probe's output (not committed;
regenerate with the script).

## Why it inverts

The samples make the mechanism plain, and it is not a fixable aggregation
choice:

- A real agmem correction is an **update**, not a negation: "the hook nudges
  for X because the CLI doesn't exist" → "the CLI now exists and the hook
  uses it". Different sentences about different states of the world — NLI
  reads *neutral*.
- A noise pair is two dense, related claims about the same subject differing
  in surface details — file lists, counts, dates. Token-level mismatch is
  exactly what MNLI-trained models score as *contradiction*.

So the one aggregation that beats random — mean p(neutral), 0.618 — is
measuring "these two sentences are about different things", which cosine
distance already provides for free. A 70 MB model that reproduces cosine's
failure mode with extra steps does not ship.

## Consequences

- #84 closes measured-and-dropped, the same end #80 and #81 met.
- #53 (ranking the contradictions list) loses its only
  constraint-compatible candidate lever and stays parked on its trigger
  condition: an agent seen acting on a wrong pair.

## Reopen condition

A model class actually trained on *revision* pairs — "updated versus
outdated statement of the same fact" — rather than MNLI-style sentence
logic. Generic NLI is the wrong task: what agmem needs ranked is
*supersession likelihood*, and this measurement is the evidence those are
different things. Any candidate re-runs this probe and must clear the same
bar, on the same stored pairs.
