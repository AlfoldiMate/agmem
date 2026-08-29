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
DEFINE FIELD invalid_reason ON memory TYPE option<string>
    ASSERT $value IN ["superseded", "forgotten", "expired"];    -- skipped when NONE
DEFINE FIELD supersedes    ON memory TYPE option<record<memory>>;
DEFINE FIELD superseded_by ON memory TYPE option<record<memory>>;
DEFINE FIELD source        ON memory TYPE object;   -- { kind: "episode"|"agent"|"external", ref: option }
DEFINE FIELD created_at    ON memory TYPE datetime DEFAULT time::now();
DEFINE INDEX mem_hash     ON memory COLUMNS space, content_hash UNIQUE;
DEFINE INDEX mem_entities ON memory COLUMNS entities.*;   -- .* = per element
DEFINE INDEX mem_tags     ON memory COLUMNS tags.*;
DEFINE INDEX mem_ft  ON memory COLUMNS content FULLTEXT ANALYZER english BM25 HIGHLIGHTS;
DEFINE INDEX mem_vec ON memory FIELDS embedding HNSW DIMENSION 384 DIST COSINE;
```

Record IDs: `memory:ulid()`, `episode:ulid()` — ULIDs are temporally sortable,
which makes "recent N" range scans and stable pagination free.

Notes:
- `embedding` is `option<…>` so `--embedder none` (BM25-only degraded mode)
  and deferred embedding both work; HNSW ignores rows without vectors.
- Array indexes need `COLUMNS <field>.*` and are only used by
  `field CONTAINS $x`. Without the `.*` the index covers the whole array, and
  the planner then serves `field = $x` from it — returning nothing, silently
  (verified on 3.2.4).
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
first. At #23 they were measured against real headless sessions
(`docs/tool-descriptions.md` — numbers, trace, harness), and the finding was
that a description carries the read path and not the write path: `recall` is
called unprompted, `remember` is not, and rewording `remember` to name its
trigger changed nothing because the host's own memory instructions get there
first. Write descriptions as contracts; do not expect one to win an argument
with the client's system prompt.

Descriptions are overridable via env (`AGMEM_TOOL_DESC_<TOOL>`), the qdrant-mcp
trick for steering per-deployment behavior without code changes — the whole
description, replaced at router build time, per server. A variable naming
something that is not a tool stops startup rather than being ignored, and the
override travels in the daemon handshake with `space`: the daemon belongs to
whichever session started it, so wording that stayed behind would be that
session's wording for every project sharing the store.

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
// → { created: [ids],                       // in request order, minus the duplicates
//     duplicates: [{ id, of, similarity }], // `of` = index into memories[]
//     superseded: [ids], episode: id|null } // the episode id, written or reused

// recall
{
  "query": "string",          // optional — omitted takes the tier-1 path
  "k": 10,                    // max AGMEM_MAX_K (50); refused, not clamped
  "space": "current|user|all|<name>",   // default: current + user
  "kinds": ["fact","lesson"],           // optional filter
  "entities": [], "tags": [],           // optional filters (tier-1 path)
  "as_of": "RFC3339",         // optional: what was believed valid at T
  "include_invalidated": false          // ignored when as_of is set
}
// → { spaces: [names actually searched],
//     hits: [{ id, kind: "fact|lesson|instruction|episode", content, space,
//              score, signals: { rrf, rrf_normalized, retention, importance },
//              source: "agent|episode:<id>|external:<origin>",
//              entities, tags,
//              valid_from, invalid_at, invalid_reason, superseded_by }] }
// Episode chunks compete in the same order as memories (`kind: "episode"`);
// they carry no validity window and rank on retrieval alone.

// context
{ "query": "optional focus string", "space": "…", "budget_chars": 6000 }
// → one markdown string (see §3.2)

// forget
{ "ids": ["memory:01J…", "01J…", "episode:01M…"],   // exact; no dry run needed
  "query": "alternative to ids",                     // dry_run: true required first
  "space": "…", "purge": false, "dry_run": false }
// → { spaces: [searched], dry_run, purge,
//     matched: [{ id, kind: "memory|episode", content, space,
//                 invalid_reason?, derived? }],   // derived: claims an episode leaves behind
//     invalidated: [ids],       // soft: the rows this call closed
//     purged: [ids], chunks_purged: n }
// `matched` is the list, not a count: a dry run whose answer is a number is
// not a scope anyone can check. It holds what a purge pulls in as well — the
// whole correction chain — so the blast radius is what the agent reads.
// An id already closed appears in `matched` with its `invalid_reason` and is
// absent from `invalidated`: a forget never rewrites another close.

// inspect
{ "ref": "memory:01J… | 01J… | episode:01J… | entity:<name> | stats",
  "space": "current|user|all|<name>" }   // default: current + user; all for stats
// → { ref: canonical form, spaces: [searched],
//     found: one of
//       { kind: "memory",  memory, chain: [oldest→newest], episode? }
//       { kind: "episode", episode, chunks: [reading order], derived: [claims] }
//       { kind: "entity",  entity, memories: [live and closed] }
//       { kind: "stats",   counts: [{ space, memories, live, invalidated,
//                                     episodes, chunks, live_by_kind }] } }
// A bare ULID resolves against memory, then episode_chunk, then episode —
// that is the form `remember` and `recall` hand out, and a verbatim hit's id
// is a *chunk* id, so requiring a prefix made the obvious call fail (#36).
// A chunk answers as the episode it belongs to; the echoed `ref` says which.
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

What the sketch leaves out, settled while building it (#19):

- **Every line ends with its memory id**, in backticks. 26 characters of the
  budget per entry buys a block an agent can act on: a stale claim goes to
  `remember`'s `supersedes` or to `inspect` without a `recall` in between.
- **A claim appears once.** The sections are filled in order and an id already
  written is skipped, so an identity fact that is also the best match for the
  query stays in Profile.
- **A heading is charged to the first entry under it that fits**, so a section
  the store had nothing for — or one the budget ate whole — leaves nothing
  behind. An entry too long for what is left is skipped rather than ending the
  section; the next one may fit.
- **Verbatim episode chunks are excluded** from the Relevant search (the
  store's `Search::episodes`). A chunk runs to ~1500 chars, so one would take a
  quarter of the default budget and the Lessons section with it. The block is a
  briefing; `recall` is the route to the text.
- **`context` reinforces nothing.** It is called on a schedule rather than
  because something was needed, so counting it as use would flatten every decay
  curve to permanent within a few sessions.
- The non-search sections rank through `core::scoring::rank` on a pool with no
  retrieval score — retention and importance — which is what §3.2's "by
  strength" and "by strength·recency" reduce to, since strength *is* the
  stability term of the retention curve.
- `budget_chars` below 200 is refused: a block too small for a heading and one
  claim comes back as a bare title, which reads exactly like an empty store.
- It answers with **text content, not `Json<T>`**: the payload is markdown
  meant for the prompt, and the `Json` wrapper puts the JSON serialisation in
  `content`, so every client would show the model an escaped string.

### 3.3 Prompts (MCP prompts — rituals)

- `recall_first` — session-start: call `context`, read the block as
  established fact rather than as a suggestion, `recall` what it does not
  cover, and correct what it gets wrong instead of working around it.
- `checkpoint` — end of session: review the conversation for what is durable,
  **`recall` each candidate before writing it** to find the id it corrects,
  then one batched `remember` with `supersedes` set on the corrections, then
  say what was saved and what was left out.

Both take one optional `focus` argument — free text, because that is how a
client renders a prompt argument (Claude Code passes whatever follows the
slash command), and a ritual that needs configuring is one nobody runs.

Neither touches the store. A ritual returns text about which tools to call in
which order; running them is the agent's turn, which is what keeps the
extraction discipline Spectron implements as a server-side pipeline on this
side of the "no server-side LLM" line.

**Why this is a prompt and not a longer description.** #23 measured the
difference: a tool description is read while the model is choosing between
options and loses that choice to whatever the host already put in its system
prompt, while a prompt arrives as a turn in the conversation because somebody
asked for it. The two rituals therefore carry exactly the instructions the
descriptions could not make stick — `checkpoint` step 2 is "look before you
correct", which `remember`'s description states in as many words and which was
skipped in 6 of 6 measured runs (§9 item 6, issue #38).

Naming: the prompts are `checkpoint` and `recall_first`, not
`agmem_checkpoint` / `agmem_recall_first` as this section originally said. A
client that shows prompts scopes them by server — Claude Code renders them
`/mcp__agmem__checkpoint` — so the prefix only stutters.

Resources (`memory://<space>/<id>` URIs) are a phase-4 progressive
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
│           ├── main.rs           # clap → config → telemetry → one of three
│           │                     #   routes: be the daemon, attach to it,
│           │                     #   or own the store here
│           ├── daemon/           # the shared store (#37), unix only
│           │   ├── mod.rs        #   socket path, handshake, wanted()
│           │   ├── serve.rs      #   owns the store, one service per session
│           │   └── client.rs     #   find or start it, then pump stdio
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
 3. route:
    a. unix + embedded + not --no-daemon → attach to the shared daemon (#37):
         connect <data_dir>/agmem.sock
         └─ nothing there → take <data_dir>/agmem.spawn.lock, clear a stale
            socket, spawn `argv[0] --daemon-serve` in its own process group,
            wait for it to accept
         send one JSON handshake line
            { version, db_url, embedder, space, pool, max_k }
         then pump stdio ↔ socket, and decide nothing else
         └─ unreachable → stderr naming <data_dir>/daemon.log, exit 1. Never
            fall back to opening the store: that is the second writer.
    b. --no-daemon, ws://, mem://, or a non-unix platform → steps 4–9 here,
       holding the lock for this process lifetime.

 the daemon (--daemon-serve) and route (b) both then:
 4. acquire exclusive lock file <data_dir>/agmem.lock (embedded engines only)
      └─ held by another pid → exit with MCP-friendly stderr message
         ("that pid is usually the daemon; drop --no-daemon, or use ws://")
 5. any::connect(db_url); USE NS agmem DB main
 6. migrate::ensure()     — idempotent DEFINEs, meta.schema_version gate
 7. embedder init (async — model may download on very first run)
      meta.embedder_model/dim  vs  configured backend
      └─ mismatch → hard error naming the `reindex` remedy (no silent mixing)
 8. repo::prune_expired() — lazy TTL close of decayed `fast` records, every
      space at once, and never fatal: the schema and the embedder are up, so a
      failed sweep is logged and the session is served anyway
 9. ensure space row; AgmemService.serve(transport); waiting()

 The daemon binds its socket only after 4–8 have succeeded, so one that dies
 at migrate cannot invite every session to respawn it forever. Step 9 is per
 *connection* there: the store is shared and the space is not, so each session
 gets its own Config and its own AgmemService over the one Db and embedder —
 which is also where `ensure space row` has to happen, since the daemon has
 never heard of a project until one attaches.
```

### 5.2 Write path (`remember`)

```
remember(params)
 1. validate: space slug, kinds, non-empty contents, supersedes ids exist
 2. per memory: normalize content → blake3
    exact dup? (space, hash) unique index         → report as duplicate (NOOP)
    …but the check runs *inside* the transaction of step 5: a unique-index
    conflict aborts the whole transaction, so a duplicate must never reach it
 3. embed all new contents + episode chunks in one batch
                                                   (spawn_blocking, passage: prefix)
 4. near-dup gate: HNSW top-1 among live memories in space
    cosine ≥ 0.95                                  → report {id, similarity}; skip insert
    skipped when the memory carries `supersedes` — the agent has already made
    the ADD/UPDATE call, and a correction usually *is* close to what it corrects
 5. one transaction:
      episode? → insert episode + chunks (chunk.rs) + chunk embeddings
      inserts  → CREATE memory:ulid() CONTENT {...}, source.ref → episode
      supersedes? → UPDATE old SET superseded_by, invalid_at, invalid_reason
 6. → { created, duplicates, superseded, episode } (structured diff, Spectron-style)
```

### 5.3 Read path (`recall`)

```
recall(q)
 1. filters-only / entity-exact?  → tier-1 direct lookup (indexed WHERE, no embed)
 2. embed query (query: prefix)
 3. ONE SurrealQL round-trip (per searched table set):
      -- one match reference per query word, OR'd, scores summed (issue #39)
      LET $ft = (SELECT id, search::score(1) + search::score(2) AS s FROM memory
                 WHERE space IN $spaces AND invalid_at IS NONE
                   AND (content @1@ $t0 OR content @2@ $t1)
                 ORDER BY s DESC LIMIT 64);
      -- the scan runs bare and its result is filtered: no conjunct may ride
      -- the KNN operator (issue #40), so K is over-fetched 4x and re-capped
      LET $vs = (SELECT id, d FROM
                   (SELECT id, space, kind, entities, tags, valid_from,
                           invalid_at, vector::distance::knn() AS d
                    FROM memory WHERE embedding <|256,80|> $vec)
                 WHERE space IN $spaces AND invalid_at IS NONE
                 ORDER BY d LIMIT 64);
      -- same pair over episode_chunk; fuse all lists:
      LET $fused = search::rrf([$ft, $vs, $ft_ec, $vs_ec], 64, 60);
      -- then project the survivors by id, in the same request
 4. rescore in Rust (core::scoring):
      final = 0.6·norm(rrf) + 0.25·retention(m) + 0.15·importance(decay_class)
      norm  = min–max over the pool: (rrf − min) / (max − min)
      as_of? → filter valid_from ≤ T < invalid_at (walk chains for history)
 5. take k; fire-and-forget reinforcement UPDATE (strength+1, last_accessed)
 6. → hits with per-signal scores (agent can see *why* something surfaced)
```

Pool size 64 default (`AGMEM_POOL`), k default 10 / max 50. Tier-2 semantic
response caching à la Spectron is intentionally absent — there is no
generation step to save; retrieval itself is the whole cost, and it is local.

`norm` is **min–max, not max-only** (issue #34). RRF barely spreads —
`1/(60 + rank)` differs by 3% between the first hit and the fourth — so
dividing by the best candidate left the 0.6 retrieval term varying by 0.02
across a pool while the 0.15 importance term varied by 0.075, and decay class
decided every order. Min–max gives the retrieval signal the range its weight
implies; the price is that the pool's weakest candidate scores zero on it. A
pool where nothing was retrieved (the tier-1 path) normalises to 0 throughout;
one where every candidate tied, to 1.

Five engine details the sketch has to obey (verified on 3.2, issues #13, #39,
#40):

- **`@N@` ANDs the words inside one reference.** `content @1@ 'who formats
  python'` matches only rows holding all three, so a question — which always
  carries a word the answer does not — took the fulltext arm to empty on
  nearly every real call. The fix is a disjunction of one reference per word,
  `content @1@ $t0 OR content @2@ $t1 …`, scored by the sum: a reference that
  did not match contributes 0, so the sum ranks by how much of the question a
  row answered. Terms are lower-cased and split on non-alphanumerics (the
  index's `class` tokenizer would split `don't` itself and then AND the
  halves), deduplicated, and capped at 12 — a question is a handful of words,
  and a pasted paragraph is not a question. No stop-word list: a row matching
  only `the` scores near zero in a pool that exists to be rescored.
- **No conjunct may ride the KNN operator.** A `KnnScan` on a cold index that
  carries a predicate emits fewer rows than the same scan without one — a bare
  `1 = 1` is enough, and one *unfiltered* scan repairs every filtered scan
  after it for that connection's life. agmem's arms all carry `space` and
  liveness, so nothing ever warmed them and every recall a process served came
  back short. Each vector arm therefore scans bare in a subquery and filters
  its result. The cost is that candidates spent on other spaces and superseded
  rows no longer count toward the pool, which a 4× over-fetch buys back:
  measured on 384 rows across two spaces, a full 64 of 64 where a bare `K`
  gave 48 — and *faster* than the pushed-down form it replaces, ~72 ms against
  ~112 ms. Same treatment for the near-dup gate (§5.2 step 4), whose `K` of 1
  had the same predicate riding it.
- The KNN operator's `K` must be an **integer literal** — `<|$pool,80|>` is a
  parse error — so the pool is formatted into the query text and clamped there.
- `ORDER BY` may only name an idiom the projection carries, which is why each
  arm selects the column it sorts on (`s`, `d`).
- `search::rrf` fuses by **rank**, returning each input object with an added
  `rrf_score`; lists from different tables merge cleanly, so memories and
  episode chunks compete in one fused order.

Entity and tag filters use `CONTAINSANY`, which the `entities.*` / `tags.*`
indexes serve as a `UnionIndexScan` (one `IndexScan` per value). An empty
filter list must be **omitted from the query**, not bound: `CONTAINSANY []`
matches nothing.

### 5.4 `forget`

```
ids given   → resolve in each searched space → soft-invalidate (invalid_reason="forgotten")
                                                  | purge (DELETE + chain + chunks)
query given → BM25 only, memories only → dry_run required first (returns the matches)
              an identical second call with dry_run=false executes, and spends the confirmation
purge on an episode also purges chunks; purge on a memory keeps its episode
```


Five rules, each of them the answer to "what could this destroy that nobody
asked it to":

- **Soft is the default and the reversible state.** A forgotten memory is
  closed exactly as a superseded one is, so `inspect` still reads it, dated and
  labelled. A memory a correction already closed is left alone: `invalid_at IS
  NONE` guards the update, so a forget never rewrites another close and never
  moves a claim's date.
- **A purge takes the whole supersession chain.** Deleting only the newest
  wording of a claim would leave its earlier wordings readable, which is the
  opposite of what "delete this" means. The dry run lists the chain, so the
  blast radius is what the agent reads before it decides.
- **Query mode is BM25 only — no vector arm.** KNN returns its nearest
  neighbours however far away they are, which on a small store is the whole
  store; as a selector for deletion that is wrong in the unrecoverable
  direction. A row that does not contain the words does not match at all.
- **The confirmation is per session and spent on use.** It lives on the
  `AgmemService`, which the daemon builds per connection, so one agent's dry
  run cannot authorise another's delete. `purge` is part of the scope:
  previewing what would be closed does not authorise deleting the same rows.
- **Verbatim text can only be purged.** An episode has no validity window, so
  there is no soft state for it to enter; asking to close one is refused rather
  than silently ignored. Its chunk ids are refused too — a slice is not a thing
  anyone forgets — and the claims distilled from it survive it, still naming
  the source. `inspect` therefore treats a `source` naming no episode as
  history rather than as a broken store.

### 5.5 Maintenance without a scheduler

Everything Spectron runs as worker/scheduler jobs is folded into two lazy
points, keeping the process count at one:

| Spectron job | agmem equivalent |
|---|---|
| Decay sweep (importance × rate daily) | Computed in the scoring formula at read time — nothing to run |
| TTL expiry of context-category | `repo::prune_expired` closes decayed `fast` records at every start |
| Consolidation / elaboration | Phase 3: `consolidate` returns *candidates* (near-dup clusters, stale contradictions); the **agent** decides merges via `remember(supersedes)` — the LLM stays client-side |
| Reindex / re-embed | Phase 4 explicit maintenance op (`agmem --reindex`), required for embedder change |
| fsck duplicate audit | Folded into `inspect stats` + `consolidate` candidates |

The prune is one `UPDATE`, and the decay curve is not repeated in it. The
selector is the **inverse** of the retention formula, computed once in
`core::scoring::decay_horizon_secs`: the idle time at which unit strength
falls to 0.05 — about twenty days for `fast` — which the engine then scales by
each row's own `strength`. So the comparison is
`last_accessed + horizon·strength < now`, written forwards on purpose:
SurrealDB durations are unsigned, and `now − last_accessed` on a row with a
future timestamp either errors the statement or comes back large and positive,
expiring exactly the row that has barely aged.

Three properties the sweep is designed around:

- **Only `fast` expires.** It is the TTL class by construction; a `normal`
  fact 400 days old ranks near zero and stays live, because a fact's end is a
  correction or a `forget`, not a timeout.
- **Reinforcement buys time.** Strength scales the horizon, so a working note
  recalled five times survives five times as long — the same reinforcement
  that flattens its decay curve at read time.
- **Soft, and idempotent.** `invalid_reason = "expired"`, chain intact,
  readable through `inspect`; `invalid_at IS NONE` guards the update, so a
  second start moves no date and a row a `forget` already closed keeps its own
  reason.

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
| `AGMEM_LOG`, `AGMEM_LOG_FILE` | `warn` + agmem crates at `info`, stderr | Telemetry |
| `--no-daemon` / `AGMEM_NO_DAEMON` | off | Own the store in this process instead of through the shared daemon (#37) |
| `--idle-timeout` / `AGMEM_IDLE_TIMEOUT` | 600 | Seconds the daemon outlives its last session; 0 keeps it until reboot |
| `--daemon-serve` | — | Be the daemon. Started automatically; hidden from `--help` |
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
   **Confirmed a real limit at #18**: Claude Code runs one stdio server per
   session and the data dir is shared by every project, so the second
   concurrent session gets no memory tools at all. One store per machine is
   the design (`space: "all"`, the shared `user` space), so the answer is a
   shared endpoint rather than per-project data dirs — issue #37, **closed**:
   one process owns the store and the rest attach over a unix socket carrying
   MCP (§5.1 step 3a). surrealdb ships clients only, so the shared endpoint is
   agmem's own rather than SurrealDB's.
3. **rmcp API churn** (3 majors in 2026) — pin minor, snapshot-test schemas.
4. **Will agents actually call it?** The whole system rides on tool
   descriptions + prompts out-signaling Claude Code's built-in auto memory.
   Phase 2's rituals and description-tuning are first-class work, not polish.
   **Measured at #23** and the answer splits in two: reading is reflexive
   (`recall` called 3/3 before answering, and correctly *not* called 3/3 on a
   question memory has nothing to say about), writing does not happen at all
   (`remember` 0/3 on a stated convention, every session replying that it had
   saved it). See item 5. **Closed at #22 for the write half**: the identical
   turn followed by `/mcp__agmem__checkpoint` writes 3/3. A description cannot
   win the write path and a ritual can, because a ritual is asked for rather
   than chosen.
5. Settled at #23, against the hypothesis: **the write path is not a wording
   problem.** `remember` was reworded to open with its trigger, the way the two
   tools that *were* called do, and measured again over 12 more sessions: no
   change, in either direction. Tracing every tool call rather than agmem's
   showed why — the agent did save the convention, with `Write`, into Claude
   Code's own auto-memory directory, which the client names in the system
   prompt before any tool description is read. Turning that off
   (`autoMemoryEnabled: false`) takes both `store` and `correct` to 3/3 — and
   so does the *old* wording under the same conditions, served through
   `AGMEM_TOOL_DESC_*`. The description was never being rejected; it was never
   being read against a live choice. So the write path needs a mechanism (§3.3
   prompts as rituals, issue #22) rather than better words, and the reworded
   text ships on the §3.1 contract ("say when to call") rather than on
   evidence. `docs/tool-descriptions.md` carries the numbers, the trace and the
   harness (`scripts/desc-eval.nu`) — any future wording change goes through it
   first.
6. Open, found at #23: **a correction is stored as a new claim.** In 6 of 6
   isolated runs, told that a seeded fact no longer holds, the agent called
   `remember` with a fresh memory and never `recall`ed for the id or set
   `supersedes` — leaving both claims live at once, which is the state the
   chain exists to prevent.
   **Mostly resolved by #39.** Measuring it again at #22 showed the agent under
   the `checkpoint` ritual *does* look — and was getting an empty answer for a
   claim that was live in the store, because the fulltext arm ANDed the words
   of the question (item 7). With that fixed, `ritual_correct` supersedes 3/3.
   What is left is the path with no ritual, where the agent never looks:
   **issue #38** — have `remember` return contradiction candidates the way it
   already returns near-duplicates — is now a convenience for that path rather
   than a correctness gap.
7. Found at #22, diagnosed as **two faults** at #39. First, **`@N@` ANDs its
   terms**, so the fulltext half of "hybrid BM25 + vector" returned nothing for
   any question carrying a word the stored claim does not — which is every
   question, and is the query style `recall`'s own description asks for. Fixed
   at #39 (one match reference per word, OR'd, scores summed; §5.3). Second,
   **`KnnScan` under-returns when a predicate is pushed into it** — `k: 64`, two
   matching rows, one emitted, and any conjunct triggers it including `1 = 1`.
   The first masked the second, which is why the symptom looked like one thing.
   Closed at #40: the state is per **connection**, not per query — one
   *unfiltered* scan repairs every filtered scan after it, repeating the
   filtered scan does not, and agmem's arms always carried a filter, so no
   agmem process ever warmed itself. That is also why it read as
   unreproducible four times: any probe that measures the unfiltered arm first
   has already destroyed what it came to observe. The arms now scan bare and
   filter outside (§5.3); it is confirmed against surrealdb 3.2.4 and worth
   reporting upstream.
8. Still open: whether `user` space writes need an explicit `space: "user"`
   (current answer: yes — cross-project writes should be deliberate).
9. Settled at #16: **`recall` unions episodes by default**, with no
   `include_episodes` flag. The §5.3 flow already fuses `$ft_ec`/`$vs_ec` into
   the same pool, distillation is lossy by design, and a chunk that outranks
   every memory is exactly the case the verbatim copy exists for. `kind:
   "episode"` on the hit is what makes the two distinguishable, so the flag
   would buy nothing a filter cannot.
10. Found at #24, writing the install docs: **an exact duplicate does not report
    `1.0`.** The near-duplicate vector gate runs before the transaction whose
    content-hash lookup produces `Written::Duplicate`, so verbatim text is
    always caught by the gate first, at the f32 self-similarity of its own
    embedding (0.9999998). The `similarity: 1.0` branch is only reached when
    there is no embedding to gate on — `--embedder none` — so the same input
    reports different numbers under the two backends. §3.1's wording and the
    tool schema both still claim 1.0; the README no longer does. **Issue #41**
    carries the choice: hash-check first, or document the cosine.
