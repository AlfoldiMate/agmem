# Research: a document store alongside an agent memory store (2026-09-03)

Scope: what "document store next to memory" means in SurrealDB Agent Memory and
comparable systems, and the design questions agmem (Rust + SQLite + MCP) has to
answer to add a document layer. Web research only; repo untouched.

---

## 1. SurrealDB Agent Memory (formerly/internally "Spectron")

Product: https://surrealdb.com/agent-memory · Docs root: https://surrealdb.com/docs/agent-memory
Binaries/env are still `spectron`/`spectrond`, `SPECTRON_*`, `@surrealdb/spectron`.
Note: PyPI `surreal-memory` is an unrelated third-party project (spreading-activation
graph on a SurrealDB backend) — https://pypi.org/project/surreal-memory/ — not
SurrealDB's product.

### 1.1 Two streams, one graph
Docs: https://surrealdb.com/docs/agent-memory/mental-model/two-layer-architecture

- **Authoritative stream** = documents: "manuals, policies, product data, repos,
  structured exports". `source.kind = document` (or `upsert`). Default trust: high.
- **Experiential stream** = what users/agents said plus derived facts.
  `source.kind = turn | reflect | elaboration | consolidation`. Default trust: lower.
- Both live as records and edges in *the same* SurrealDB graph — not a separate
  store. "Retrieval, elaboration, and consolidation see one entity/relation graph."
- Conflicts between streams are not resolved silently: the reconciler records the
  experiential assertion with provenance intact and emits an `uncertainty` record
  (https://surrealdb.com/docs/agent-memory/reasoning/cross-layer-linking).

So SurrealDB's "document store" is not a blob bucket; it is the authoritative half
of the same knowledge graph, with the raw bytes retained and addressable.

### 1.2 What a document holds, and how it is stored
Docs: https://surrealdb.com/docs/agent-memory/ingest/authoritative/uploading-documents
Deep-dive: https://surrealdb.com/agent-memory/deep-dive

- **Identity is content-addressed**: "Every document is identified by a BLAKE3 hash
  of its raw bytes." Re-uploading identical bytes returns `deduplicated: true`.
  If a second scope uploads the same hash, the scope clause is *unioned* onto the
  existing document and a `document.scope_widen` audit event is emitted.
  Scope on a document is immutable after creation (re-upload with scopes → 400).
- **Raw bytes are kept**: `GET /documents/{id}/raw`. Chunks: `GET /documents/{id}/chunks`.
  Upload is async (`POST /api/v1/{ctx}/documents` → 202; poll `GET /documents/{id}`).
- **Chunks ("passages") are first-class rows**: "Passages are first-class chunk rows
  with their own embeddings, byte spans into the original artefact, and edges to the
  entities extracted from them." Chunking is "overlapping segments"; exact sizes are
  not published. Oversized chunks fail permanently (WS message-size limit alignment
  between `SPECTRON_DB_WS_MAX_MESSAGE_BYTES` and `SURREAL_WEBSOCKET_MAX_MESSAGE_SIZE`).
- **Non-LLM keyword layer**: RAKE keyword nodes with PMI-scored edges, used as
  "keyword bridges" in retrieval.
- **Cross-document links**: `knowledge_links_to` edges extracted from markdown/HTML/
  PDF annotations.
- Accepted MIME types: text/plain, text/markdown, application/json, text/html,
  application/pdf; plus png/jpeg/webp/gif, wav/mpeg/ogg/flac/aac, mp4/webm/quicktime.
  Processing depth set per Context by an `IngestionProfile`
  (`TextOnly` → `TextPlusKeyword` → `StandardMultimodal` → `MultimodalFull`).
- Upload response: `{ id, status: "queued", content_hash, deduplicated, version }`.
  Versioning beyond that field is not documented; identity is by content, not by
  a mutable "document id" with revisions.

### 1.3 How memories relate to documents (provenance)
Docs: https://surrealdb.com/docs/agent-memory/mental-model/provenance-and-traceability
Marketing: https://surrealdb.com/agent-memory/provenance

"No fact-bearing record is anonymous." Every fact-bearing row (entity, attribute,
relation, instruction, uncertainty) carries a `source` object:

| field | meaning |
|---|---|
| `source.kind` | `turn`, `document`, `upsert`, `reflect`, `elaboration`, `consolidation` |
| `source.ref` | id of the originating turn, document, or trace |
| `source.session_id`, `source.turn_at` | conversation anchors |
| `source.valid_from`, `source.span` | temporal anchor + **character positions** ("quote position in the originating message") — supports "jump to quote" |
| `source.location` | optional geo |
| `source.trust` | ranking; documents outrank turns by default |
| `source.derived_from` | lineage for derivative records (reflection/consolidation) |

Three trace tables complete the chain: `decision_trace` (extraction/reconciliation),
`retrieval_trace` (ranked reads), `response_trace` (`/chat`, `/reflect`). Traces are
queryable (`GET /api/v1/{ctx}/traces[/{id}]`, `spectron inspect trace:…`).
"How a memory formed is itself a graph you can traverse and audit."

Direction of the link: **memory → document** via `source.ref` + `source.span`, and
**passage → entities** via edges from chunk rows. Both directions are queryable.

### 1.4 Retrieval over documents + memories
Docs: https://surrealdb.com/docs/agent-memory/retrieve/recall ·
https://surrealdb.com/docs/agent-memory/retrieve/hybrid-search

- One read path ranks facts and passages together: `POST /api/v1/{ctx}/query`
  (`k` ≤ 50, `lens`, `labels`, `asOf` temporal playback, `scope_view`,
  `include: facts | passages | both`, `includeDuplicates`). `/context` returns a
  pre-formatted prompt string; `/chat` adds synthesis.
- Response: `tier` (`direct | cache | hybrid | full_context`), `hits[]` each with
  `source` type (`entity`, `attribute`, `memory_chunk`, `chunk`, `section`) and a
  normalised `score`, `contextHits[]` (same-section siblings, not counted in `k`),
  `queryMs`, `trace.traceId`.
- Fusion: RRF over vector + BM25 (`score(d) = Σ 1/(k + rank_i(d))`, `rrf_k` default
  60, `vector_weight` 0–1), then `hybrid_graph` rerank with `graph_alpha` (default
  0.3) over keyword bridges, typed-knowledge edges, section-heading similarity,
  cross-document link density, document summaries, personalised PageRank.
  Marketing lists "eight signals": vector, BM25, graph traversal, keyword bridges,
  document links, PageRank, geographic recall, trace-derived features.
- Query tiering: Tier 1 direct graph lookup; Tier 2 response reuse with
  entity-aware invalidation; Tier 3 hybrid; Tier 4 full-context fallback.

### 1.5 Lifecycle
Docs: https://surrealdb.com/docs/agent-memory/mental-model/memory-lifecycle ·
https://surrealdb.com/docs/agent-memory/operations/forget

- **Tri-temporal**: system time (MVCC), known time (first belief), valid time.
- **Supersession**: old belief gets `valid_until`, "closed in time, not erased";
  new one wins when same source.
- **Category-aware decay with reinforcement**: six categories — episodic,
  identity, knowledge, context, instructions, uncertainty — each with its own
  lifetime (context short, knowledge medium, identity long, instructions until
  revoked, uncertainty until resolved). Retrieval reinforces. Note: *uploaded
  documents are not one of these categories* — "knowledge" is "facts learnt in
  conversation, distinct from uploaded manuals". Documents sit in the authoritative
  stream and do not decay on the schedule; they are removed explicitly.
- **Forget**: `POST /forget { query?, dryRun?, purge? }`. Default = expire (hidden
  from `/query`, `/profile`, `/state`, kept for audit). `purge: true` = hard delete
  of entities, fact history, relations, and "linked conversational chunks"; for
  document-derived facts the source passages go too. Documents also have
  `DELETE /documents/{id}` and `POST /scopes/forget` for subtree erasure.
  Docs warn forgetting is not prevention — re-mention re-extracts.

### 1.6 Scoping
Docs: https://surrealdb.com/docs/agent-memory/mental-model/contexts-and-scope

- **Context** = hard isolation, its own SurrealDB `(namespace, database)` and keys.
  Documents never cross Contexts.
- **Scope** = hierarchical path tags inside a Context (`org/acme/user/alice/`),
  stored as an OR of conjunctive clauses; reads match by subset (parent-scoped
  facts are visible to child queries, not vice versa). Grants (`memory:read`,
  `memory:write`, `memory:forget`) on paths; `org/acme/*` covers descendants.
- Documents carry scope clauses; same-hash uploads from another scope widen the
  clause rather than duplicating the bytes.

### 1.7 MCP surface
Docs: https://surrealdb.com/docs/agent-memory/reference/mcp-tools

Seven **tools**, no MCP resources or prompts documented:
`remember(text, session_id?, scope?, labels?, infer?)`,
`recall(query, k?, mode?, lens?, labels?)` → "ranked hits with scores and source
kinds" over facts *and* passages, `context(query?, lens?, labels?)` → markdown,
`reflect(query, persist?)`, `forget(query, purge?)`,
`upload(bytes_base64, title?, source?, mime_type?, filename?, scopes?, labels?)`
→ `{id, status:"queued", content_hash, deduplicated, version}`,
`inspect(ref)` → entity / trace / document metadata (and chunks) by id.
Everything returns a `trace_id`.

---

## 2. Comparable designs

### 2.1 Zep / Graphiti — episodes as the non-lossy layer
Paper: https://arxiv.org/abs/2501.13956 · README: https://github.com/getzep/graphiti ·
Docs: https://help.getzep.com/adding-data-to-the-graph ·
https://help.getzep.com/chunking-large-documents ·
https://help.getzep.com/graphiti/core-concepts/graph-namespacing

- **Document/artifact layer = episodic subgraph.** An episode is one ingested
  unit (`message`, `text`, or `json`), stored verbatim: "a non-lossy data store"
  from which entities and relations are extracted.
- **Link to distilled memory**: episodic edges (episode → entity it mentions) and
  an `episodes` field on every entity edge; the paper describes "bidirectional
  indices between semantic edges and source episodes" so facts can be traced for
  citation/quotation. Episode metadata is "projected onto every graph artifact
  derived from the episode" for filtering.
- **Size caps**: `graph.add` accepts ≤ 10,000 characters; cookbook recommends
  ≤ 500-character chunks for graph quality, same `document_id` across chunks so a
  later chunk's extraction can reference earlier ones (pronoun resolution).
  Contextual retrieval pattern: prepend LLM-written context, `${context}\n\n---\n\n${chunk}`.
- **Retrieval**: hybrid BM25 + cosine + graph BFS, RRF or node-distance reranking,
  no LLM on the read path.
- **Lifecycle**: bi-temporal edges (`valid_at`, `invalid_at`, `created_at`,
  `expired_at`); contradictions handled by invalidating the old edge, not deleting.
- **Scoping**: `group_id` on every node/edge; episode and all extracted artifacts
  share the group.

### 2.2 mem0 — memories first, sources optional
Docs: https://docs.mem0.ai/core-concepts/memory-operations/add ·
https://docs.mem0.ai/platform/features/graph-memory · Paper: https://arxiv.org/abs/2504.19413

- No document layer as such. `add()` runs extraction (ADD/UPDATE/DELETE/NOOP over
  existing memories) from message pair + rolling summary + recent messages.
  `infer=False` stores the raw message *as a memory* (docs warn this duplicates if
  mixed with inferred memories). `metadata` and `expiration_date` per memory.
- Graph memory (now built-in, Neo4j path removed) is entity ↔ memory co-occurrence,
  untyped; used as a ranking boost on top of vector + BM25.
- Provenance to the raw conversation is not a documented first-class link — the
  paper reports 90%+ token savings precisely by *not* keeping full history in
  the retrieval path.
- Scoping: `user_id`, `agent_id`, `app_id`, `run_id`.

### 2.3 Letta / MemGPT — archival passages; files came and went
Docs: https://docs.letta.com/guides/core-concepts/memory/archival-memory ·
https://docs.letta.com/guides/agents/sources (deprecated) ·
https://www.letta.com/blog/context-repositories/ · https://github.com/letta-ai/ai-memory-sdk

- Three tiers: core memory blocks (in context), **recall memory** (full message
  history, searchable), **archival memory** (pgvector passages with `content`,
  `tags`, optional timestamp; semantic search + tag/temporal filters; agent can
  insert, cannot edit).
- The document layer was **Letta Filesystem / data sources**: files (.pdf .txt .md
  .json) in folders → async chunking into passages with embeddings → agent tools
  `open_file`, `grep_file`, `search_file`; open files were pinned into the context
  window under a size cap. **Deprecated and disabled** (2026) in favour of
  **Context Repositories**: git-tracked plain files on disk, YAML frontmatter,
  a `system/` dir always loaded, progressive disclosure by tree layout, memory
  subagents in git worktrees merging back. Lesson: Letta moved the artifact layer
  *out* of the DB and into versioned files the agent edits with normal tools.
- No documented memory → passage provenance link; the link is by the agent
  writing citations into the block text.

### 2.4 LangMem — namespaces, no artifact layer
Docs: https://langchain-ai.github.io/langmem/concepts/conceptual_guide/ ·
https://blog.langchain.com/langmem-sdk-launch/

- Semantic (collection or single profile doc), episodic (successful interaction
  exemplars), procedural (evolving prompt rules). Storage is LangGraph `BaseStore`
  with namespace tuples `("org", "{user_id}", "context")`; retrieval by key,
  semantic search, metadata filter. Raw transcripts are LangGraph checkpoints, not
  linked from memories. Useful only as a scoping model precedent.

### 2.5 Cognee — explicit document → chunk → entity provenance
Docs: https://docs.cognee.ai/core-concepts/architecture ·
https://docs.cognee.ai/core-concepts/main-operations/cognify ·
https://docs.cognee.ai/core-concepts/main-operations/search

- Three stores: **relational** (documents, chunks, provenance — "where each piece
  came from and how it's linked to the source"), **vector** (chunk + DataPoint
  embeddings), **graph** (entities/relations). `cognify` pipeline: classify →
  permissions → extract chunks → extract graph → summarize (`TextSummary` per
  chunk) → add data points → optional provenance ledger (SHA-256 chained,
  `PROVENANCE_TRACKING=true`) → optional contradiction detection.
- Default chunk size `min(embedding_max_tokens, llm_max_tokens/2)` ≈ 1k–8k tokens;
  costs 2 LLM calls per chunk.
- Search types return sources: `CHUNKS` (id, text, chunk_index), `CHUNKS_LEXICAL`
  (BM25), `SUMMARIES`, `GRAPH_COMPLETION` with `include_references=True`, etc.
  Dataset scoping via `dataset_id` when access control is on.

### 2.6 Anthropic memory tool + context editing — files, client-side
Docs: https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool ·
https://platform.claude.com/docs/en/build-with-claude/context-editing

- `memory_20250818`: a `/memories` directory the *client* implements (`view`,
  `create`, `str_replace`, `insert`, `delete`, `rename`). No embeddings, no
  structure: the document *is* the memory. Guidance: cap file sizes, page long
  files with `view_range` (tool description truncates at 16k chars), expire
  files not accessed for a long time, and enforce path traversal protection.
- Context editing (`clear_tool_uses_20250919`, `clear_thinking_20251015`, beta
  `context-management-2025-06-27`) clears old tool results server-side; the
  recommended pairing is "save to memory before results are cleared". This is the
  clearest statement of why an artifact layer exists: tool output is ephemeral,
  memory files are where it survives.
- Contextual Retrieval (https://www.anthropic.com/news/contextual-retrieval):
  prepend 50–100 tokens of LLM-written context per chunk before embedding and BM25;
  −35% retrieval failures (embeddings), −49% (+BM25), −67% (+rerank); ~$1.02 per
  million doc tokens with prompt caching; under ~200k tokens skip RAG entirely.

### 2.7 Papers (2023–2026)

- **A-MEM** (Xu et al., Feb 2025, https://arxiv.org/abs/2502.12110): Zettelkasten
  notes with content, timestamp, keywords, tags, LLM-written context, and links
  formed by similarity + LLM judgement; new notes can rewrite old notes'
  attributes ("memory evolution"). No raw-artifact tier — notes are the store.
- **HippoRAG 2** (Gutiérrez et al., Feb 2025, https://arxiv.org/abs/2502.14802):
  KG with **passage nodes and phrase nodes** in one graph; OpenIE triples, synonym
  edges; PPR seeded from both passages and triples; +7% on associative memory.
  Design point: keep raw passages *as nodes* so retrieval can land on either.
- **MemoryBank** (Zhong et al., 2023, https://arxiv.org/abs/2305.10250):
  Ebbinghaus-style decay — retention = exp(−t/S), strength S incremented on
  recall; hierarchy of raw logs → daily event summaries → global profile.
- **TierMem** (Zhu et al., Feb 2026, https://arxiv.org/abs/2602.17913, ICLR'26
  MemAgents): two tiers — fast **summary index** and **immutable raw-log store**; a
  sufficiency router escalates to raw logs only when summaries are insufficient;
  verified findings are written back as summary units *linked to their raw
  sources*. 0.851 vs 0.873 raw-only on LoCoMo with −54% tokens, −61% latency.
  Names the failure mode: the "write-before-query barrier" (compression decided
  before you know the question).
- **Eywa** (Joshi, May 2026, https://arxiv.org/abs/2605.30771): "evidence before
  belief" — immutable source evidence stored first; extracted facts validated
  against their source span before promotion to canonical; deterministic
  multi-route retrieval (facts, observations, temporal, entity scope, keyword,
  vector) with zero LLM calls. 90.19% LoCoMo, 88.2% LongMemEval-S.
- **MemTier** (Sidik & Rokach, May 2026, https://arxiv.org/abs/2605.03675):
  tiered memory with daemon-driven async consolidation; documents 14-point
  tool-success degradation over 72h from flat-file memory ("context collapse,
  compaction discontinuity, structural blindness, no attribution loop").
- Related: "From Unstructured Recall to Schema-Grounded Memory"
  (https://arxiv.org/pdf/2604.27906); "Mitigating Provenance-Role Collapse …
  via Typed Memory" (https://arxiv.org/pdf/2605.25869).

**Convergent pattern across all of the above**: a raw, immutable, content-hashed
artifact tier (episode / passage / raw log / evidence) + a distilled tier
(facts / notes / summaries) + an explicit *distilled → raw* link carrying a span,
+ retrieval that can land on either tier and fall back to raw when distilled is
insufficient. Systems that skip the raw tier (mem0 default, A-MEM, LangMem) trade
auditability for token cost; systems that skip the distilled tier (Anthropic
memory tool, Letta context repos) push distillation onto the model.

---

## 3. Design questions for agmem (Rust + SQLite + MCP)

### 3.1 Blob in SQLite vs file on disk
Sources: https://www.sqlite.org/intern-v-extern-blob.html · https://www.sqlite.org/fasterthanfs.html

- SQLite's own measurements: BLOBs < ~100 KB read faster inside the DB; > 100 KB
  faster as external files; 10 KB blobs ~35% faster than fread/fwrite and ~20%
  less disk. Page size 8192/16384 best for large blob I/O.
- Recommendation: **store in-DB up to a threshold (e.g. 256 KB–1 MB), spill larger
  bodies to a content-addressed file next to the DB** (`<store>/blobs/<hash[0..2]>/<hash>`),
  with the row holding `body BLOB NULL` xor `blob_path TEXT NULL`. One
  transactional store for the common case (notes, diffs, logs, JSON), external
  files only for PDFs/media. Keeps `agmem` single-file for backup/`consolidate`
  semantics in the normal case.
- Keep **text extraction** as a column (`text_extracted`) separate from `body`, so
  FTS/embedding work on text even when body is a PDF/binary.

### 3.2 Chunking for embedding
- Store chunks as rows (`doc_chunks(doc_id, idx, byte_start, byte_end, text,
  embedding)`) — SurrealDB, Cognee, HippoRAG 2 and Zep all treat chunks as
  first-class with spans back into the artifact. Spans (byte offsets) are what
  make `source.span`-style citation possible; store them.
- Defaults from evidence: ~400–512 tokens with 10–20% overlap is the common
  default (https://www.firecrawl.dev/blog/best-chunking-strategies-rag), though a
  Jan 2026 study found overlap gave no measurable gain on NQ. Zep recommends
  ≤ 500 chars for *graph extraction*, which is a different objective. For a
  memory store whose documents are mostly markdown/code/logs, split on structure
  first (headings, blank lines, fenced blocks), then cap by tokens.
- Cheap quality win: Anthropic's contextual retrieval (prepend a 1–2 sentence
  "where this chunk sits in the document" note before embedding/FTS). Optional,
  costs one LLM call per chunk; can be deferred to `consolidate`.
- Embed lazily/asynchronously (SurrealDB returns 202 and a `status`); a document
  should be `remember`-able and FTS-searchable before embeddings exist.

### 3.3 Dedupe by content hash
- Identity = BLAKE3 (SurrealDB) or SHA-256 (Cognee ledger, git) of raw bytes.
  BLAKE3 is fast and already a common Rust crate; either is fine. Uniqueness on
  `content_hash`; re-`remember` of same bytes returns the existing id with
  `deduplicated: true` (copy SurrealDB's response shape).
- Handle the **same bytes in two spaces**: SurrealDB widens scope on the one row.
  For agmem, where a space maps to a git dir, prefer a `documents` table keyed by
  hash + a `document_spaces(doc_id, space)` link table so a doc can belong to
  `user` and a project without duplication.
- Near-duplicate (edited file re-saved) is a *versioning* question, not dedupe:
  keep a `supersedes` link between document rows, exactly as memories do today.
  Do not mutate a hashed row.

### 3.4 Provenance memory → document
- Model on SurrealDB's `source` object and Graphiti's `episodes` field: a memory
  gets optional `source_doc_id`, `source_chunk_idx`, `source_span (start,end)`,
  plus `derived_from` for memories minted by `reflect`/`consolidate` from other
  memories. Many-to-many is rare but real (a fact supported by two docs) — a
  `memory_sources(memory_id, doc_id, chunk_idx, span)` table is safer than
  columns.
- Reverse query ("what memories came from this doc?") must be cheap: index on
  `doc_id`. This is what makes "doc superseded → flag derived facts stale" possible
  (Eywa/TierMem validation step; SurrealDB `uncertainty` on cross-source conflict).
- Return sources in `recall` output the way SurrealDB `recall` does ("source
  kinds" + refs) so the model can `inspect` a chunk instead of trusting a claim.

### 3.5 Retention / decay
- Every reviewed system decays *distilled* memories but treats *documents* as
  explicitly managed: SurrealDB documents are not in the decay schedule
  ("knowledge" ≠ uploaded manuals), Graphiti episodes are invalidated not aged,
  Anthropic suggests expiring memory files "not accessed in a long time".
- Proposal: documents get `decay_class` like memories but default `none`/`slow`;
  `fast` for session artifacts (build logs, transcripts) tagged `branch:<slug>`
  mirroring branch-state facts. A document is never auto-deleted while a live
  memory cites it (referential guard); orphaned fast-class docs are collected by
  `consolidate`. `forget` on a doc should default to *expire* (hidden, kept) with
  an explicit `purge` flag, and purge cascades to chunks and marks dependent
  memories `uncertain` rather than deleting them.

### 3.6 Size caps
- Zep: 10,000 chars per add. Anthropic memory tool: `view` truncates at 16k chars,
  page with `view_range`; docs advise capping file growth. SurrealDB: bounded by
  WS message size, otherwise "permanent size errors".
- Proposal: hard cap per document (e.g. 5–10 MB raw), text-extraction cap for
  indexing (e.g. first 1 MB), and — most important for an MCP server — a **return
  cap**: `inspect`/read returns a window (`offset`, `limit` in lines or bytes)
  with total size, never the whole body. Reject oversize at write with a clear
  error rather than truncating silently.

### 3.7 MCP: tools vs resources
Spec: https://modelcontextprotocol.io/specification/2025-06-18/server/resources ·
https://modelcontextprotocol.io/specification/2025-06-18/server/tools

- Resources are **application-controlled** (host/user decides to attach), have a
  URI, `mimeType`, optional `size`, `text` or base64 `blob`, annotations
  (`audience`, `priority`, `lastModified`), support `resources/list` (paginated),
  `resources/templates/list` (RFC 6570 URI templates), `subscribe`, `listChanged`.
  Tools are **model-controlled**. A tool result MAY contain a `resource_link`
  (`{type:"resource_link", uri, name, mimeType, annotations}`) or an embedded
  `{type:"resource", resource:{uri, mimeType, text|blob}}`; servers using embedded
  resources SHOULD declare the `resources` capability. Resource links from tools
  "are not guaranteed to appear in `resources/list`".
- Client reality: Claude Code lists `resources/list` on connect and lets the user
  attach resources with `@server:uri` mentions; the model reaches them through
  `ListMcpResources`/`ReadMcpResource` (per Claude Code MCP docs,
  https://code.claude.com/docs/en/mcp — resources section not captured in this
  fetch; verify against the current page before relying on it). SurrealDB, mem0,
  Zep ship *tools only*.
- Recommendation: **both, with tools primary.**
  1. Tools: `remember` gains a `document` variant (or new `attach`/`store_document`),
     `recall` returns `resource_link`s (`agmem://<space>/doc/<hash>#chunk=<n>`) next
     to hits, `inspect(ref)` reads a windowed slice. Tools are what the model can
     call unprompted, which is how memory gets used mid-task.
  2. Resources: expose a template `agmem://{space}/doc/{id}` and list recent/pinned
     docs so the *user* can `@`-attach a briefing artifact at session start, and so
     `lastModified`/`priority` annotations carry the doc's decay class. Do not list
     every document — resources lists should stay small; templates cover the rest.
  3. Declare `resources.listChanged` only if cheap; skip `subscribe`.

### 3.8 Scoping to a project space
- Precedents: SurrealDB Context (hard) + hierarchical Scope paths (soft, subset
  match); Graphiti `group_id`; LangMem namespace tuples; mem0 `user/agent/app/run`.
- agmem already derives a space from the shared git dir plus a reserved `user`
  space. Keep documents in the same space model: a document row is global by hash,
  membership is per-space (link table), `recall` in a project sees project ∪ user
  docs (subset-match analogue of `org/acme` visible under `org/acme/user/alice`).
  Branch-specific artifacts are a *tag* (`branch:<slug>`) with fast decay, not a
  space — the same choice already made for branch-state facts.
- Files on disk (if spilled) live under the store dir, never in the repo, so
  worktrees share them the way they share the SQLite file.

### 3.9 SQLite implementation notes
- FTS5 **external-content** table over `doc_chunks(text)` avoids duplicating text;
  `contentless_delete` if chunks are append-mostly
  (https://www.sqlite.org/fts5.html). Vectors: sqlite-vec virtual table keyed by
  chunk rowid; the documents/chunks/vec_chunks/fts_chunks four-table pattern is
  the established one (https://github.com/asg017/sqlite-vec/issues/48,
  https://arxiv.org/pdf/2604.15484 "vstash: local-first hybrid retrieval").
- Fusion: RRF with k=60 over FTS and vector ranks (SurrealDB default) is a
  two-line implementation and needs no tuning to start.

---

## Open questions to settle before building
1. Is the first document type "session artifact produced by a subagent" (notes
   files, logs) or "user-supplied reference doc" (spec, ADR)? Decay defaults and
   the tool name follow from this.
2. Embeddings: does agmem already embed memories locally? If yes, reuse for
   chunks; if not, FTS-only for docs is a legitimate v1 (SurrealDB, Zep, Cognee
   all keep BM25 as a peer signal).
3. Does purge of a document delete derived memories, mark them uncertain, or
   leave them? Every system reviewed chose "keep, but mark" except SurrealDB
   purge (deletes chunks and dependent facts).
