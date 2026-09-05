# agmem research review — new literature, mined mechanisms, competitive delta

Date: 2026-08-31. Follows up docs/idea.md (2026-08-28). Scope: late 2025 → Aug 2026.
Constraints every idea is judged against: MCP-only surface (stdio tools + prompts), no
server-side LLM calls ever, local embeddings only (fastembed/ONNX BGE-small 384d or
BM25-only), one embedded SurrealDB store, single binary.

---

## Part 1 — New since the survey

### (a) LLM-free / client-side-judged memory management

- **SAGE — novelty gate for memory writes** ([arXiv:2605.30711](https://arxiv.org/abs/2605.30711)).
  Frames the write decision as novelty detection: a von Mises–Fisher density estimator
  over existing memory embeddings scores each candidate; clearly novel → Add, clearly
  redundant → Noop, only uncertain cases escalate to an LLM merge step (which in agmem's
  world is the calling agent). Measured: best average token-F1 vs Mem0 across 7
  open-weight backbones on LoCoMo; 3.4× add-phase API cost reduction; skips 16–18% of
  LLM calls as a drop-in gate for A-MEM. This is a principled generalisation of agmem's
  cosine-0.95 dup gate: the gate becomes a *density* judgment, not a nearest-neighbour
  threshold, and the "uncertain" band maps exactly onto agmem's existing
  report-duplicates-with-text behaviour.
- **Memory Worth — outcome-conditioned forgetting** ([arXiv:2604.12007](https://arxiv.org/abs/2604.12007)).
  Two counters per memory (co-occurrence with successful vs failed outcomes) estimate
  P(task success | memory retrieved). Explicitly no LLM. Measured: Spearman ρ = 0.89 ±
  0.02 against true utilities after 10k synthetic episodes (static baselines ≈ 0); on
  real embeddings, stale memories fell to 0.17 worth while specialists held 0.77 over 3k
  episodes. The catch for agmem: MCP has no outcome channel. Proxies exist server-side:
  "retrieved and later cited in a `reflect` derived_from" = positive; "retrieved and
  shortly superseded" = negative. See idea #3.
- **Beyond Heuristics: decision-theoretic memory management** ([arXiv:2512.21567](https://arxiv.org/pdf/2512.21567))
  and **Remember the Decision, Not the Description (rate-distortion framing)**
  ([arXiv:2605.10870](https://arxiv.org/pdf/2605.10870)) — theory papers; useful vocabulary
  (store what changes future decisions, not what describes the past), no mechanism agmem
  can lift directly.
- **Mem-α / AgeMem — RL-trained memory policies** ([arXiv:2509.25911](https://arxiv.org/abs/2509.25911)
  et al.) — train the store/retrieve/update/discard policy with RL. Does not fit: needs
  training loops and a policy model. Noted for completeness.
- **Dakera** ([dakera.ai](https://dakera.ai/), [benchmark](https://dakera.ai/benchmark/)) —
  a self-hosted single Rust binary with hybrid BM25+HNSW, built-in embeddings,
  decay-weighted composite ranking (relevance + recency + importance − decay penalty),
  6 decay strategies, and **zero LLM dependency in the retrieval path**. Claims 88.2%
  on full LoCoMo (v0.11.94, June 2026) with single-pass retrieval and no LLM reranking,
  and explicitly disputes Mem0's/Zep's higher numbers as using LLM-assisted protocols.
  This is existence proof that agmem's no-LLM lane can score competitively on the
  standard benchmarks — and the closest architectural sibling found.
- **LightMem / MemReader** (via [awesome-agent-memory](https://github.com/tfatykhov/awesome-agent-memory)) —
  SLM-driven and RL-trained extraction policies; both need a model in the write path,
  out of scope.

### (b) Temporal reasoning and knowledge-update handling

- **Mem0 temporal reasoning** ([blog](https://mem0.ai/blog/introducing-temporal-reasoning-in-mem0)).
  The important part for agmem is the **split**: the write path uses an LLM enrichment
  pass (out of scope), but the **read path is fully algorithmic** — queries are
  classified into seven temporal modes (current_state, historical_range, upcoming,
  duration_state, …) "with no additional LLM call", and temporal fit is applied as an
  **additive reranking signal after semantic retrieval**, never as a hard filter.
  Measured: LongMemEval 94.4% at top_200 (from 90.4%); multi-session +11.2pp; temporal
  category 97.0%; +1 ms median read latency. agmem already has valid_from/invalid_at
  and as_of — what it lacks is the read-side temporal intent handling and a
  changed-since query. In agmem the caller can supply temporal intent explicitly as
  tool parameters (`since`, `until`, `changed_since`), which is *more* honest than
  pattern-classifying the query string.
- **TiMem — temporal-hierarchical memory tree** ([arXiv:2601.02845](https://arxiv.org/html/2601.02845),
  ACL Findings 2026). Consolidates raw observations upward into progressively
  abstracted levels; complexity-aware recall picks the level. SOTA LongMemEval-S
  76.88%, recalled-memory length −52.2% on LoCoMo. Consolidation is LLM-prompted —
  but in agmem's model the *agent* is the consolidator (reflect already exists);
  the server-side residue is just a summary kind with derived_from links and a
  briefing that prefers summaries when budget is tight.
- **Memora benchmark + FAMA metric** ([arXiv:2604.20006](https://arxiv.org/abs/2604.20006)).
  Weeks-to-months conversations; introduces Forgetting-Aware Memory Accuracy, which
  **penalises reliance on obsolete/invalidated memory**. Finding: systems frequently
  reuse invalid memories; memory agents offer only marginal improvements. Directly
  relevant to agmem's supersession chains — and FAMA is a metric the offline eval
  harness could adopt (score a run down when a superseded memory is surfaced live).
- **ChronoMem (version control + semantic rollback)** and **MemTX (transactional belief
  commits, snapshot isolation, staging writes before they become actionable truth)** —
  July 2026 entries in [awesome-agent-memory](https://github.com/tfatykhov/awesome-agent-memory).
  MemTX's staging idea is judge-free and maps to a `status: staged` promotion flow.
- **Cognis** ([arXiv:2604.19771](https://arxiv.org/pdf/2604.19771)) — version chains +
  temporal boosting: 96.2% knowledge updates, 92.5% temporal reasoning on their eval;
  validates explicit version chains (agmem has these) + explicit time-awareness at
  read (agmem lacks this).

### (c) Memory poisoning / injection defenses without a judge

- **Utility Under Attack: the limits of content filtering** ([arXiv:2608.21230](https://arxiv.org/html/2608.21230v1),
  Aug 2026). The most decision-relevant result found. Poisoning 1.2% of a corpus with
  plainly-worded false assertions dropped accuracy 0.850 → 0.300. A four-stage content
  filter that catches prompt injection **rejected 0 of 360 poisoned memories** — "a
  false assertion is indistinguishable from a true one without external grounding."
  Additive provenance weighting has **no usable setting** (a weight strong enough to
  resist poison also suppresses legitimate untrusted evidence; default weighting
  p=0.80 vs no defense). What the authors advocate: **bounded occupancy constraints at
  retrieval** — cap how much of the returned page any single source can occupy. That
  is a rank-time quota, implementable in agmem with zero models, and it doubles as a
  diversity mechanism (one chatty session can no longer flood recall).
- **A Survey on Long-Term Memory Security** ([arXiv:2604.16548](https://arxiv.org/abs/2604.16548)).
  Six lifecycle phases (Write/Store/Retrieve/Execute/Share/Forget-Rollback); key claim:
  security "cannot be retrofitted at retrieval or execution time alone — it must be
  anchored in **storage-time provenance, versioning, and policy-aware retention**."
  Judge-free primitives it catalogues: provenance + versioning, isolation, quotas,
  signed writes, retrieval-time filtering. agmem already has versioning (supersession)
  and spaces (isolation); it does not record *which session/tool call* wrote each
  memory.
- **From Untrusted Input to Trusted Memory** ([arXiv:2606.04329](https://arxiv.org/abs/2606.04329)) —
  six attack classes over four write channels; headline: **agents that write and
  retrieve more aggressively are more exploitable**. A design endorsement of agmem's
  deliberate write gate and agent-mediated writes.
- **MemLineage** ([arXiv:2605.14421](https://arxiv.org/pdf/2605.14421)) — lineage-guided
  enforcement; source-aware retrieval policies that demote or quarantine entries from
  untrusted writer principals. Judge-free; needs provenance fields at write.
- Also seen: MemSecBench, SMSR (certified defense), sleeper-poisoning
  ([arXiv:2605.15338](https://arxiv.org/html/2605.15338v1)) — attack-side; no
  constraint-fitting defense beyond the above.

### (d) Retrieval diversity and abstention

- **Adaptive-k retrieval** ([EMNLP 2025 main 1017](https://aclanthology.org/2025.emnlp-main.1017.pdf),
  [arXiv:2506.08479](https://arxiv.org/pdf/2506.08479)): choose k per query as the
  **largest gap in the sorted similarity distribution** — k = argmax(sᵢ − sᵢ₊₁). No
  tuning, no iteration, single pass; measured competitive with oracle-k baselines.
  Degenerate case = abstention: if even the top score sits below the gap floor /
  the whole distribution is flat and low, return the honest empty page. Successors:
  **Tail-Aware Adaptive-k** ([arXiv:2606.11907](https://arxiv.org/html/2606.11907v1),
  EVT tail validation), **ScoreGate** ([arXiv:2606.14269](https://arxiv.org/html/2606.14269v1)),
  **Cluster-based Adaptive Retrieval** ([arXiv:2511.14769](https://arxiv.org/abs/2511.14769)).
  This family is the constraint-fitting answer to "score floor": absolute floors on
  cosine or fused RRF scores are uncalibrated across queries, but the *within-query
  score distribution shape* is self-calibrating.
- **PrecisionMemBench / structured belief state** ([arXiv:2605.11325](https://arxiv.org/abs/2605.11325)).
  First precision-aware memory benchmark: systems that dump the store get perfect
  recall and mask "severe precision failures" (baselines cluster at precision ≤ 0.22).
  Conclusion verbatim: systems "should architecturally enforce the possibility of
  returning nothing." Strong published support for an abstention mechanism.
- **MMR**: original claim (Carbonell & Goldstein) is from summarisation;
  **ARAGOG** ([arXiv:2404.01037](https://arxiv.org/abs/2404.01037)) measured "MMR and
  Cohere rerank did not exhibit notable advantages over naive RAG." Demand exists in
  agent-memory settings ([OpenClaw feature request #19760](https://github.com/openclaw/openclaw/issues/19760):
  one long session floods results) and OpenSearch shipped built-in
  [MMR vector search](https://docs.opensearch.org/latest/vector-search/specialized-operations/vector-search-mmr/) —
  but the *flooding* complaint is better answered by per-source occupancy caps (see (c))
  than by embedding-space MMR, and agmem's write-time dup gate already removes the
  redundancy MMR exists to fight.
- **TREC 2025 RAG track** ([arXiv:2603.09891](https://arxiv.org/pdf/2603.09891)) and
  abstention-in-RAG work (SURE-RAG, PragAURA) — mostly generation-side abstention;
  retrieval-side signal is the adaptive-k family above.

### (e) Consolidation / forgetting with measured results

- **Hindsight (Vectorize), "The Consolidation Problem"** ([blog, May 2026](https://hindsight.vectorize.io/blog/2026/05/21/agent-memory-consolidation)).
  Four levers: importance (write filter), merge, decay, eviction — with an explicit
  LLM/no-LLM split per lever. No-LLM levers that work: temporal validity windows
  (Zep), recency-based conflict resolution, TTL/LRU eviction. Their comparison:
  Hindsight 94.6% / SuperMemory 81.6% / Zep 71.2% / Mem0 67.6% on LongMemEval (their
  protocol; vendor-run, salt accordingly). Recommendation: consolidate rather than
  delete; evict only for compliance.
- **Control-Plane Placement Shapes Forgetting** ([arXiv:2606.15903](https://arxiv.org/pdf/2606.15903)) —
  13 system configurations + a 1000-case **ForgetEval** suite; finding: production
  failures are predominantly **forgetting failures, not recall failures** (i.e.
  failure to invalidate/update, echoing Memora).
- **Are We Ready For An Agent-Native Memory System?** ([arXiv:2606.24775](https://arxiv.org/abs/2606.24775)) —
  12 systems × 5 benchmarks, module-level ablations. Two findings that matter here:
  **no dominant architecture** (workload-memory alignment decides), and **localized
  maintenance is more cost-efficient than global reorganization** — supports agmem's
  lazy TTL pruning + targeted consolidate over any sleep-time global rewrite.
- **When to Forget** (Memory Worth, see (a)) — the only *measured, LLM-free* forgetting
  signal found that uses outcomes rather than time.
- FSFM ([arXiv:2604.20300](https://arxiv.org/pdf/2604.20300)), FadeMem, FOREVER,
  SleepGate — biologically-flavoured forgetting papers; mechanisms need model calls or
  training; skimmed, nothing liftable beyond what decay classes already do.

### (f) Coding-agent memory shipped in 2026

- **Claude Code**: auto-memory **on by default since v2.1.59 (Feb 2026)** — per-repo
  `~/.claude/projects/<slug>/memory/` with MEMORY.md index + topic files; first 200
  lines / 25KB of MEMORY.md load every session; **agent memory frontmatter** (v2.1.33)
  gives each subagent its own persistent store; background "Auto Dream" extracts
  memories after conversations. ([memoryplugin guide](https://blog.memoryplugin.com/claude-code-memory/),
  [mem0 write-up](https://mem0.ai/blog/how-memory-works-in-claude-code))
- **OpenAI Codex**: native project-scoped memories **launched Apr 16, 2026**; Codex
  summarises prior sessions in the background into `~/.codex/memories/`; no
  cross-machine sync. ([discussion](https://github.com/openai/codex/discussions/12567),
  [mem0](https://mem0.ai/blog/how-memory-works-in-codex-cli))
- **GitHub Copilot Memory**: **on by default for Pro** ([discussion #184415](https://github.com/orgs/community/discussions/184415)) —
  repo-scoped only, agent-judged writes, **28-day automatic expiry** ("re-learns if
  still relevant"), owner review/delete UI, and — notably — stored knowledge is
  **checked against the current codebase before being applied**. A production
  endorsement of (1) TTL-by-default for agent-written memories and (2) verify-before-use,
  both of which agmem embodies (fast decay class; briefing tells the agent to verify
  before acting).
- **Letta / Letta Code**: **MemFS** — git-backed memory filesystem replacing memory
  blocks; reflection and defrag **subagents run in git worktrees** and merge back;
  sleep-time agents share memory blocks. All LLM-driven maintenance — outside agmem's
  constraints, but the *pattern* (maintenance done by agents, not the store) is
  agmem's pattern too. ([docs](https://docs.letta.com/letta-code/memfs),
  [sleep-time](https://docs.letta.com/guides/agents/architectures/sleeptime/))
- **opencode**: plugin ecosystem — [opencode-agent-memory](https://github.com/joshuadavidthomas/opencode-agent-memory)
  (Letta-style self-editable memory blocks injected into the system prompt),
  supermemory and agentmemory plugins. **Amp**: nothing found beyond thread
  handoffs; no native persistent memory located.
- **Cursor / Copilot instructions files**: static `.github/copilot-instructions.md` +
  scoped `*.instructions.md` (glob frontmatter); Cursor community "Memory Bank"
  workflows. Static-file memory remains the norm outside the big three.

### (g) MCP spec / ecosystem changes

- **2025-06-18** (already stable): **structured tool output** (`structuredContent`),
  full JSON Schema 2020-12 for input/output schemas, elicitation, resource links in
  tool results, OAuth hardening. ([forgecode summary](https://forgecode.dev/blog/mcp-spec-updates/))
- **2026-07-28** ([official post](https://blog.modelcontextprotocol.io/posts/2026-07-28/)):
  stateless protocol core (requests self-describing via `_meta`; initialize handshake
  going away), **MRTR** (multi-round-trip requests: server returns
  `resultType: "input_required"` instead of server-initiated elicitation), tasks moved
  to a formal extension, tool-list responses carry **`ttlMs`/`cacheScope`**, and
  **sampling + roots deprecated** (12-month window) — which retroactively validates
  agmem's "no MCP sampling" stance completely. Stdio servers are least affected; the
  items worth adopting are structured output, tool annotations
  (readOnly/destructive/idempotent hints — [basic-memory already ships them](https://github.com/basicmachines-co/basic-memory)),
  and eventually MRTR for confirm-style flows (e.g. "this write matched an existing
  memory at 0.93 — supersede, merge, or store anyway?").

---

## Part 2 — Candidate mechanisms: accept / reject

Each verdict states fit with no-LLM/MCP-only or is rejected.

1. **MMR / diversity re-ranking at recall — REJECT (in embedding-MMR form), ACCEPT the
   occupancy variant.** ARAGOG measured no notable advantage for MMR over naive RAG;
   agmem's 0.95 write gate already removes most redundancy MMR targets. The real,
   observed failure mode (one session/episode flooding recall — OpenClaw #19760) is
   solved by **bounded per-source occupancy** at rank time, which [arXiv:2608.21230](https://arxiv.org/html/2608.21230v1)
   independently advocates as the *poisoning* defense that works where provenance
   weighting fails. One mechanism, two wins, zero models. Fit: pure ranking code.

2. **Write-time importance scoring without an LLM — PARTIAL ACCEPT.** agmem already
   has caller-supplied importance: the decay class *is* it (and it carries 0.15 of the
   rescoring blend). Adding a second numeric importance field would be redundant
   caller burden. The evidence-backed addition is a **novelty prior**: agmem already
   computes neighbour cosines at write for the dup gate — store the resulting novelty
   score (SAGE's density idea, degraded gracefully to distance-to-neighbourhood) as a
   weak rank feature. SAGE's measurements are for write-gating, not ranking, so treat
   as plausible, not proven. Fit: reuses existing embedding work.

3. **Bounded-lessons window — ACCEPT (as briefing/playbook cap + consolidate
   pressure).** Reflexion bounds memory to Ω = 1–3 reflections and this bound is part
   of its measured wins ([arXiv:2303.11366](https://arxiv.org/pdf/2303.11366)); agmem
   currently has no cap on lessons and `role:<agent>` playbooks will accumulate.
   Server-side, judge-free version: cap lessons per tag/topic surfaced in `context`
   (top-N by blended score), and have `consolidate` flag tags whose live-lesson count
   exceeds a threshold so the *agent* merges them. Small-scale evidence, sound
   mechanism, near-zero cost. Fit: ranking + reporting only.

4. **Abstention signal — STRONG ACCEPT.** Fixed score floors are uncalibrated, but
   the **adaptive-k largest-gap** rule (EMNLP 2025) is self-calibrating per query, no
   tuning, single pass — apply it to the fused post-rescore distribution; when the gap
   analysis leaves nothing above the knee, return "nothing relevant" (with truncation
   honesty this becomes "0 results, best rejected candidate scored X"). Publication
   support that this matters: PrecisionMemBench (precision ≤ 0.22 baselines),
   LongMemEval's abstention category. Also shrinks average k, saving caller tokens.
   Fit: pure ranking code; also directly measurable in the existing eval harness.

5. **Session summaries / hierarchical memory — ACCEPT-LITE (ritual, not server
   machinery).** TiMem shows hierarchy wins (SOTA LongMemEval-S, −52% recalled
   length), but its consolidator is an LLM — which in agmem's model is the calling
   agent, and `/checkpoint` + `reflect` with derived_from already are that pipeline.
   The missing server pieces are small: a `summary`/`episode-summary` kind that the
   briefing prefers when budget is tight, and recall that can expand a summary to its
   derived_from children on demand. Evidence measured but for an LLM-side mechanism;
   the server delta is plumbing. Fit: agent does all distillation.

6. **Cross-encoder reranking with a local model — ACCEPT (feature-flagged).**
   Feasible today in-stack: fastembed-rs ships `TextRerank`
   ([docs.rs](https://docs.rs/fastembed/latest/fastembed/)) with
   jina-reranker-v1-turbo-en (~38M params, ONNX — CPU-fine for reranking top-30) and
   bge-reranker-base. Generic evidence for cross-encoder gains is strong (BEIR/NDCG
   literature); memory-specific evidence thin, and ARAGOG found Cohere-rerank flat, so
   gate it behind the eval harness: adopt only if it beats RRF+rescore on agmem's own
   eval. Adds a model download; keep optional. Fit: local ONNX, no API.

7. **BM25+vector weight tuning / learned fusion — ACCEPT tuning, REJECT learned.**
   Industry guidance: RRF is the right zero-config default; a tuned convex combination
   beats it only with an eval set — tuned hybrid hit +7.4% NDCG on WANDS
   ([digitalapplied reference](https://www.digitalapplied.com/blog/hybrid-search-bm25-vector-reranking-reference-2026)).
   agmem *has* the eval harness, so the tuning experiment is nearly free; keep RRF if
   it doesn't lose. Per-query learned routing: **Entity-Collision**
   ([arXiv:2605.29630](https://arxiv.org/abs/2605.29630)) found 11.7pp oracle headroom
   for adaptive vector-weight routing "but no signal we tested recovers it" — don't.
   Fit: offline tuning, constants in the binary.

8. **Time-aware queries ("what changed last week") — ACCEPT.** agmem already stores
   valid_from/invalid_at and as_of; add `since`/`until`/`changed_since` recall
   parameters plus an additive temporal-fit rescore term (never a hard filter — Mem0's
   read-path design, measured +11.2pp multi-session / 97.0% temporal at top_200, and
   its read path is explicitly LLM-free). Caller supplies temporal intent as tool
   args, which is cleaner than query-string classification. Fit: schema already there.

9. **Memory usage feedback loops — ACCEPT (proxy-signal version).** Memory Worth's
   two counters are measured (ρ = 0.89 synthetic; 0.77-vs-0.17 separation on real
   embeddings) and Spectron ships trace-derived boost/demote in production
   (rows useful for similar queries boosted; rows associated with corrections
   demoted — [SurrealDB deep dive](https://surrealdb.com/agent-memory/deep-dive)).
   agmem's honest version without an outcome channel: count (a) recalled-then-cited
   (id appears in a later `reflect.derived_from` or `supersedes`) as positive, (b)
   recalled-then-superseded-soon as negative; blend as a small rank feature. Note
   agmem's recall-strength reinforcement already implements the crudest form
   (retrieval = positive); this adds the *outcome-flavoured* half. Proxy validity is
   unmeasured — run it through the eval harness. Fit: server-side counters only.

10. **Forgetting curves per kind vs per class — REJECT (for now).** No published
    evidence that per-kind curves beat per-class; the interesting alternative is
    **FSRS-6** (Vestige uses it; `rs-fsrs` exists in Rust) — but FSRS's evidence base
    is human spaced-repetition data (Anki-scale), and its inputs (graded reviews)
    don't map cleanly onto recall events. Vibes for the agent domain. Revisit only if
    the eval harness shows retention-curve shape actually moving outcomes.

11. **Graph memory lite (materialised entity co-occurrence edges) — REJECT.**
    Entity-Collision ([arXiv:2605.29630](https://arxiv.org/abs/2605.29630)) shows
    claimed graph/entity lift mostly fails attribution once the BM25 floor is pinned;
    mem0's graph arm needs an LLM for extraction; HippoRAG-style PPR is heavy. agmem's
    string one-hop already captures the cheap 80%. Materialise edges only if the eval
    harness someday shows the string hop saturating.

12. **Dedup below the 0.95 gate (simhash on normalised text) — REJECT, except
    trivial exact-hash.** RETSim ([arXiv:2311.17264](https://arxiv.org/html/2311.17264))
    and practice show embedding cosine dominates simhash/minhash for near-dup
    *ranking*; agmem's 0.75–0.95 related band plus consolidate's near-dup clusters
    already surface sub-gate duplicates for the agent. A normalised exact-text hash
    (lowercase/whitespace-fold) as a free pre-gate is fine; a second LSH index is not
    worth its weight.

13. **(New) Staged writes / provenance quarantine — ACCEPT-LITE.** From MemTX +
    MemLineage + the security survey's storage-time-provenance finding: record writer
    provenance (session id, tool, space) on every memory — cheap now, impossible
    retroactively — and optionally let callers mark a write `staged` so it competes in
    recall only with a visible [staged] marker until a later promote/supersede. No
    judge anywhere; the agent is the promoter. Evidence: provenance *weighting* is
    measured-useless, provenance *recording* + occupancy caps + review is what the
    literature converges on.

14. **(New) FAMA-style penalty in the eval harness — ACCEPT.** Adopt Memora's
    Forgetting-Aware Memory Accuracy: any eval run that surfaces a superseded/invalid
    memory as live gets penalised. Both Memora and ForgetEval say update-failure, not
    recall-failure, is where systems die — agmem's supersession machinery is its moat
    and should be what the harness proves.

---

## Part 3 — Competitive delta (closest comparables, 2026)

- **Mem0 / OpenMemory** ([changelog](https://docs.mem0.ai/changelog),
  [state-of-memory report](https://mem0.ai/blog/state-of-ai-agent-memory-2026)):
  shipped temporal reasoning (94.4% LongMemEval top_200 — but write-path LLM
  enrichment), graph memory via embedded Kuzu, local mode with FastEmbed, memory
  export/import, dashboards. What agmem lacks that has evidence: **read-path temporal
  scoring** (measured, LLM-free — idea #8/#5 above). What it has that agmem shouldn't
  copy: LLM extract-then-reconcile write path (their scores depend on it; Dakera's
  protocol critique applies).
- **Dakera** ([dakera.ai](https://dakera.ai/)): the same architectural bet as agmem
  (single Rust binary, embedded everything, no LLM), plus importance scoring, six decay
  strategies, sessions, and an honestly-protocolled LoCoMo 88.2%. Delta vs agmem:
  composite additive scoring with a recency term, published benchmark numbers as
  marketing. Lesson: publishing agmem's eval-harness numbers under a stated protocol
  is a competitive asset in this lane.
- **mcp-memory-service (doobidoo)** ([repo](https://github.com/doobidoo/mcp-memory-service)):
  v11.10 (Aug 2026) — autonomous consolidation, "conditional temporal decay,
  functional belief derivation", web dashboard, 25+ clients, Cloudflare sync. No
  published measurements found for any of it; the consolidation is server-autonomous
  (agmem deliberately keeps the agent in that loop). Vibes-heavy; no mechanism to lift.
- **basic-memory** ([releases](https://github.com/basicmachines-co/basic-memory/releases)):
  markdown knowledge graph, v0.23 added reranking and Postgres FTS, ships **MCP tool
  annotations** (readOnly/destructive/idempotent hints). The annotations are the one
  thing worth copying immediately — near-zero cost, improves client behaviour.
- **Vestige** ([repo](https://github.com/samvallad33/vestige)): Rust MCP server;
  FSRS-6 decay, prediction-error gating, spreading activation, and a **first-class
  contradictions tool using "trust-weighted local contradiction logic"** (no LLM
  claimed). Worth reading their contradiction code for the open problem — but no
  measurements published, and "trust-weighted" suggests heuristics over provenance +
  recency, i.e. ranking candidates rather than solving entailment. On the open
  problem generally, the only constraint-compatible lever found is a small **ONNX NLI
  cross-encoder** (e.g. nli-deberta-v3-xsmall class, ~70M) run over the 0.91–0.99
  band pairs — not an LLM call, single binary feasible via ort, but NLI models degrade
  out-of-domain and nobody has published memory-domain numbers: plausible, unproven,
  and the honest framing is "rank contradiction candidates better", not "judge them".
- **Spectron (SurrealDB)** ([deep dive](https://surrealdb.com/agent-memory/deep-dive)):
  SurrealDB's own agent-memory tier — 8 fused signals (vector, BM25, graph, keyword
  bridges, doc links, PageRank, geo, **trace-derived features**), tri-temporal clocks,
  contradiction "reconciler" emitting explicit uncertainty rows, supersession via
  valid_until. No benchmarks published. Two takeaways: agmem's store choice is
  validated at the vendor level, and trace-derived boost/demote is the production
  precedent for idea #9.
- **Letta**: MemFS (git-backed), sleep-time/reflection/defrag subagents — all
  LLM-driven maintenance, different lane. **Engram** (SQLite+FTS5, deterministic
  lexical recall) shows a BM25-only market exists — agmem's BM25-only fallback mode is
  worth keeping first-class.

## Benchmarks to track

LoCoMo, LongMemEval (+abstention category), **Memora/FAMA** (forgetting-aware),
**PrecisionMemBench** (precision/noise), **ForgetEval** (2606.15903), MemSecBench
(poisoning), BEAM (1M–10M scale). The field's own instruments increasingly measure
exactly what agmem was designed around: invalidation correctness and precision, not
recall volume.
