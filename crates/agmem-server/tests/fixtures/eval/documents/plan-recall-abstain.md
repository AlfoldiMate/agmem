# Plan — recall abstention (issue #77)

## The real constraint

`Ranked::score` is **min–max normalised over the pool** (`agmem-core/src/scoring.rs::rank`):
the best candidate is always `rrf_normalized = 1.0` and the pool's weakest is always `0.0`,
whatever the query. RRF itself is ordinal — `1/(60+rank)` summed over arms — so a row that
placed first in the vector arm scores the same for "the answer" and for "nothing like the
answer". **No absolute relevance signal reaches `recall` today.** The gap cut (Adaptive-k)
therefore trims but can never abstain: rank 1 has nothing above it to gap against, and a
"flat distribution ⇒ abstain" rule fires on every BM25-only pool (`NoopEmbedder`, `dim 0`,
a supported deployment mode and most of `tests/protocol.rs`), where `rrf` is a perfect
`1/(60+rank)` ramp by construction.

So abstention needs one float plumbed: the HNSW arm already computes
`vector::distance::knn() AS d` (`DIST COSINE`, `queries/read.rs::search`) and throws it away
after ordering. Carry it out as cosine similarity — the exact conversion `repo::dedup`
already does for `Neighbour.similarity`, so the write gate (0.95), the related band
(0.75–0.90) and the abstention floor all speak one unit.

## Placement

After `ranked.truncate(k)` and after `hop::reserve_tail`, before the `touched`/`reinforce`
collection, in `tools/recall.rs::run`.

- **Not before `occupancy::apply`** — the cap needs the tail as replacement inventory for
  deferred rows, and `apply` early-returns `None` when `ranked.len() <= k`, so a knee that
  shrinks the pool to `k` silently disables the flood defence #76 exists for.
- **Not before `reserve_tail`** — a trimmed page is no longer full, so `reserve_tail`
  no-ops and the #43 hop row is lost.
- **Before `reinforce`** — a row cut off the page was not recalled and must not be pushed
  back up its decay curve.
- `let considered = ranked.len();` stays where it is (post-`truncate`, pre-knee), so
  `truncated` keeps meaning "the filters select more than this" and `returned_claims`
  (from `touched.len()`) automatically reports the post-knee count.

## Algorithm — `tools/abstain.rs` (new, sibling of `occupancy.rs`)

Two independent mechanisms, one field.

**Trim (the knee).** On the page, best-first, length `n`:
1. `envelope_i = min(rrf_normalized_1..i)` — the page is ordered by `score`, not by
   `rrf_normalized`, so the raw sequence is not monotone and a naive difference can be
   negative. The prefix minimum is monotone by construction and equals the raw value
   whenever the two orders agree, which is the common case.
2. `gap_i = envelope_i − envelope_{i+1}` for `i in 1..n`; `knee = argmax gap`, earliest
   index on ties (deterministic; `f64::total_cmp`).
3. Cut to `knee + 1` rows **only if** `gap[knee] >= MIN_GAP`.

Measured on `rrf_normalized` and not on `score` on purpose: `score` mixes in
`0.25·retention + 0.15·importance`, so a decay-class boundary (pinned vs fast importance
alone is a 0.1125 step) is routinely the largest gap in a page whose retrieval is flat —
the cut would land on decay class, not on relevance.

**Abstain (the floor).** `hits` becomes empty when the top hit's `similarity` is `Some(s)`
with `s < MIN_SIMILARITY`. `None` (BM25-only pool, or a row only the text arm returned)
never abstains — the absence of a vector arm is not evidence of irrelevance.

**Guards**
- Filters-only path (no `query`): neither mechanism runs. Every `rrf` is 0, every
  `rrf_normalized` is 0, and the caller asked for a listing, not a search.
- `n <= 1`: no trim (no gap exists); the floor still applies.
- All-identical scores: max gap is 0, below `MIN_GAP`, no trim.
- Uniform ramp (BM25-only): step is `0.6/(pool−1) ≈ 0.0095`, far below `MIN_GAP`, no trim.
- Hop-reserved rows are **protected from the trim** — pass the same
  `hopped.contains(&memory.id)` predicate `reserve_tail` takes. A hop row is weak by
  design (`HOP_WEIGHT = 0.5`) and is the knee's natural first victim, which would
  re-create the #43 miss. They are *not* protected from abstention: the hop is only
  justified when the primary query found something.
- `as_of` / `include_invalidated` are treated like any other read (uniform rule, fewer
  branches). Worth revisiting only if the timeline metric moves.

**Constants (calibrate, do not guess).** `MIN_SIMILARITY` starting point 0.60,
`MIN_GAP` starting point 0.10. Both are module constants, no env knob — the precedent is
`hop`'s constants and `occupancy::cap`, and the issue implies zero-config. Calibrate with
a temporary `#[ignore]` test that seeds each eval scenario with `RecordedEmbedder` and
`eprintln!`s `(similarity, rrf_normalized, score, content)` for every probe page plus the
new abstain queries (the loop that worked for #76). Pick `MIN_SIMILARITY` strictly between
the weakest *relevant* probe hit and the strongest *abstain-case* top hit; if no such gap
exists, say so and stop — the floor is not calibratable on this fixture set and the issue
needs a bigger one.

## Response shape

```rust
#[serde(skip_serializing_if = "Option::is_none")]
pub cut: Option<Cut>,
```
`Cut { kept: usize, considered: usize, best_similarity: Option<f64>, note: String }`,
`kept: 0` being the abstention. One field for one mechanism; a struct-with-note rather
than a bare string because that is exactly what `Capped` and `Truncated` already are, and
the numbers behind the note are what make it checkable.

When `kept == 0`, `capped` and `truncated` are set to `None`: both describe a page that no
longer exists.

Note wording carries the next move, per the measured rule that steering lands in the
answer and not in the description:
- abstain — "Nothing here matched well enough to answer: the best of {considered}
  candidates was {best_similarity:.2} similar to the query. That is not an empty store —
  it is a page with nothing on it worth acting on. Ask in different words, or drop `query`
  and use `entities`/`tags` to list what is stored."
- trim — "{kept} of {considered} candidates are returned: the rest fell off a marked drop
  in match quality, not off `k`. Raising `k` will not bring them back; a filters-only call
  lists what is there."

**No tool-description change.** The contract an agent needs is "an empty page means nothing
matched, not nothing stored", and that belongs in the answer where it is read at the moment
of use. This keeps `scripts/desc-eval.nu` out of the critical path. If a description change
is ever wanted, `scripts/desc-eval.nu --isolated` runs first, per standing instruction.

## Build sequence

1. `crates/agmem-store/src/queries/read.rs::search` — return the vector arms' distances
   alongside `scored`: `nearest: $vs.map(|$r| { id: record::id($r.id), d: <float> $r.d })`
   and the same for `$vsc`, emitted only when the arm exists. Update the script assertions
   in that file's `mod tests`.
2. `crates/agmem-store/src/repo/read.rs` — `Candidate.similarity: Option<f64>`, filled at
   the construction site (~line 270) by joining the distance lists on id,
   `similarity = (1.0 - d).clamp(0.0, 1.0)` — the same conversion `dedup` uses. `None` at
   the other two sites: `tools/hop.rs:203` (hop-added rows) and
   `tools/recall.rs:330` (filters-only lookup).
3. `crates/agmem-core/src/scoring.rs` — `Signals.similarity: Option<f64>`, carried through
   `rank` into `Ranked` **with zero weight**. Plumbing must not move the baseline; verify
   by running the eval before touching step 4.
4. `crates/agmem-server/src/tools/abstain.rs` (new) + `mod abstain;` in `tools/mod.rs` —
   `MIN_SIMILARITY`, `MIN_GAP`, `struct Verdict { kept, considered, best_similarity }`, and
   `pub(super) fn apply<T>(page: &mut Vec<T>, signals: impl Fn(&T) -> (Option<f64>, f64),
   protected: impl Fn(&T) -> bool) -> Option<Verdict>`. Unit tests here cover every guard:
   flat ramp, single row, identical scores, protected hop row survives a trim, protected
   hop row does not survive abstention, `None` similarity never abstains.
5. `crates/agmem-server/src/tools/recall.rs` — call `abstain::apply` after `reserve_tail`
   and before `touched`; add `Cut` + `RecallResult.cut`; suppress `capped`/`truncated` on
   `kept == 0`; expose `similarity` on `HitSignals` (an absolute match number is what
   `rrf_normalized`'s own doc comment admits it cannot give).
6. `crates/agmem-server/tests/protocol.rs` — three tests with `AngleEmbedder` (exact cosine
   angles) or `KeywordEmbedder`: an irrelevant query abstains and the note names the next
   move; a relevant query does not; a `NoopEmbedder` (BM25-only) page never abstains. Plus
   `cargo insta` re-accept of `snapshots/protocol__list_tools.snap` (output schema gains
   `cut` and `signals.similarity`).
7. Eval — `tests/eval/scenario.rs`: `Scenario.abstain: Vec<AbstainCase { query, k }>`.
   `tests/eval/metrics.rs`: `Retrieval.returned: u32` (precision denominator — without it
   the trim's cost is measured and its benefit is not, and the harness vetoes a correct
   change), and `Abstention { fired, expected, false_abstentions, pages }` scored over the
   abstain cases plus every existing probe page (an empty probe page is a false
   abstention). Add two abstain cases per scenario to
   `tests/fixtures/eval/scenarios/*.json`.
8. Re-record and document — `cargo test -p agmem-embed --test fastembed -- --ignored
   regenerate_eval_vectors` (new query strings; real model, ~30 MB), then
   `cargo test -p agmem-server --test eval -- --ignored record_baseline`, then
   `docs/design.md` §5.3 (a step 6b between `take k` and the `truncated` count) and
   `docs/eval/quality.md` (scorecard prose for the two new columns + a third
   `<!-- eval:mutation -->` entry: `MIN_SIMILARITY = 0.0` and `MIN_GAP = f64::INFINITY`
   re-recorded, diffed, restored).

Steps 1–3 are behaviour-neutral and each leaves the tree building; the first behaviour
change lands at step 5.

## Risks

- **The trim cuts labelled-relevant hits** — `retrieval.found`/`mrr` fall against the
  committed baseline. Tell early: `record_baseline` and read the diff *before* writing any
  docs; `user-profile` (mrr 0.8333, `found` 3/3) is the most exposed.
- **BM25-only mode** (`dim 0`) has no similarity anywhere; if the floor treats `None` as
  zero, most of `tests/protocol.rs` and every BM25-only deployment abstains on everything.
  Tell early: `cargo test -p agmem-server --test protocol` right after step 5.
- **The hop's reserved row is the knee's natural victim**, re-creating #43. The multihop
  gate is not in the suite, so nothing else catches it. Tell early: the protected-row unit
  test in step 4.
- **Empty pages change agent behaviour** — an agent that reads "no hits" as "nothing
  stored" stops asking. Only measurable with `scripts/desc-eval.nu`; the note is the
  mitigation, and a follow-up session-level measurement is the check.
- **#40 (KnnScan under-return) territory** — projecting a second list off `$vs` is not a
  predicate pushed into the scan, so it should be safe, but this engine has under-returned
  here before and only a store written by an earlier process shows it. Tell early:
  `tests/knn_probe.rs` still passes.
- **No escape hatch**: a wrongly-abstaining query hides data with no runtime override. The
  filters-only path is the documented way out; an `AGMEM_ABSTAIN` knob is cheap to add
  later (`AGMEM_POOL`/`AGMEM_MAX_K` are the precedent) and deliberately not added now.

## Unknowns

- The similarity distribution of the eval probes — nothing in the repo says where a
  relevant BGE query/passage pair sits, and BGE-small's unrelated-pair floor is high. The
  calibration test in step 4 is the only way to a defensible `MIN_SIMILARITY`.
- Whether episode chunks should abstain on the same floor as claims — verbatim text
  competes on retrieval alone (`Signals::for_episode_chunk`) and its chunking makes
  query/chunk similarity systematically lower. May need its own constant.
- Whether four scenarios can separate abstain from relevant at all. If they cannot, the
  fixture set needs a fifth scenario built for it rather than a fudged threshold.
