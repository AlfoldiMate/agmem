# agmem — Idea & Foundation

**One sentence:** an agent memory system inspired by SurrealDB's Agent Memory
("Spectron"), radically simplified — **no REST API, no server-side LLM pipeline;
the Model Context Protocol is the only surface**, and the calling agent does the
thinking.

This document is the foundation for everything that follows. Part I records
everything publicly known about the inspiration (SurrealDB Agent Memory /
Spectron, researched 2026-08-28). Part II surveys the MCP memory-server
landscape we are entering. Part III grounds the design in the research
literature. Part IV distills the constraints and principles for agmem.

---

## Part I — The inspiration: SurrealDB Agent Memory ("Spectron")

### 1.1 Product identity and timeline

SurrealDB's agent memory offering carries two interchangeable names:
**"SurrealDB Agent Memory"** (marketing) and **"Spectron"** (product/internal —
binaries `spectron`/`spectrond`, env vars `SPECTRON_*`, npm package
`@surrealdb/spectron`, MCP tools `spectron_*`). It is a **closed-source Rust
application tier** in front of open-source SurrealDB — "one Rust binary with MCP
built in, no Python runtime." It is explicitly *not* an agent runtime and does
not manage the context window.

| Date | Event |
|---|---|
| 2025-06-27 | Thesis blog post: "The state of Agentic AI and the need for Agentic Memory" (Tobie Morgan Hitchcock) |
| 2025-12-17 | Agno (Agent OS) native SurrealDB memory-provider integration |
| 2026-02-17 | SurrealDB 3.0 released, branded "the future of AI agent memory"; $23M Series A extension ($44M total) |
| 2026-06-03 | Spectron launches — invite-only preview, "from $29/month" |
| 2026-06/07 | Ten public `spectron-*` framework-integration repos appear under the `surrealdb` GitHub org |
| 2026-08 | Still waitlist-gated for self-serve; docs fully published under `/docs/agent-memory` (mirrored at `/docs/spectron`) |

Positioning claim: unlike Mem0/Zep/Letta ("middleware orchestrating separate
stores"), Agent Memory *is* the database — "graph, vector, document, and time in
a single ACID transaction."

### 1.2 Memory taxonomy — eight pillars, six categories

**Eight architectural pillars:**

1. **Authoritative** — vetted documents (`source.kind = "document"`)
2. **Experiential** — conversational facts (`source.kind = "turn"`)
3. **Reconciliation** — one reconciler for all source types; conflicts become explicit uncertainty records, never silent winners
4. **Elaboration** — background linking of related facts (`derived_from` pointers)
5. **Reflection** — insights minted on demand (`POST /reflect`, lower default trust)
6. **Consolidation** — repeated observations crystallize into stable beliefs (background job, `proof_count`)
7. **Calibration** — confidence on every assertion; refuses overwrites below a configurable floor (default 0.7), emitting uncertainty instead
8. **Collective** — shared memory across agents/people with per-source provenance

**Six experiential memory categories**, each with its own lifecycle and
retrieval weight:

| Category | Content | Retention/decay |
|---|---|---|
| Episodic | Sessions and turns verbatim, in order; source of truth for quotes | permanent |
| Identity | Name, role, employer, preferences | long retention, low decay |
| Knowledge | Facts shared in chat (distinct from uploaded docs) | medium, decays |
| Context | Active tickets/sprints/working set | short, rapidly replaced |
| Instructions | Behavioral rules ("always British English"), applied at prompt assembly | until superseded |
| Uncertainty | Explicit "we don't know yet" rows | until resolved |

### 1.3 Data model (actual SurrealQL, from their docs)

Control plane: `context`, `api_key`, `context_api_key` (hash, principal,
grants, TTL), `context_api_key_usage` (append-only log).

**Knowledge (authoritative) tables, per context:**
- `document` — title, mime_type, storage_key, Blake3 `content_hash`
  (content-addressed dedup), version, pipeline `status:
  queued|extracting|chunking|embedding|keywording|ready|failed`
- `knowledge_chunk` — `embedding option<array<float, 3072>>` with
  `DEFINE INDEX ... HNSW DIMENSION 3072 DIST COSINE TYPE F32`; byte spans,
  `simhash`, `duplicate_of`, section, token_count
- `keyword` — RAKE keyphrases, own HNSW index, unique normalized form
- `knowledge_has_keyword` — `TYPE RELATION IN document OUT keyword`, scored

**Experiential tables, per context:**
- `session`, `turn` (role `user|assistant|system|tool`, content, unique (session, seq))
- `entity` — name, type, `memory_category: identity|knowledge|context`, 3072-d
  HNSW embedding, `resolves_to option<record<knowledge>>`, `source_turn`
- `attribute` — entity, key, value, **supersession chain** (`supersedes` /
  `superseded_by`), `valid_from`/`valid_until`, source_turn, temporal index
- `relates_to` — `TYPE RELATION IN entity OUT entity`, label, category,
  valid_from/valid_until — edges are documents carrying properties, confidence, timestamps
- `instruction`, `uncertainty` (about, reason, resolved, source_turn)
- `memory_chunk` — embedded turn segments (HNSW 3072, simhash dedup)
- `decision_trace` — query, tier, sources, token_cost, duration, api_key

SurrealDB features leveraged: multi-model document+graph (RELATION tables),
HNSW vector indexes, BM25 full-text, record links, MVCC/VERSION time-travel,
geospatial predicates. Embeddings are **deployment-locked to Google
`gemini-embedding-2` (3072-dim)** at launch.

### 1.4 Ingestion

- **Conversations:** turns via `POST /facts` / `/facts/batch`; server-side LLM
  extraction (`infer: full|triples|preview|none`) produces entities /
  attributes / relations; a **reconciler** decides create / update / supersede /
  flag per record and emits a `decision_trace`.
- **Documents:** async multi-modal pipeline (text, code, PDF, image, audio,
  video), per-context `IngestionProfile` (TextOnly → MultimodalFull); originals
  in S3-compatible object store; Blake3 dedup; content-aware chunking; a
  **non-LLM RAKE pass** builds the keyword graph. Documents and conversations
  flow through **the same reconciler** (stated differentiator).
- **Dedup:** simhash + `duplicate_of` on chunks, idempotency keys on writes,
  content hash on documents.
- **Forgetting:** supersession, not deletion — `valid_until` set with reason
  recorded; category-aware decay (context fades fast, identity is stable);
  retrieval-based reinforcement; `POST /lifecycle/decay|expire`; `forget` verb
  is soft-delete by default, `purge: true` for GDPR erasure, `dryRun` supported.
  Optional PII redaction at ingest.
- **Tri-temporal model:** **system time** (MVCC: what the DB contained at T),
  **known time** (when first believed; `as_of` traverses supersession chains),
  **valid time** (`valid_from`/`valid_until`: when true in the world).
- **Background understanding:** reflection (on demand), elaboration (async
  linking sweep), consolidation (async, `derived_from` + `proof_count`).

### 1.5 Retrieval — eight fused signals, four tiers

Signals (per-feature scores recorded on every `retrieval_trace`):
1. dense vector recall; 2. BM25 lexical; 3. graph traversal (1–2 hops from
seed entities); 4. RAKE keyword bridges; 5. section embeddings / document
links; 6. Personalized PageRank biased toward query seeds; 7. geographic
recall; 8. **trace-derived features** — retrieval reads its own history:
entities that helped similar queries get boosted, records tied to corrections
demoted.

Query ladder:

| Tier | Mechanism | Cost |
|---|---|---|
| 1 Direct lookup | typed key lookup on the entity/attribute graph — no embeddings, no LLM | sub-ms, zero tokens |
| 2 Response reuse | semantic match against prior answers, **entity-aware invalidation** (cache valid only while every cited fact is current) | no generation on hit |
| 3 Hybrid retrieval | all eight signals over a bounded pool (256 candidates), LLM synthesis, k=10 default / 50 max | hundreds of ms |
| 4 Full-context fallback | broader sweep, deeper traversal, optional HyDE rewrite; fires when tier 3 is thin or fused confidence < 0.40 | highest, explicitly traced |

Document-query extras: `useHyde`, `decomposeQuery`, `useReranker` (external
cross-encoder via `SPECTRON_RERANKER_URL`).

### 1.6 API surface — seven verbs everywhere

Seven core verbs across MCP, REST, SDKs, CLI:
**`remember, recall, context, reflect, forget, upload, inspect`**.

**MCP tools** (the part most relevant to agmem — the binary speaks MCP
natively; remote endpoint = context host + `/mcp`):

- `remember` — `text` (req), `session_id`, `scope` (DNF array, e.g.
  `[["org/acme/user/alice"]]`), `labels`, `infer: full|preview|none`. Returns a
  structured diff (entities/attributes/relations) + `trace_id`.
- `recall` — `query` (req), `k` (10/max 50), `mode: vector|bm25|graph|hybrid`,
  `lens` (DNF read selector), `labels`. Ranked hits with scores, source kinds, `trace_id`.
- `context` — `query` (req), `lens`, `labels`. Returns a **markdown block**
  (profile + preferences + relevant facts) for system-prompt injection.
- `reflect` — `query` (req), `persist` (default false). Reflection text +
  evidence refs.
- `forget` — `query` (req), `purge`. Soft-delete via `valid_until`.
- `upload` — `bytes_base64` (req), title/source/mime/filename/scopes/labels.
  Async: `{ id, status: "queued", content_hash, deduplicated, version }`.
- `inspect` — `ref` (req): `"entity:Person/alice" | "trace:<id>" | "document:<id>"`.

**Everything else it also ships (and agmem will not):** a full REST API
(`/api/v1/{context}/facts|sessions|query|chat(SSE)|state|profile|entities|
reflect|forget|lifecycle/*|elaborate|consolidate|fsck|documents|traces|audit|
keys|scopes|principals|health`), a separate Management API (contexts CRUD,
migrations, key minting, cloud-brokered tokens), TypeScript SDK
(`@surrealdb/spectron`), Python (inside `surrealdb` v3), claimed generated
clients for Go/Swift/Kotlin/Haskell/Elixir/Dart, a CLI (`spectron` client with
TUI + REPL, `spectrond` server with api/worker/scheduler/management processes),
and ~15 framework adapters (`spectron-hermes`, `spectron-eve`,
vercel-ai-sdk, openai-agents, pydantic-ai, crew-ai, google-adk, langchain,
mastra, n8n, ElevenLabs/LiveKit voice, AgentOps observability…).

**Security model:** seven grant verbs (`memory:read/write/forget`,
`scope:read/create/delete`, `grant:manage`), hierarchical scope paths in DNF
selectors, "labels and lens filter within the grant; they never widen access,"
deny-by-default, key attenuation, prompt-injection sanitization before
interpolation into extraction/synthesis prompts, graph-resident audit of denied
attempts. SOC 2 Type 2, GDPR, ISO 27001.

**Traces:** three first-class trace node types — `retrieval_trace` (candidate
sets per index, fused scores, returned subset), `decision_trace` (extraction
input, per-record outcome created/updated/superseded/flagged, acting
principal), `response_trace` (assembled prompt, model response, tokens, cost,
latency). Every fact carries a `source` object: kind, ref, trust, byte-offset
lexical span, derivation chain — "answers are walkable back to originating
message bytes."

### 1.7 Deployment, pricing, adoption signals

- Cloud (waitlisted): Lite $29/mo (1M tokens), Standard $299/mo (10M), Plus
  $1,099/mo (40M); Enterprise custom (single-tenant, air-gapped, BYOM).
- Self-hosted `spectrond` exists but the binary is **closed source**; only the
  SurrealDB engine underneath is open.
- **No embedded/in-process mode** — docs verbatim: "There is no supported
  in-process API that runs extraction and recall inside your application binary."
- **No published benchmarks** — the "accuracy promise" page has zero numbers;
  no LOCOMO/LongMemEval results (Mem0 and Zep both publish theirs). The
  accuracy argument is architectural, not empirical.
- Weak community signal so far: integration repos at 0–2 stars, SurrealDB 3.0
  HN post at 26 points / 3 comments, no dedicated Spectron launch thread found.
- `github.com/surrealdb/agent-memory` 404s; embedding model locked
  deployment-wide.

### 1.8 What we take from Spectron, and what makes it "not easy"

Worth stealing (mostly *ideas*, portable without their machinery):
- The **seven-verb surface** — `remember/recall/context/reflect/forget/inspect`
  (± upload) is a clean, teachable vocabulary; `context` returning a
  prompt-ready markdown block is a genuinely good MCP tool.
- **Supersession over deletion** (`supersedes`/`superseded_by`,
  `valid_from`/`valid_until`) and explicit **uncertainty records** instead of
  silent overwrites.
- **Provenance on every fact** (source kind, ref, derivation chain) and
  traceable retrieval decisions.
- **Tiered retrieval** — cheap direct lookup before expensive hybrid search.
- Category-aware decay (identity stable, working context fades fast).
- SurrealDB itself as the single multi-model store (document + graph + vector
  + full-text in one engine) — this part is open source and free.

What makes Spectron heavy — the complexity agmem deliberately drops:
- Server-side **LLM extraction/reconciliation/synthesis pipeline** (five model
  hooks per context, provider API keys, token-metered pricing).
- The **entire REST + Management API surface**, SDK matrix, CLI/TUI, key
  minting, delegation headers, multi-tenant control plane.
- Multi-modal document ingestion (OCR/CLIP/STT endpoints, object stores).
- Eight retrieval signals incl. PageRank, geo, response-cache invalidation.
- Closed source, waitlist, $29–1,099/mo.

---

## Part II — The MCP memory-server landscape (what already exists)

### 2.1 The decisive protocol fact: sampling is dead

MCP **sampling** (`sampling/createMessage` — a server asking the *client's*
LLM to do work) was the mechanism intended for "server-side extraction without
server-side API keys." It failed: Claude Desktop and Claude Code never shipped
it, no major memory server relied on it, and it is **deprecated as of MCP spec
revision 2026-07-28** (SEP-2577, 12-month removal window; "new implementations
SHOULD NOT adopt it").

**Consequence:** an MCP-only, LLM-free memory server has exactly one viable
extraction model — **the calling agent does all distillation, and the tool
contract (names, input schemas, descriptions) is the extractor.** This is not
the easy fallback; post-2026 it is the only spec-sanctioned design. Transports:
stdio for local (default everywhere), streamable HTTP if remote ever matters
(the old HTTP+SSE transport is deprecated too).

### 2.2 The field, compressed

| System | Tools | Data model | Server-side LLM? | Embeddings |
|---|---|---|---|---|
| Official `server-memory` (Anthropic) | 9: `create_entities`, `create_relations`, `add_observations`, 3 deletes, `read_graph`, `search_nodes`, `open_nodes` | entity/relation/observation triples | **No** | none (substring search) |
| OpenMemory MCP (Mem0 local) | 4: `add_memories`, `search_memory`, `list_memories`, `delete_all_memories` | extracted facts | **Yes** (OpenAI mandatory) | OpenAI |
| mem0-mcp (hosted) | 9 incl. `update_memory` (read-before-write), scoped bulk deletes | facts + optional graph | **Yes** | platform |
| Graphiti/Zep MCP | ~13: `add_memory` (episodes), `search_nodes`, `search_memory_facts`, `add_triplet`, … | bi-temporal KG (`valid_at`/`invalid_at`) | **Yes** (multi-call per episode) | OpenAI/Voyage/local |
| Letta (MemGPT) | agent-internal: `core_memory_append/replace`, `archival_memory_insert/search`, `conversation_search` | size-capped memory blocks + recall + archival | it *is* the agent | configurable |
| basic-memory | ~20: `write_note`, `edit_note`, `search_notes`, `build_context`, … | Markdown entities, `[category] fact #tag` observations, `[[wiki]]` relations, `memory://` URIs | **No** | FastEmbed (optional) |
| memory-bank (alioshr) | 5: list/read/write/update over project dirs | Markdown docs | **No** | none |
| mcp-server-qdrant | **2**: `qdrant-store`, `qdrant-find` | text + metadata | **No** | **FastEmbed local** |
| mcp-memory-service (doobidoo) | ~9 + Claude Code hooks | tagged memories + typed KG + decay | **No** (core) | **local ONNX** MiniLM |
| Anthropic `memory_20250818` tool | 6 commands: `view`, `create`, `str_replace`, `insert`, `delete`, `rename` over `/memories` | file tree | **No** | none |
| Claude Code auto memory | built-in | `MEMORY.md` index (~25KB startup cap) + topic files per repo | No (session model writes it) | none |

### 2.3 Lessons the landscape teaches

1. **Three proven LLM-free shapes:** (a) client-curated graph triples (official
   server — best relationship queries, worst retrieval), (b) structured
   Markdown/files (basic-memory, memory-bank, Anthropic memory tool — best
   model affordance: frontier models are *trained* on the file-editing loop),
   (c) tagged text + **local embeddings** (qdrant-mcp with FastEmbed,
   mcp-memory-service with ONNX MiniLM — best retrieval per unit of
   complexity, zero external API calls). The strongest designs combine (b) or
   (a) with (c)'s search.
2. **Small tool surfaces get used; big ones don't.** Successful servers
   converge on 2–9 tools around store/search/list/delete. Qdrant's
   env-configurable tool descriptions (`TOOL_STORE_DESCRIPTION`) are a cheap
   lever to steer *when* an agent reaches for memory.
3. **Extraction quality lives in the tool contract.** The official server's
   suggested system prompt, basic-memory's `- [category] fact #tag` observation
   syntax, and the memory tool's auto-injected "ALWAYS VIEW YOUR MEMORY
   DIRECTORY FIRST / ASSUME INTERRUPTION" prompt are the load-bearing
   extractors. Schema + description = pipeline.
4. **What LLM-free servers must add structurally** (nothing dedups for them):
   idempotent duplicate-skipping writes; read-before-update guards;
   caller-driven supersede/invalidate semantics; size caps + an index file to
   bound startup load (Claude Code's ~25KB cap); consolidation as an
   agent-invoked operation, not a background LLM job. Unbounded growth is the
   #1 reported failure of the simple servers; naive substring search is the
   #1 complaint about the official one.
5. **Scoping:** at least two axes — a project/namespace axis (with
   path-traversal validation) and an actor axis (user/agent); bulk deletes
   must be scope-confirmed. Mem0's user/agent/app/run hierarchy and Graphiti's
   `group_id` are the reference vocabularies.
6. **MCP primitives:** tools-first (the only primitive all clients support);
   prompts as memory *rituals* (slash-command templates like `/remember`,
   `/checkpoint`); resources (`memory://…` URIs) and notifications as
   progressive enhancement only. Use tool annotations
   (readOnly/destructive/idempotent) like basic-memory does.
7. **The incumbent is free:** Claude Code ships per-repo auto memory
   (MEMORY.md + topic files). An MCP memory server must out-signal a built-in
   that costs nothing — its pitch has to be cross-client sharing, structure,
   retrieval quality, and temporal correctness.

---

## Part III — Research grounding

### 3.1 Canonical mechanisms (with the formulas)

- **Generative Agents** (Park et al. 2023, arXiv:2304.03442) — memory stream +
  retrieval scored as `α·recency + α·importance + α·relevance` (each min-max
  normalized; recency = `0.995^Δhours` since *last access*; importance =
  LLM-rated 1–10 at write time; relevance = cosine). **Reflection**: when
  summed importance of recent events crosses a threshold, generate salient
  questions, retrieve, and write cited insights back into the same stream.
- **MemGPT** (Packer et al. 2023, arXiv:2310.08560) — context window as RAM,
  external store as disk; the agent pages via its own tool calls
  (`core_memory_append/replace`, archival insert/search); memory-pressure
  interrupt at ~70% window. Two ideas to keep: a small always-in-context
  editable core block, and *the agent decides what to persist*.
- **Reflexion** (Shinn et al. 2023, arXiv:2303.11366) — store the distilled
  **lesson**, not the trajectory; a bounded window of ≤3 lessons beats
  unbounded accumulation (91% pass@1 HumanEval vs GPT-4's 80%).
- **CoALA** (Sumers et al. 2023, arXiv:2309.02427) — the standard taxonomy:
  working memory + **episodic / semantic / procedural** long-term stores. Use
  as *metadata* (`kind: episode|fact|lesson`), not separate databases.
- **MemoryBank** (Zhong et al. 2023, arXiv:2305.10250) — Ebbinghaus
  forgetting: retention `R = e^(−t/S)`; strength `S` starts at 1, `S += 1` on
  each recall, `t` resets on access. One float + one timestamp per record buys
  usage-based decay *and* importance for free.
- **Zep/Graphiti** (Rasmussen et al. 2025, arXiv:2501.13956) — **bi-temporal
  facts**: `t_valid`/`t_invalid` (true in the world) ×
  `t_created`/`t_expired` (known to the system); contradictions *invalidate*,
  never delete. LongMemEval +18.5 pts over full-context with ~90% lower
  latency. The single cheapest high-value schema decision; even the halved
  version (`valid_from`/`invalid_at`) captures most of the benefit.
- **Mem0** (Chhikara et al. 2025, arXiv:2504.19413) — the de-facto standard
  consolidation loop: extract candidate facts → retrieve top-10 similar
  memories → decide **ADD / UPDATE / DELETE / NOOP** per candidate. In an
  LLM-free server this decision moves to the *calling agent*, guided by the
  tool contract. Note Mem0's own 2026 production shift toward add-only writes
  with read-time conflict resolution.
- **A-MEM** (Xu et al. 2025, arXiv:2502.12110) — Zettelkasten notes with
  write-time enrichment (keywords, tags, context) and linked neighbors;
  write-time enrichment alone captures most of the value.
- **HippoRAG** (Gutiérrez et al. 2024/2025, arXiv:2405.14831, 2502.14802) —
  entity graph + Personalized PageRank buys multi-hop association cheaply;
  only worth it if multi-hop queries are a demonstrated workload.

### 3.2 Benchmark reality check

- **LOCOMO** (arXiv:2402.17753): full-context 72.9 > Mem0g 68.4 > Mem0 66.9 >
  Zep 66.0 > RAG 60.5 (Mem0's run). **Memory systems win on cost (~10% of
  tokens, p95 1.4s vs 17s), not accuracy, until history exceeds the window.**
- **Letta's filesystem result:** an agent given plain files + grep/search
  tools scored **74.0 on LOCOMO with gpt-4o-mini** — above every published
  memory system. Agents use *familiar* tools well; exotic query APIs poorly.
  Strong argument for a file-shaped, simple-verb surface.
- **Verbatim beats extracted** (arXiv:2601.00821): controlled ablation —
  verbatim chunks beat LLM-extracted artifacts by +15.9 pts (LOCOMO) / +22.0
  (LongMemEval-S); extraction-only *never* beat naive RAG. **Facts are an
  index over episodes, never a replacement.**
- **LongMemEval** (arXiv:2410.10813): temporal reasoning, knowledge updates,
  and abstention are where systems actually differentiate.
- **MemoryAgentBench** (arXiv:2507.05257): nobody masters conflict resolution
  + selective forgetting; commercial systems not uniformly better than simple
  RAG. Cross-paper scores are not comparable; benchmark on your own workload.

### 3.3 Documented failure modes (design them out on day one)

- **Memory poisoning** (MINJA, arXiv:2503.03704: >95% injection success via
  ordinary queries) — provenance and write gating are *security* features.
- **Experience-following** (arXiv:2505.16067): agents imitate retrieved past
  records; bad memories compound. Quality-gated writes + deletion of harmful
  records ≈ +10% absolute task gains.
- **Staleness**: confidently retrieved outdated facts — the argument for
  temporal validity and supersession.
- **Over-retrieval / context pollution**: retrieval budgets, diversity (MMR),
  and the ability to return "nothing relevant" matter.
- **Unbounded growth**: the #1 failure of simple LLM-free servers —
  consolidation/decay must exist, even if agent-invoked.

---

## Part IV — agmem: constraints, principles, and shape

### 4.1 Hard constraints (decided)

1. **MCP is the only surface.** No REST API, no management API, no SDKs, no
   HTTP control plane. Tools-first; stdio transport first.
2. **No server-side LLM calls, ever.** No provider API keys, no extraction
   pipeline, no token metering. The calling agent distills; the tool contract
   teaches it how. (Sampling is deprecated — this is the only compliant
   design anyway.)
3. **Easier than Spectron** in every dimension: open, local-first, no
   waitlist, no per-token pricing, minutes to run.

### 4.2 Design principles (evidence-backed defaults for the upcoming work)

1. **Small verb surface, Spectron vocabulary.** Start from
   `remember / recall / context / forget / inspect` (reflect optional; upload
   probably out of scope). Every tool description doubles as the extraction
   prompt; consider configurable descriptions à la qdrant-mcp.
2. **Episodes are ground truth; facts are an index.** `remember` can accept
   both a verbatim episode and the caller's distilled facts; facts link to
   their source episode (provenance ref on every record).
3. **Supersede, don't delete.** `valid_from` / `invalid_at` (+ `created_at`)
   on facts; caller-driven supersession (`supersedes: <id>`); `forget` is
   soft by default with a purge escape hatch.
4. **One record schema, three kinds** — `episode | fact | lesson` — with
   per-kind decay policy (identity-like facts stable, working context fades),
   not three storage systems.
5. **Retrieval floor: hybrid local search.** Local embeddings (FastEmbed/ONNX
   class, no API) + BM25/full-text, scored roughly as
   `relevance + recency + importance` with Ebbinghaus reinforcement
   (`S += 1` on access). Graph traversal only if/when multi-hop demand is real.
6. **A `context` tool that returns a prompt-ready markdown block** — the best
   single idea in Spectron's MCP surface, and the thing Claude Code's built-in
   memory can't do across clients.
7. **Bound growth structurally:** dedup on write (content hash / simhash-ish),
   idempotent writes, size-capped index, agent-invoked consolidation
   (a `reflect`/`consolidate` verb the *agent* runs, e.g. from an MCP prompt
   ritual like `/checkpoint`), decay as ranking penalty rather than deletion.
8. **Two scoping axes** — namespace/project + actor — validated against path
   traversal; destructive tools scope-confirmed and annotated
   (readOnly/destructive/idempotent hints).
9. **Storage: one embedded multi-model store.** SurrealDB (embedded, e.g.
   SurrealKV/RocksDB backend) is the natural homage — document + graph +
   vector + BM25 in one open-source engine, no separate server required. The
   choice stays open until the architecture pass, but "one file on disk, one
   process" is the bar.
10. **Win condition is not benchmark rank.** It's: cross-client shared memory,
    temporal correctness (updates/supersession), inspectable provenance, and
    zero external dependencies — evaluated on our own workload.

### 4.3 Open questions (for the next requests)

- Exact tool list and input schemas (start minimal: how few verbs survive?).
- Storage engine decision: embedded SurrealDB vs SQLite(+vec) vs plain files
  with an index — weigh the Letta filesystem result against retrieval quality.
- Whether `reflect` (agent-run consolidation) is v1 or v2.
- Embedding model choice and whether vector search is v1 or BM25 suffices
  initially.
- Scope model depth: flat namespaces vs hierarchical DNF-style paths.
- MCP prompts (`/remember`, `/checkpoint` rituals) and resources
  (`memory://` URIs) as progressive enhancements.

---

## Appendix — Sources

### SurrealDB Agent Memory / Spectron
- Product: https://surrealdb.com/agent-memory · https://surrealdb.com/agent-memory/deep-dive · https://surrealdb.com/platform/spectron · https://surrealdb.com/pricing/spectron · https://surrealdb.com/mcp
- Docs: https://surrealdb.com/docs/agent-memory — esp. `/reference/mcp-tools`, `/reference/rest-api`, `/reference/data-model-and-schema`, `/reference/configuration`, `/reference/cli`, `/reference/management-api`, `/architecture/eight-pillars-and-categories`, `/architecture/tri-temporal-model`, `/architecture/coherence-retrieval-and-tiers`, `/architecture/traces-and-evolution`, `/mental-model/memory-lifecycle`, `/quickstarts/embedded`, `/welcome/accuracy-promise`
- Blog: https://surrealdb.com/blog/introducing-surrealdb-3-0--the-future-of-ai-agent-memory · https://surrealdb.com/blog/the-state-of-agentic-ai-and-the-need-for-agentic-memory · https://surrealdb.com/blog/agents-with-memory-how-agno-and-surrealdb-enable-reliable-ai-systems
- Integrations: https://github.com/surrealdb/spectron-hermes · https://github.com/surrealdb/spectron-eve · https://surrealdb.com/docs/agent-memory/integrations
- Press: https://venturebeat.com/data/surrealdb-3-0-wants-to-replace-your-five-database-rag-stack-with-one · https://tech.eu/2026/02/17/surrealdb-secures-23m-and-launches-surrealdb-3-0-to-address-ai-agent-memory-challenges/

### MCP memory servers & protocol
- Official memory server: https://github.com/modelcontextprotocol/servers/tree/main/src/memory
- Mem0/OpenMemory: https://mem0.ai/blog/introducing-openmemory-mcp · https://github.com/mem0ai/mem0-mcp
- Graphiti MCP: https://github.com/getzep/graphiti/tree/main/mcp_server
- Letta: https://docs.letta.com/guides/legacy/memgpt_agents_legacy · https://github.com/oculairmedia/letta-mcp-server
- basic-memory: https://github.com/basicmachines-co/basic-memory · memory-bank: https://github.com/alioshr/memory-bank-mcp
- Qdrant/Chroma: https://github.com/qdrant/mcp-server-qdrant · https://github.com/chroma-core/chroma-mcp
- mcp-memory-service: https://github.com/doobidoo/mcp-memory-service · supermemory: https://github.com/supermemoryai/supermemory
- Anthropic memory tool: https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool · Claude Code memory: https://code.claude.com/docs/en/memory
- MCP spec / sampling deprecation: https://modelcontextprotocol.io/specification/2026-07-28/client/sampling · https://blog.modelcontextprotocol.io/posts/2026-07-28/ · https://github.com/anthropics/claude-code/issues/1785

### Research
- Generative Agents: https://arxiv.org/abs/2304.03442 · Reflexion: https://arxiv.org/abs/2303.11366 · MemGPT: https://arxiv.org/abs/2310.08560 · CoALA: https://arxiv.org/abs/2309.02427 · MemoryBank: https://arxiv.org/abs/2305.10250
- HippoRAG: https://arxiv.org/abs/2405.14831 · https://arxiv.org/abs/2502.14802 · Zep: https://arxiv.org/abs/2501.13956 · A-MEM: https://arxiv.org/abs/2502.12110 · Mem0: https://arxiv.org/abs/2504.19413 · MemOS: https://arxiv.org/abs/2507.03724 · MIRIX: https://arxiv.org/abs/2507.07957
- Surveys: https://arxiv.org/abs/2404.13501 · https://arxiv.org/abs/2504.15965 · https://arxiv.org/abs/2505.00675 · https://arxiv.org/abs/2509.18868
- Benchmarks: LOCOMO https://arxiv.org/abs/2402.17753 · LongMemEval https://arxiv.org/abs/2410.10813 · MemBench https://arxiv.org/abs/2506.21605 · MemoryAgentBench https://arxiv.org/abs/2507.05257
- Critiques/attacks: Letta filesystem benchmark https://www.letta.com/blog/benchmarking-ai-agent-memory/ · Verbatim-vs-artifacts https://arxiv.org/abs/2601.00821 · Structural memory https://arxiv.org/abs/2412.15266 · Experience-following https://arxiv.org/abs/2505.16067 · MINJA https://arxiv.org/abs/2503.03704 · AgentPoison https://arxiv.org/abs/2407.12784 · Mem0 2026 state report https://mem0.ai/blog/state-of-ai-agent-memory-2026
