# Plan — the three open hard issues (2026-09-02)

Scope: #112, #86, #53. #119 and #120 stay parked by decision and are not in
this plan. Sources: issue threads, PR #111/#118 state, three read-only
architecture passes, two literature passes, the live store probe, daemon.log,
and the Homebrew install receipt.

| # | What is actually left | Gate | Effort | Order |
|---|---|---|---|---|
| 112 | Harden the retire path PR #87 already shipped; the issue's premise is stale | none — a live test on the 0.1.7 upgrade | ~1 day | 1 |
| 86 | Data gate is 0/20 because the checkpoint ritual never calls `reflect`; fix the ritual, then measure | ≥20 live rows carrying `derived_from` | 30 min now, ~1 day when the gate clears | 2 |
| 53 | A model-free revision score, probe-first under a bar written before the numbers | AUC ≥ 0.70 on both labelled sets, plus a hand-labelled live top-20 | ½ day probe, ~1 day to ship | 3 |

Order: #112 first (pure engineering, no waiting), the #86 ritual fix the same
day (it starts a clock), the #53 probe next (a few hours, no model, no spend).

---

## #112 — The upgraded binary defers to the daemon it replaced

### Evidence that changes the issue

- PR #87 (`b641b64`, in v0.1.4, v0.1.5, v0.1.6) made the handshake protocol
  v3: the client sends its release, a mismatch makes the daemon answer
  `retiring:true` and stop accepting, the client waits for the socket to go
  and respawns from its own binary once. Code: `daemon/mod.rs:166-175`,
  `daemon/serve.rs:87-95`, `daemon/client.rs:64-81`; test
  `tests/daemon.rs:262`.
- Homebrew receipt, Cellar mtime, `opt` symlink and brew log all say 0.1.6
  was installed at 2026-09-01T20:23:56Z. daemon.log: the 0.1.3 daemon's last
  `session attached` is 20:12:25; the 4→8 migration ran at 20:24:48. So no
  0.1.6 client ever attached to the 0.1.3 daemon, and the takeover took under
  a minute. The 15 attaches earlier that day were 0.1.3 clients.
- What a 0.1.6 client hits against a 0.1.3 daemon: protocol v2 refuses on
  `version` and closes without an Ack, so the client bails loudly naming
  daemon.log (`client.rs:132-140`). Correct, but it leaves the kill to a
  human — and it is the only case #87 cannot resolve by itself.

### What is left, ranked

1. **Compare, don't test inequality.** `accept` retires on any release
   difference, so an older attacher demotes a newer daemon (two installs on
   PATH ping-pong, cutting live sessions). Retire only when the attacher is
   newer (`semver` is already in Cargo.lock); serve an older one. File:
   `daemon/mod.rs`; extend the refusal table test with an older-release row.
2. **Bound `read_ack`** (`client.rs:122-151` is unbounded) with a ~10 s
   `tokio::time::timeout`; unit test against a socket that accepts and says
   nothing.
3. **Retiring flag.** After the first retire, later attachers must get
   `retiring:true`, not an `ok` on a dying daemon; drain sessions through a
   `JoinSet` + `CancellationToken` with a ~2 s bound. Test: attach A, retire
   via B, assert C sees `retiring:true` and A closes only after the drain.
4. **Lock probe instead of the 200 ms sleep** (`client.rs:156-162`): poll
   `try_lock` on `agmem.lock` inside `start_one` after the spawn lock is
   held; the freed lock is the "old process is gone" signal.
5. **Startup selfcheck.** Extract doctor's store checks into
   `selfcheck(&Db, &dyn Embedder)` over the already-open handles (a second
   connect would be a second writer); run after prune, before bind; log per
   check to daemon.log, non-fatal. Check that `roundtrip` deletes its
   `doctor_probe` rows under a failing DB.
6. **Takeover line.** Hidden `--took-over` passed when a client respawns
   after a retirement; the ready line and the retiring line both say
   "existing sessions need a restart".
7. **Pre-v3 daemon (≤0.1.3), optional.** Cheapest honest form: on the no-Ack
   bail, print the pid from `agmem.lock` and the exact kill command. Full
   form: comm-checked SIGTERM via `rustix` + wait for the lock, once. Only
   machines still on ≤0.1.3 ever hit this.
8. `docs/design.md:689-693` — rewrite route 3a for compare-not-inequality,
   the lock wait, the selfcheck, and the pre-v3 fallback.

### Live test result (2026-09-02T08:36:55Z)

PR #118 merged, brew upgraded 0.1.6 → 0.1.7, then one `agmem context`
from the new binary: the 0.1.6 daemon logged "refused a session from
another release … retiring" with attached=1 (the session that was open),
the 0.1.7 daemon logged "shared store ready" 513 ms later on schema 8,
and the one-shot answered with exit 0. The #87 path works as designed; the
open session lost its memory tools, which is what the #112 takeover line
now says out loud. Hardening shipped as PR #121.

### Decision for the user

Dev builds share `CARGO_PKG_VERSION` with the installed release, so a
`cargo run` on main is served by the brew daemon, silently, and after step 1
stays that way. Options: (a) keep it and document `--no-daemon` for dev
work; (b) suffix `RELEASE` with the git sha on untagged builds, so a dev
build retires the live daemon — and then runs dev migrations on the live
store. Recommend (a).

### Steps

0. Merge PR #118 (v0.1.7, CI green), `brew upgrade agmem`, open a new
   session, read daemon.log: expect a retiring line from the 0.1.6 daemon
   and a ready line from 0.1.7. This is the live test of #87 and costs
   nothing.
1. Re-scope the issue with the evidence above (comment draft at the end).
2. Steps 1–8, one PR, each step with its test. ~1 day.

Risks: pid reuse if step 7's full form is built (gate on socket-existed AND
no-Ack AND comm contains `agmem`; log pid and comm first); drain deadline
too long turns an upgrade into a hang; `RunningService::cancel` semantics
(waits for an in-flight handler, or aborts it) decide whether "finish
in-flight work" is real — check before writing the design.md sentence.

---

## #86 — Outcome-proxy counters

### Where it stands

`nu scripts/outcome-probe.nu` today, against the live store:

| count | value |
|---|---|
| schema_version | 8 |
| memories / live / superseded | 176 / 98 / 76 |
| live_with_citations / cited_rows | 0 / 0 |
| with_writer / with_novelty | 12 / 5 |

The 0.1.6 daemon has run ~11 h: writer and novelty now record, citations do
not. Gate: ≥20 live rows carrying `derived_from`.

### Why citations are zero

The description eval measured `reflect` at 0/3 from its description alone
and 3/3 with a citation when the checkpoint ritual carries the reflect step
(`docs/eval/reflect-isolated` vs `docs/eval/ritual-reflect-note`). That step
is step 4 of the server-side checkpoint prompt
(`crates/agmem-server/src/prompts.rs:145-149`). The ritual sessions here
actually run is context-flow's `/checkpoint`
(`~/Development/context-flow/commands/checkpoint.md`), which mentions
`reflect` only in a table row. So live sessions never reflect, and there is
nothing to count. The blocker is the ritual, not adoption of 0.1.6.

### What the research changes

- arXiv:2604.12007 is "When to Forget: A Memory Governance Primitive"
  (Simsek, Apr 2026). Memory Worth = hits⁺ / (hits⁺ + hits⁻), incremented
  when a memory is in the retrieved set of an episode with a ±1 outcome.
  ρ = 0.89 is Spearman against a **synthetic** utility over 100 memories with
  uniform random retrieval; the paper names a live agent as the missing
  validation. It needs an evidence floor (V_min = 10 in the experiments) and
  a non-zero retrieval probability for every memory. No code.
- Spectron is SurrealDB's agent-memory product: trace-derived features boost
  rows used in successful answers and demote rows with superseded lineage;
  no weights, no effect sizes published.
- Measured judge-free analogues: MemQ (per-memory Q from task reward,
  ε-greedy, +0.8 to +5.7 pp), Memento (+0.6 pt). Every one uses a ratio, not
  a count, and keeps an exploration floor.
- Implicit-feedback cautions (Joachims 2005/2017, Chaney 2018): exposure
  bias and rich-get-richer; mitigations are an exposure-normalised ratio, an
  evidence gate, a capped weight, decay, and an exploration floor.

### Steps

1. **Now, ~30 min, in the context-flow repo.** Port step 4 of
   `prompts.rs` into `commands/checkpoint.md` verbatim (or have `/checkpoint`
   invoke the `/mcp__agmem__checkpoint` prompt). Verbatim keeps the wording
   inside what the eval measured.
2. **Re-probe on 2026-09-16 and 2026-10-01.** If the count is still under 20
   on 2026-10-01 with the ritual fixed, close #86 as "it measured reflect
   adoption", which the counters doc already names as the alternative
   ending.
3. **When the gate clears: decision rule first.** Add to
   `docs/eval/outcome-counters.md`, before any number: cited live rows must
   rank-separate from uncited ones on the free #54-style measurement (a bar
   such as AUC ≥ 0.65 against recall rank).
4. **If cleared, build per the doc's sizing (~½ day + baseline re-record).**
   Read-time cite count through one grouped `derived_from CONTAINSANY` query
   and an index-only migration → `Signals.cited_by`; `WEIGHT_CITED = 0.0`,
   report-only first. Eval `Seed` gains `session`; a new scenario with a
   scripted recall-before-reflect step (new, because seeding recalls
   reinforce and would move existing scenarios).
5. **Weight, when it becomes non-zero.** A ratio, not a count: `cited_by`
   over a monotonic `recall_count` incremented by REINFORCE — the one piece
   of new state the doc allows, since `last_accessed` is overwritten. V_min
   of 10 recalls before any weight; cap like the hop arm; sweep gated by the
   quality baseline the way fusion-sweep was.
6. **Trim the issue.** Drop the negative arm (superseded rows are not live;
   its per-writer form waits for writer data) and mark ρ = 0.89 as synthetic.

Risk to decide before weight > 0: recall is deterministic top-k with no
exploration floor, so a positive weight starves uncited rows of exposure.
The occupancy cap and hop arm add diversity but are not a floor.

---

## #53 — Contradictions rank duplicates above disagreements

### Where it stands

- NLI is measured dead (PR #107, `docs/eval/nli-gate.md`,
  `scripts/nli-gate-probe.py`): AUC 0.22–0.31. Corrections are updates,
  which NLI reads as neutral, while paraphrases differing in surface detail
  are what MNLI calls contradiction. The literature says the same: MNLI-only
  models score ~45–49 % on VitaminC's contrastive revision pairs.
- Relevance rerankers are also out: on NevIR contrastive pairs
  bge-reranker-base scores 32 % at 25 % chance; a contradiction is *more*
  relevant, not less.
- The measured cause (#54): the hub entity makes `shared_entities` vacuous,
  and cosine carries the subject, not the polarity.

### What the research supports

The signal is "factual revision under high overlap": high masked-token
overlap plus a changed number, date, version or identifier. VitaminC's
edit-distance-only baseline reaches AUC 71.3 on flagging factual revisions;
Weissman 2015 finds "factual drift" clusters at Jaccard ≥ 0.9; de Marneffe
2008 puts numeric mismatch at 29 % and negation at 17.6 % of real
contradictions; Yang 2017 defines the "fact update" edit class the same way.
Free labels already in the store: 58 superseded pairs (positive) and the
20-pair live list (negative, 0 real). The extractor exists at
`scripts/nli-gate-probe.py:42-88`; its dump SELECT needs `created_at`,
`tags`, `writer` added.

### Steps

1. **Decision doc first.** `docs/eval/pair-rank.md`, written before any
   number: the blend clears at AUC ≥ 0.70 on both the T54 set (22/20) and
   the current set (58/20), permutation p < 0.05, cosine control ≤ 0.6.
   Acceptance test matching the unpark trigger: reorder the live 20-pair
   list and hand-label it — ≥4 real disagreements in the top 20, against 0
   today.
2. **`scripts/pair-rank-probe.py`**, numpy only, lifting the #84
   extractors verbatim. Features: entity IDF / hub discount over the live
   pool (the `HUB_SHARE` idea in `tools/hop.rs`); |Δcreated_at| and creation
   order; changed-slot count (numbers, dates, versions, `#ids`) gated on
   masked-token Jaccard; negation and replacement-cue asymmetry ("no
   longer", "instead", "now", "turned out"); cosine as control. Report
   per-feature AUC on both sets and the blend; re-test the age feature
   against time-matched negatives; assert features come only from
   `content`, `entities`, `created_at` (leakage guard); report how many of
   cosine's top-20 the new order evicts.
3. **If cleared, ship.** `agmem-core/src/dedup.rs`: pure
   `disagreement_score` with unit tests; `tools/consolidate.rs`: entity DF
   over the pool once, a `signals` object on `Contradiction` beside the
   unchanged `similarity`, sort at `:273` by it; one protocol test (a
   rare-entity, time-separated, numeric-conflict pair outranks a same-burst
   hub paraphrase); `docs/design.md:1012-1034`; and, because the
   `Contradiction` shape changes, run the consolidate scenario pair in
   `scripts/desc-eval.nu` first.
4. **If not cleared, close** #53 measured-and-dropped with the doc, and
   replace the unpark trigger with a countable one: real pairs in the live
   top-20 stay at 0 across two dumps ≥30 days apart once the store passes
   200 live claims.
5. **Separate small PR:** normalise entity names in `shared_entities`
   (case and `[-_ ]`), a recall bug independent of ranking.
6. **Last resort**, only if the numeric-conflict feature shows signal but
   the blend misses the bar: VitaminC ALBERT
   (`tals/albert-base-vitaminc-mnli`, 11.7 M params, REFUTES logit) through
   fastembed's user-defined reranker path, which reads logits column 0, so
   the export must put REFUTES at index 0. Weights licence unverified. Probe
   first, same bar.

Risks: entity IDF has no spread in a single-entity space (the probe reports
the DF histogram; if >80 % of pairs share only max-DF entities the lever is
dead in that space); the age feature may measure the store's write history
rather than supersession (the time-matched re-test tells early); numeric
conflict may fire on the noise pairs, which differ in "file lists, counts,
dates" (drop it below AUC 0.55 alone).

---

## Comment drafts

**#112**
> Re-scoping on evidence. PR #87 (v0.1.4+) already retires a daemon on
> release skew and respawns from the attacher's binary. The Homebrew
> receipt puts the 0.1.6 install at 20:23:56Z; daemon.log shows the 0.1.3
> daemon's last attach at 20:12 and the new daemon migrating 4→8 at 20:24:48,
> so no new client attached silently and the takeover took under a minute.
> What remains: retire only when the attacher is newer; bound the Ack read;
> refuse late attachers on a retiring daemon; wait on the store lock instead
> of sleeping; run the doctor checks at daemon start and log them; say
> "existing sessions need a restart" on takeover; and name the pid to kill
> when a pre-v3 daemon closes without an Ack.

**#86**
> Probe on 2026-09-02: 176 memories, 0 cited rows, 12 with writer, 5 with
> novelty. Citations are zero because the checkpoint ritual sessions
> actually run (context-flow's /checkpoint) lacks the reflect step the
> server prompt carries, which the description eval measured at 3/3 cited
> vs 0/3 without. Fixing the ritual first; re-probing 2026-09-16 and
> 2026-10-01. Also: arXiv:2604.12007's ρ = 0.89 is a synthetic-simulation
> result, and the negative arm is structurally inert, so the issue should
> shrink to the positive arm as a ratio with an evidence floor.

**#53**
> Unparking as a probe, not a feature. NLI is dead (#84); the literature
> says the signal is "revision under high overlap": changed numbers, dates,
> versions or identifiers under high masked-token overlap, plus entity
> rarity and creation order. The store holds 58 labelled positives and 20
> negatives for free. Writing the decision bar in docs/eval/pair-rank.md
> first, then a numpy-only probe; ship only if it clears, close
> measured-and-dropped if not.

## Sources

- When to Forget (Simsek 2026): https://arxiv.org/abs/2604.12007
- Spectron traces: https://surrealdb.com/docs/agent-memory/architecture/traces-and-evolution
- MemQ: https://arxiv.org/html/2605.08374 · Memento: https://arxiv.org/html/2508.16153
- Joachims 2005: https://www.cs.cornell.edu/people/tj/publications/joachims_etal_05a.pdf · Unbiased LTR 2017: https://arxiv.org/abs/1608.04468 · Chaney 2018: https://arxiv.org/abs/1710.11214
- VitaminC: https://ar5iv.labs.arxiv.org/html/2103.08541 · dataset https://huggingface.co/datasets/tals/vitaminc · model https://huggingface.co/tals/albert-base-vitaminc-mnli
- de Marneffe 2008: https://aclanthology.org/P08-1118.pdf · Weissman 2015: https://ar5iv.labs.arxiv.org/html/1406.1143 · Yang 2017: https://aclanthology.org/D17-1213/
- NevIR reranker results: https://arxiv.org/html/2502.13506v2
- fastembed-rs user-defined reranker: https://github.com/Anush008/fastembed-rs/blob/main/src/reranking/impl.rs
- Small-n classifier cautions: Beleites 2013 https://pubmed.ncbi.nlm.nih.gov/23265730/ · Vabalas 2019 https://journals.plos.org/plosone/article?id=10.1371/journal.pone.0224365 · Ojala 2010 https://jmlr.csail.mit.edu/papers/volume11/ojala10a.pdf
- Gradle/Bazel precedent for "client newer than daemon ⇒ restart the daemon" (architect pass, unverified against docs)
