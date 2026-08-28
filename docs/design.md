# agmem — High-Level Design

Companion to [idea.md](idea.md), which holds the research and rationale. This
document holds the architecture: what we build, out of which parts, with which
data model, and in which order. It is the source document for backlogs, feature
descriptions, and task breakdowns.

**Decisions already made** (see idea.md §4.1 and the Q&A of 2026-08-28):
- **Rust**, single binary, MCP is the **only** surface (stdio first).
- **No server-side LLM calls, ever.** The calling agent distills; tool
  contracts are the extraction pipeline.
- **Embedded-first storage** (SurrealDB in-process via surrealkv) with a
  **connection-string escape hatch** to a user-run SurrealDB server for
  simultaneous multi-client sharing.
- **Hybrid retrieval in v1**: SurrealDB BM25 full-text + HNSW vectors with a
  local embedding model (no API keys).

---

## 1. System overview

```
┌────────────────────────┐  ┌────────────────────────┐
│  MCP client A          │  │  MCP client B          │
│  (Claude Code)         │  │  (Cursor, …)           │
└───────────┬────────────┘  └───────────┬────────────┘
            │ stdio (JSON-RPC / MCP)    │ stdio
            ▼                           ▼
┌─────────────────────────┐ ┌─────────────────────────┐
│ agmem process (per      │ │ agmem process           │
│ client)                 │ │                         │
│ ┌─────────────────────┐ │ │   Default mode: each    │
│ │ rmcp server         │ │ │   process owns its OWN  │
│ │  tools: remember,   │ │ │   embedded DB file      │
│ │  recall, context,   │ │ │   (one process per      │
│ │  forget, inspect    │ │ │   data dir, enforced    │
│ │  prompts: rituals   │ │ │   by a lock file).      │
│ └────────┬────────────┘ │ │                         │
│ ┌────────▼────────────┐ │ │   Sharing mode: both    │
│ │ core (domain)       │ │ │   processes point       │
│ │  dedup · scoring ·  │ │ │   AGMEM_DB=ws://… at a  │
│ │  supersession ·     │ │ │   user-run SurrealDB    │
│ │  context assembly   │ │ │   server — same code    │
│ └───┬────────────┬────┘ │ │   path via Surreal<Any> │
│ ┌───▼──────┐ ┌───▼────┐ │ └─────────────────────────┘
│ │ store    │ │ embed  │ │
│ │ SurrealQL│ │fastembed│ │
│ │ repo     │ │(local  │ │
│ └───┬──────┘ │ ONNX)  │ │
│     │        └────────┘ │
│ ┌───▼──────────────────┐│
│ │ SurrealDB embedded   ││   document + graph + vector
│ │ (surrealkv file)     ││   + BM25, one engine, one file
│ └──────────────────────┘│
└─────────────────────────┘
```

Key properties:

- **One process, one binary, no daemon, no ports** in the default mode. The
  MCP client spawns `agmem` over stdio; the DB is a file in the platform data
  dir. Everything Spectron does with api/worker/scheduler/management processes,
  a job queue, and an object store, agmem does in-process or lazily (§6.5).
- **stdout is the MCP wire.** All logging goes to stderr (or a file). This is
  a hard invariant enforced in the telemetry module; `println!` is forbidden.
- **Single-writer discipline.** Embedded SurrealKV has no documented
  cross-process locking, so agmem takes an advisory lock file on the data dir
  at startup (`std::fs::File` locking, stable since Rust 1.89). A second
  process gets a clean MCP-visible error telling the user to either share one
  client at a time or switch to `ws://` sharing mode.
- **Engine is a connection string.** `surrealdb::engine::any::connect` accepts
  `surrealkv://<path>` (default), `mem://` (tests), or `ws://host` (sharing /
  escape hatch). The repository layer never knows which engine it runs on.

### 1.1 What we kept from Spectron, and what it became

| Spectron | agmem |
|---|---|
| 7 verbs: remember/recall/context/reflect/forget/upload/inspect | 5 tools v1: **remember, recall, context, forget, inspect** (+ reflect as an MCP *prompt* ritual, later a persisting tool; upload dropped) |
| Server-side 3-stage LLM extraction pipeline | **The calling agent extracts**; tool descriptions + input schemas are the contract |
| Reconciler (create/update/supersede/flag, confidence floor) | Caller-driven supersession (`supersedes:` param) + server-side dedup gate (exact hash + cosine ≥ 0.95 → report duplicate instead of insert) |
| Tri-temporal (system/known/valid time) | **Bi-temporal-lite**: `created_at` (known) + `valid_from`/`invalid_at` (valid); supersede-don't-delete chains |
| 8 retrieval signals, 4-tier ladder | **2 fused signals** (BM25 + vector, RRF in one SurrealQL query) + rescoring by recency/importance; a 2-step ladder (direct lookup → hybrid) |
| 6 memory categories with per-category decay | `kind` (episode/fact/lesson/instruction) × `decay_class` (pinned/slow/normal/fast) |
| Background workers: elaboration, consolidation, decay sweeps | **No background jobs.** Decay is computed at read time; pruning runs lazily at startup; consolidation is agent-invoked (phase 3) |
| decision/retrieval/response trace graph | Provenance (`source`) on every record + supersession chains; `inspect` walks them. Full trace tables deferred |
| Grants, principals, key attenuation, delegation | Local trust model: space isolation + destructive-op flags. No auth in v1 |
| REST + Management API + SDKs + CLI/TUI | **None.** MCP tools only (a `--doctor` CLI flag for self-check is the whole CLI) |

---

## 2. Data model

One SurrealDB namespace/database (`agmem`/`main`). Isolation between projects
is the `space` field (indexed string), *not* separate databases — this keeps
hybrid search a single query and makes cross-space recall (project + user
memory together) trivial.

### 2.1 Entity-relationship picture

```
 space (registry)
   │ 1:n (by name)
   ▼
 episode ──1:n──▶ episode_chunk        [FT + HNSW indexes]
   ▲   (verbatim ground truth, append-only)
   │ source.ref (provenance link)
   │
 memory  (distilled: fact | lesson | instruction)
   │  [FT + HNSW indexes; supersession chain within table]
   ├── supersedes / superseded_by ──▶ memory      (version chain)
   ├── source: {kind, ref}          ▶ episode | external string
   ├── entities: [string]           (denormalized subject index, v1)
   └── tags: [string]

 phase 3 (schema reserved, not v1):
 entity ◀──about── memory          entity ──relates──▶ entity
        (RELATION)                        (RELATION, label, valid window)
 ```

Design stances behind this shape (evidence in idea.md §3):

1. **Episodes are ground truth, memories are an index.** Verbatim text beats
   extracted artifacts in controlled ablations; extraction is lossy. So the
   agent can store both: the distilled fact *and* the episode it came from,
   linked. Recall searches both tables and fuses.
2. **Supersede, don't delete.** A corrected fact closes with `invalid_at` and
   a `superseded_by` pointer; history stays walkable. `forget` soft-invalidates
   by default; `purge` is the explicit destructive escape.
3. **Two axes of scoping**: `space` (project/context, validated slug) and the
   built-in global space `user` for cross-project personal memory. Recall
   defaults to `current space ∪ user`.
4. **Entities start as strings.** A denormalized `entities: array<string>`
   field gives tier-1 direct lookup and filtering without an entity table.
   The full graph (entity nodes + RELATION edges) is phase 3, added only if
   multi-hop queries prove to be a real workload — per HippoRAG/Mem0 evidence,
   graphs pay off for multi-hop/temporal, cost complexity everywhere else.

### 2.2 Schema (SurrealQL, v3 syntax)

```surql
-- meta: one row; guards schema + embedder compatibility
DEFINE TABLE meta SCHEMAFULL;
DEFINE FIELD schema_version ON meta TYPE int;
DEFINE FIELD embedder_model ON meta TYPE string;        -- e.g. "bge-small-en-v1.5-q"
DEFINE FIELD embedder_dim   ON meta TYPE int;           -- e.g. 384
DEFINE FIELD created_at     ON meta TYPE datetime DEFAULT time::now();

DEFINE TABLE space SCHEMAFULL;
DEFINE FIELD name       ON space TYPE string;           -- slug, validated in Rust
DEFINE FIELD created_at ON space TYPE datetime DEFAULT time::now();
DEFINE INDEX space_name ON space COLUMNS name UNIQUE;

DEFINE ANALYZER english TOKENIZERS class FILTERS lowercase, snowball(english);

-- verbatim ground truth, append-only
DEFINE TABLE episode SCHEMAFULL;
DEFINE FIELD space        ON episode TYPE string;
DEFINE FIELD content      ON episode TYPE string;
DEFINE FIELD content_hash ON episode TYPE string;        -- blake3
DEFINE FIELD occurred_at  ON episode TYPE datetime DEFAULT time::now();
DEFINE FIELD session      ON episode TYPE option<string>;
DEFINE FIELD created_at   ON episode TYPE datetime DEFAULT time::now();
DEFINE INDEX episode_hash ON episode COLUMNS space, content_hash UNIQUE;

DEFINE TABLE episode_chunk SCHEMAFULL;
DEFINE FIELD episode   ON episode_chunk TYPE record<episode>;
DEFINE FIELD space     ON episode_chunk TYPE string;
DEFINE FIELD text      ON episode_chunk TYPE string;
DEFINE FIELD position  ON episode_chunk TYPE int;
DEFINE FIELD embedding ON episode_chunk TYPE option<array<float>>;
DEFINE INDEX ec_ft  ON episode_chunk COLUMNS text FULLTEXT ANALYZER english BM25 HIGHLIGHTS;
DEFINE INDEX ec_vec ON episode_chunk FIELDS embedding HNSW DIMENSION 384 DIST COSINE;

-- distilled, supersedable memory records
DEFINE TABLE memory SCHEMAFULL;
DEFINE FIELD space         ON memory TYPE string;
DEFINE FIELD kind          ON memory TYPE string
    ASSERT $value IN ["fact", "lesson", "instruction"];
DEFINE FIELD content       ON memory TYPE string;
DEFINE FIELD content_hash  ON memory TYPE string;        -- blake3(normalized content)
DEFINE FIELD entities      ON memory TYPE array<string> DEFAULT [];
DEFINE FIELD tags          ON memory TYPE array<string> DEFAULT [];
DEFINE FIELD embedding     ON memory TYPE option<array<float>>;
DEFINE FIELD decay_class   ON memory TYPE string DEFAULT "normal"
    ASSERT $value IN ["pinned", "slow", "normal", "fast"];
DEFINE FIELD strength      ON memory TYPE float DEFAULT 1.0;   -- Ebbinghaus S
DEFINE FIELD last_accessed ON memory TYPE datetime DEFAULT time::now();
DEFINE FIELD access_count  ON memory TYPE int DEFAULT 0;
DEFINE FIELD valid_from    ON memory TYPE datetime DEFAULT time::now();
DEFINE FIELD invalid_at    ON memory TYPE option<datetime>;    -- none = live
DEFINE FIELD invalid_reason ON memory TYPE option<string>;     -- "superseded"|"forgotten"|"expired"
DEFINE FIELD supersedes    ON memory TYPE option<record<memory>>;
DEFINE FIELD superseded_by ON memory TYPE option<record<memory>>;
DEFINE FIELD source        ON memory TYPE object;   -- { kind: "episode"|"agent"|"external", ref: option }
DEFINE FIELD created_at    ON memory TYPE datetime DEFAULT time::now();
DEFINE INDEX mem_hash     ON memory COLUMNS space, content_hash UNIQUE;
DEFINE INDEX mem_entities ON memory COLUMNS entities;
DEFINE INDEX mem_tags     ON memory COLUMNS tags;
DEFINE INDEX mem_ft  ON memory COLUMNS content FULLTEXT ANALYZER english BM25 HIGHLIGHTS;
DEFINE INDEX mem_vec ON memory FIELDS embedding HNSW DIMENSION 384 DIST COSINE;
```

Record IDs: `memory:ulid()`, `episode:ulid()` — ULIDs are temporally sortable,
which makes "recent N" range scans and stable pagination free.

Notes:
- `embedding` is `option<…>` so `--embedder none` (BM25-only degraded mode)
  and deferred embedding both work; HNSW ignores rows without vectors.
- The unique `(space, content_hash)` index is the exact-dup gate; the
  semantic near-dup gate (cosine ≥ 0.95 against top-1 neighbor) runs in Rust
  during `remember` because it needs the query-side embedding anyway.
- Dimension 384 matches the default embedder. The dimension is recorded in
  `meta`; switching embedders requires an explicit `reindex` maintenance
  operation (phase 4) — startup refuses a model/dim mismatch with a clear
  error rather than silently mixing spaces.

### 2.3 Kinds, decay classes, and lifecycle

| `kind` | Meaning | Default `decay_class` | Lifecycle |
|---|---|---|---|
| (episode table) | Verbatim record of what happened | — (no decay) | Append-only; never superseded; prunable only by `forget --purge` |
| `fact` | Distilled statement about the world/user/project | `normal` | Supersedable; decays unless recalled |
| `lesson` | Procedural insight ("X fails when Y; do Z") | `slow` | Supersedable; agent keeps these few and sharp (Reflexion evidence: bounded lessons beat accumulation) |
| `instruction` | Standing behavioral rule | `pinned` | Active until superseded/forgotten; always in `context` output |

Decay is **computed at read time** — no background sweeper exists:

```
retention(m) = exp(-Δdays(now, m.last_accessed) * rate(m.decay_class) / m.strength)

rate: pinned = 0        slow = 0.005      normal = 0.02     fast = 0.15
```

Reinforcement: when `recall` returns a memory, the server bumps
`strength += 1`, `access_count += 1`, `last_accessed = now` (batched,
fire-and-forget) — MemoryBank's Ebbinghaus model; frequently used memories
become effectively permanent, untouched ones fade in ranking. `fast` records
(working context) additionally get lazy TTL pruning: at startup, `fast`
memories with retention below ~0.05 are closed with
`invalid_reason = "expired"` (still soft — history remains).

---

## 3. MCP surface

### 3.1 Tools (v1)

Every tool description is written as an *extraction contract* — it tells the
model when to call, what good input looks like, and what it must distill
first. Descriptions are overridable via env (`AGMEM_TOOL_DESC_<TOOL>`), the
qdrant-mcp trick for steering per-deployment behavior without code changes.

| Tool | Annotations | Purpose |
|---|---|---|
| `remember` | `destructive: false, idempotent: true` | Write distilled memories and/or a verbatim episode |
| `recall` | `read_only: true, open_world: false` | Hybrid search over memories + episode chunks |
| `context` | `read_only: true, open_world: false` | Prompt-ready markdown block for session start / topic switch |
| `forget` | `destructive: true` | Soft-invalidate (default) or purge by id/query |
| `inspect` | `read_only: true, open_world: false` | Provenance, history chains, stats, health |

Input schemas (sketch; exact schemars structs are a phase-1 task):

```jsonc
// remember — batch-first; the agent distills BEFORE calling
{
  "space": "optional string (default: configured space)",
  "memories": [{
    "content": "one atomic statement, self-contained, third person",
    "kind": "fact | lesson | instruction (default fact)",
    "entities": ["Person/alice", "project-x"],      // optional
    "tags": ["identity"],                            // optional
    "decay_class": "pinned|slow|normal|fast",        // optional, defaults by kind
    "supersedes": "memory:01J…",                     // optional — caller-driven correction
    "valid_from": "RFC3339"                          // optional (when it became true)
  }],
  "episode": {                                       // optional verbatim ground truth
    "content": "…", "occurred_at": "RFC3339", "session": "…"
  }
}
// → { created: [ids], duplicates: [{id, of, similarity}], superseded: [ids] }

// recall
{
  "query": "string",
  "k": 10,                    // max 50
  "space": "current|user|all|<name>",   // default: current + user
  "kinds": ["fact","lesson"],           // optional filter
  "entities": [], "tags": [],           // optional filters (tier-1 path)
  "as_of": "RFC3339",         // optional: what was believed valid at T
  "include_invalidated": false
}
// → { hits: [{id, content, kind, space, score, signals: {ft, vec, recency},
//             valid_from, invalid_at, source, entities, tags}] }

// context
{ "query": "optional focus string", "space": "…", "budget_chars": 6000 }
// → one markdown string (see §3.2)

// forget
{ "ids": ["memory:…"], "query": "alternative to ids", "space": "…",
  "purge": false, "dry_run": false }
// → { matched: n, invalidated|purged: [ids] }   (query-mode requires dry_run first)

// inspect
{ "ref": "memory:01J… | episode:01J… | entity:<name> | stats" }
// → history chain / provenance / source episode text / per-space counts
```

Behavioral rules baked into the tools:

- `remember` returns **duplicates explicitly** (id + similarity) instead of
  silently inserting or silently skipping — the agent decides whether that
  means NOOP or `supersedes`. This is the Mem0 ADD/UPDATE/NOOP loop with the
  decision moved to the caller, which is the only place an LLM exists.
- `supersedes` sets the old record's `superseded_by`, `invalid_at = new.valid_from`,
  `invalid_reason = "superseded"` in the **same transaction** as the insert.
- `forget` by query without `dry_run: true` first is rejected — destructive
  ops confirm scope by construction, not by convention.
- Every write records `source` (episode link when the episode is provided in
  the same call, `"agent"` otherwise) — no anonymous facts (poisoning defense).

### 3.2 `context` assembly (fixed section order, budget-capped)

```markdown
# Memory context (space: <name> + user)
## Instructions        ← kind=instruction, live, all (pinned, cheap)
## Profile             ← facts tagged `identity`, live, ranked by strength
## Relevant            ← recall(query) top-k if query given, else recent high-strength facts
## Lessons             ← kind=lesson, live, top 5 by strength·recency
```

Sections are filled in that priority order until `budget_chars` is exhausted;
whole entries are dropped, never truncated mid-sentence. This is Spectron's
best idea (`context` verb) merged with its `profile` layout, minus the LLM
synthesis — pure retrieval and formatting.

### 3.3 Prompts (MCP prompts — rituals, v1.5)

- `agmem_checkpoint` — instructs the agent: review the session, distill new
  durable facts/lessons/corrections, call `remember` (with `supersedes` where
  a belief changed), confirm what was saved.
- `agmem_recall_first` — session-start ritual: call `context`, then proceed.

These carry the extraction discipline that Spectron implements as a server-side
pipeline. Resources (`memory://<space>/<id>` URIs) are a phase-4 progressive
enhancement — tools-first, since resources have uneven client support.

---

## 4. Code structure

Cargo workspace, four crates. The split isolates the two heavy/unstable
dependencies (surrealdb, ort) behind narrow trait boundaries and gives the
backlog clean seams.

```
agmem/
├── Cargo.toml                    # workspace, shared lints, release profile
│                                 # (lto=true, strip=true, codegen-units=1)
├── crates/
│   ├── agmem-core/               # pure domain — no I/O, no async
│   │   └── src/
│   │       ├── model.rs          # MemoryRecord, Episode, Kind, DecayClass,
│   │       │                     #   Source, SpaceName (validated newtype), ids
│   │       ├── scoring.rs        # retention(), fuse(), rank()  — pure fns
│   │       ├── dedup.rs          # normalize + blake3; cosine gate threshold
│   │       ├── chunk.rs          # episode chunking (~1500 chars, para-aware)
│   │       └── error.rs          # thiserror taxonomy (CoreError)
│   ├── agmem-store/              # SurrealDB repository
│   │   └── src/
│   │       ├── db.rs             # any::connect, engine selection, lockfile
│   │       ├── migrate.rs        # DEFINE-set + meta.schema_version gate,
│   │       │                     #   embedder/dim compatibility check
│   │       ├── queries.rs        # const SurrealQL strings (bound params only)
│   │       ├── repo.rs           # insert_batch, search_hybrid, direct_lookup,
│   │       │                     #   supersede, invalidate, reinforce,
│   │       │                     #   history_chain, stats, startup_prune
│   │       └── types.rs          # row structs (serde) ↔ core model mapping
│   ├── agmem-embed/              # embedding backends
│   │   └── src/
│   │       ├── lib.rs            # trait Embedder { dim(), model_id(),
│   │       │                     #   embed_passages(), embed_query() }
│   │       ├── fastembed.rs      # BGESmallENV15Q 384d, spawn_blocking wrapper,
│   │       │                     #   cache dir mgmt   [feature "onnx", default]
│   │       ├── static_m2v.rs     # model2vec potion-base-8M 256d [feature "static"]
│   │       └── noop.rs           # BM25-only mode (dim 0)
│   └── agmem-server/             # the binary: `agmem`
│       └── src/
│           ├── main.rs           # clap → config → telemetry → lock → connect
│           │                     #   → migrate → embedder → serve(stdio)
│           ├── config.rs         # flags + AGMEM_* env; --doctor self-check
│           ├── service.rs        # AgmemService { repo, embedder, cfg,
│           │                     #   tool_router, prompt_router }
│           ├── tools/
│           │   ├── remember.rs   # one file per tool: schema struct +
│           │   ├── recall.rs     #   description text + handler
│           │   ├── context.rs
│           │   ├── forget.rs
│           │   └── inspect.rs
│           ├── prompts.rs        # checkpoint / recall_first rituals
│           └── telemetry.rs      # tracing → stderr (or AGMEM_LOG_FILE)
├── docs/                         # idea.md, design.md, feature specs
└── tests/                        # workspace integration tests (see §7)
```

Dependency rule: `server → {core, store, embed}`, `store → core`,
`embed → core`. `core` depends on std + serde + blake3 + jiff only, so scoring
and dedup are unit-testable without a DB or model.

### 4.1 Libraries (verified 2026-08-28; see idea.md sources)

| Crate | Version (pin) | Role | Gotchas we design around |
|---|---|---|---|
| `rmcp` | 3.1.x | Official MCP SDK: `#[tool_router]`/`#[tool]` macros, stdio transport, prompts, annotations | 3 majors in 6 months — pin minor; supports spec 2026-07-28; params must derive `schemars::JsonSchema` **1.x** |
| `surrealdb` | 3.2.x, `default-features=false, features=["kv-surrealkv","kv-mem"]` | Embedded multi-model store | No documented cross-process lock → our lockfile; 3.0 renamed `SEARCH`→`FULLTEXT` analyzer clause; stay off 3.3 betas |
| `fastembed` | 6.x | Default embedder (BGESmallENV15Q, 384d, quantized, offline after first fetch) | Rides `ort` 2.0.0-**rc** — pin exact; sync API → `spawn_blocking`; BGE wants `passage:`/`query:` prefixes; cache via `FASTEMBED_CACHE_DIR` |
| `model2vec-rs` | 0.2.x (feature `static`) | Pure-Rust fallback embedder (potion-base-8M, 256d, ~30MB, instant cold start, ~92% of MiniLM quality) | Different dim → per-store `meta` guard |
| `tokio` | 1.53.x | Runtime (required by rmcp + surrealdb) | — |
| `serde`/`serde_json` | 1.x | Wire + rows | — |
| `schemars` | 1.2.x | Tool JSON schemas | Must be 1.x (rmcp `^1.0`); a stray 0.8 in the tree = baffling trait errors |
| `thiserror` / `anyhow` | 2.x / 1.x | Errors: thiserror in libs, anyhow in `main` | — |
| `tracing` + `tracing-subscriber` | 0.1 / 0.3 | Logging | **stderr writer only** — stdout is the protocol |
| `clap` | 4.x | Flags (derive) | — |
| `directories` | 6.x | Platform data/cache dirs | — |
| `jiff` | 0.2.x | Time (chrono is soft-deprecated for new projects) | SurrealDB datetimes round-trip as RFC 3339 strings — no chrono coupling |
| `blake3` | 1.x | Content hashing / exact dedup | — |
| `insta`, `proptest`, `tempfile` | dev | Snapshots (tool schemas!), property tests, surrealkv reopen tests | — |

Deliberately **not** used: `ulid` crate (SurrealQL `ulid()` generates IDs),
simhash/`gaoya` (blake3 exact + cosine-on-embeddings gate is simpler and
better for semantic near-dups), `figment` (clap + env is enough), any HTTP
framework (there is no HTTP).

---

## 5. Core flows

### 5.1 Startup

```
main()
 1. clap + AGMEM_* env  → Config { data_dir, db_url, space, embedder, log }
 2. telemetry: tracing → stderr / file      (before anything can fail)
 3. if db_url is embedded (surrealkv://, default):
      acquire exclusive lock file <data_dir>/agmem.lock
      └─ held by another pid → exit with MCP-friendly stderr message
         ("another agmem owns this store; close it or use ws:// sharing")
 4. any::connect(db_url); USE NS agmem DB main
 5. migrate::ensure()     — idempotent DEFINEs, meta.schema_version gate
 6. embedder init (async — model may download on very first run)
      meta.embedder_model/dim  vs  configured backend
      └─ mismatch → hard error naming the `reindex` remedy (no silent mixing)
 7. startup_prune()       — lazy TTL close of decayed `fast` records
 8. ensure space row; AgmemService.serve(stdio()); waiting()
```

### 5.2 Write path (`remember`)

```
remember(params)
 1. validate: space slug, kinds, non-empty contents, supersedes ids exist
 2. per memory: normalize content → blake3
    exact dup? (space, hash) unique index         → report as duplicate (NOOP)
 3. embed all new contents in one batch            (spawn_blocking, passage: prefix)
 4. near-dup gate: HNSW top-1 among live memories in space
    cosine ≥ 0.95                                  → report {id, similarity}; skip insert
 5. one transaction:
      episode? → insert episode + chunks (chunk.rs) + chunk embeddings
      inserts  → CREATE memory:ulid() CONTENT {...}, source.ref → episode
      supersedes? → UPDATE old SET superseded_by, invalid_at, invalid_reason
 6. → { created, duplicates, superseded }          (structured diff, Spectron-style)
```

### 5.3 Read path (`recall`)

```
recall(q)
 1. filters-only / entity-exact?  → tier-1 direct lookup (indexed WHERE, no embed)
 2. embed query (query: prefix)
 3. ONE SurrealQL round-trip (per searched table set):
      LET $ft = SELECT id, search::score(1) AS s FROM memory
                WHERE space IN $spaces AND invalid_at IS NONE AND content @1@ $q
                ORDER BY s DESC LIMIT $pool;
      LET $vs = SELECT id FROM memory
                WHERE space IN $spaces AND invalid_at IS NONE
                  AND embedding <|$pool,80|> $vec;
      -- same pair over episode_chunk; fuse all lists:
      search::rrf([$ft, $vs, $ft_ec, $vs_ec], $pool, 60)
 4. rescore in Rust (core::scoring):
      final = 0.6·norm(rrf) + 0.25·retention(m) + 0.15·importance(decay_class)
      as_of? → filter valid_from ≤ T < invalid_at (walk chains for history)
 5. take k; fire-and-forget reinforcement UPDATE (strength+1, last_accessed)
 6. → hits with per-signal scores (agent can see *why* something surfaced)
```

Pool size 64 default (`AGMEM_POOL`), k default 10 / max 50. Tier-2 semantic
response caching à la Spectron is intentionally absent — there is no
generation step to save; retrieval itself is the whole cost, and it is local.

### 5.4 `forget`

```
ids given   → resolve → soft-invalidate (invalid_reason="forgotten") | purge (DELETE + chain + chunks)
query given → dry_run required first (returns matches + count)
              second call with dry_run=false and same query executes
purge on an episode also purges chunks; purge on a memory keeps its episode
```

### 5.5 Maintenance without a scheduler

Everything Spectron runs as worker/scheduler jobs is folded into two lazy
points, keeping the process count at one:

| Spectron job | agmem equivalent |
|---|---|
| Decay sweep (importance × rate daily) | Computed in the scoring formula at read time — nothing to run |
| TTL expiry of context-category | `startup_prune()` closes decayed `fast` records |
| Consolidation / elaboration | Phase 3: `consolidate` returns *candidates* (near-dup clusters, stale contradictions); the **agent** decides merges via `remember(supersedes)` — the LLM stays client-side |
| Reindex / re-embed | Phase 4 explicit maintenance op (`agmem --reindex`), required for embedder change |
| fsck duplicate audit | Folded into `inspect stats` + `consolidate` candidates |

---

## 6. Configuration

| Flag / env | Default | Meaning |
|---|---|---|
| `--data` / `AGMEM_DATA` | `ProjectDirs("dev","agmem","agmem")` data dir | Home of DB file, lock file |
| `--db` / `AGMEM_DB` | `surrealkv://<data>/agmem.db` | Engine string; `mem://` (tests), `ws://host` (sharing mode) |
| `--space` / `AGMEM_SPACE` | `default` | Current space for this server instance (per-project value set in the client's MCP config) |
| `--embedder` / `AGMEM_EMBEDDER` | `fastembed` | `fastembed` \| `static` \| `none` |
| `AGMEM_POOL` / `AGMEM_MAX_K` | 64 / 50 | Retrieval pool and k ceiling |
| `AGMEM_TOOL_DESC_<TOOL>` | built-in | Override a tool description (steering lever) |
| `AGMEM_LOG`, `AGMEM_LOG_FILE` | `info`, stderr | Telemetry |
| `--doctor` | — | One-shot self check: lock, DB open, migrate, embedder, sample roundtrip; prints report, exits |

Client registration (the entire install story):

```jsonc
// .mcp.json / claude_desktop_config.json
{ "mcpServers": { "agmem": {
    "command": "agmem",
    "env": { "AGMEM_SPACE": "myproject" }
} } }
```

---

## 7. Testing strategy

1. **Unit (core):** scoring/retention curves, dedup normalization
   (proptest: arbitrary unicode in → no panic, idempotent), chunking bounds.
2. **Integration (store):** every test gets a throwaway `mem://` connection
   and runs the *real* migrations — FULLTEXT/HNSW syntax errors surface in CI,
   not at runtime. One `surrealkv://` test on `tempfile::tempdir()` covers
   persistence, reopen, and lockfile contention (second open must fail cleanly).
3. **Protocol (server):** in-process rmcp client over a duplex transport
   (the SDK's own test pattern) driving full tool round-trips; **insta
   snapshots of `list_tools` JSON** — the stack's most likely silent breakage
   is schema drift from rmcp/schemars upgrades.
4. **Smoke:** MCP Inspector (`npx @modelcontextprotocol/inspector agmem`)
   as a manual gate before releases.
5. **Memory-quality eval (phase 4):** a small fixture set of sessions with
   known facts/corrections, scored on recall precision and supersession
   correctness — our own workload, not LOCOMO (idea.md §3.2 on why).

---

## 8. Build order (backlog seed)

Each phase is releasable; later phases only add.

- **Phase 0 — skeleton:** repo init, workspace + crate stubs, CI (fmt, clippy,
  test), config/telemetry/lockfile, `--doctor`, `mem://` connect + migrate
  walking skeleton. *(~small)*
- **Phase 1 — the loop (MVP):** schema + migrations; embed crate (fastembed +
  none); `remember` (dedup, supersession, episodes) + `recall` (hybrid RRF +
  rescoring + reinforcement) + `inspect` (history/stats); stdio serve; protocol
  tests. **Exit criterion: usable daily from Claude Code.**
- **Phase 2 — discipline:** `context` assembly with budget; `forget`
  (dry-run flow, purge); startup pruning; prompts (`checkpoint`,
  `recall_first`); tool-description overrides; docs + install story.
- **Phase 3 — understanding:** `consolidate` candidates (agent-driven merge);
  `reflect` as persisting tool (`derived_from` provenance); entity table +
  `about`/`relates` edges + graph signal in recall *(only if multi-hop demand
  is demonstrated)*.
- **Phase 4 — polish:** `--reindex` (embedder migration); `static` embedder;
  packaging (cargo-dist, Homebrew); `memory://` resources; eval harness;
  `ws://` sharing-mode hardening docs.

## 9. Risks & open questions

1. **ort is still 2.0-rc** under fastembed — pin exact versions; the `static`
   backend is the contingency if ONNX linking breaks on a platform.
2. **SurrealKV cross-process behavior undocumented** — mitigated by the
   lockfile; revisit if SurrealDB documents multi-process embedded access.
3. **rmcp API churn** (3 majors in 2026) — pin minor, snapshot-test schemas.
4. **Will agents actually call it?** The whole system rides on tool
   descriptions + prompts out-signaling Claude Code's built-in auto memory.
   Phase 2's rituals and description-tuning are first-class work, not polish.
5. Open: exact wording of tool descriptions (deserves its own iteration);
   whether `recall` unions episodes by default or behind `include_episodes`;
   whether `user` space writes need an explicit `space: "user"` (current
   answer: yes — cross-project writes should be deliberate).
