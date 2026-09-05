# agmem design review — architecture findings

Reviewed at main (4095523), all four crates read in full. Every finding carries
file:line evidence and a concrete failure scenario. Ranked by severity.

---

## HIGH

### H1. The exact-hash duplicate gate ignores liveness — a closed claim's content can never live again, and the documented retry corrupts the chain

The in-transaction duplicate lookup matches **any** row with the hash, closed
rows included:

- `crates/agmem-store/src/queries/write.rs:107-108` — `SELECT VALUE id FROM
  memory WHERE space = $space AND content_hash = $hash{index} LIMIT 1` — no
  `invalid_at IS NONE`.
- The vector gate *does* filter live-only (`crates/agmem-store/src/queries/read.rs:364`),
  so only the hash path reaches dead rows.

**Scenario A (silent loss):** claim X stored as row A; A soft-forgotten (or
superseded). Later the agent legitimately re-learns X and calls `remember`.
The vector gate compares against live rows only, passes; the transaction finds
closed A → `Duplicate(A)`, nothing written. The tool renders it with
`similarity: 1.0` and the caller's own text (`remember.rs:326-332`) — nothing
signals that A is dead. The agent reports "already noted"; `recall` returns
nothing. Because the unique index is `(space, content_hash)` with no liveness
qualifier (`v1_schema.surql`, `mem_hash`), that exact content is permanently
unwritable in the space.

**Scenario B (cycle / zero live rows via the documented retry):** X stored as
A; corrected by Y (row B, `supersedes: [A]`) → A.superseded_by = B. Later Y
turns out wrong; agent re-sends content X with `supersedes: [B]` — exactly
what the tool description instructs ("re-send yours with the id here in
`supersedes`"). Gate skipped (supersedes non-empty, `remember.rs:229-234`);
transaction: `$dup` = A (closed); dup branch
(`queries/write.rs:96-104`) computes `$keep = [B]` and closes B in favour of
A. Result: A.superseded_by = B **and** B.superseded_by = A — a supersession
cycle — and **neither row is live**. The agent is told
`Duplicate(A), superseded: [B]` and reads it as success. `recall` then returns
neither claim; `inspect`'s history walk on either row produces duplicate ids
(the `{..64+collect}` walk revisits the cycle) which the pairing in
`crates/agmem-store/src/repo/read.rs:581-595` turns into a hard
`UnexpectedResponse` ("the walk named a link it did not return") — or at best
a corrupted chain. `forget --purge` also fails there
(`forget.rs:402-426` goes through `history_chain`), so the broken pair cannot
even be purged by id.

The issue-#57 test (`agmem-store/tests/writes.rs:683-739`) only covers the dup
row being **live**; the closed-dup case is untested and broken.

Fix direction: dup lookup filters `invalid_at IS NONE` (letting a re-assert
create a fresh row — the unique index must then also be scoped or the hash
salted with the closure), or the dup branch refuses/resurrects when `$dup` is
closed, and `Duplicate` carries the row's liveness so the agent can see it.

### H2. Daemon handshake refusal — including version skew — is invisible to the session; the session exits 0 having served nothing

The handshake is fire-and-forget: the client writes one line and never reads a
response (`crates/agmem-server/src/daemon/client.rs:51-64`). When the daemon
refuses (`Handshake::accept`, `daemon/mod.rs:143-170` — version, db_url, or
embedder mismatch), the error is only **logged in the daemon's own log**
(`daemon/serve.rs:73-74,130`); the socket closes; the client's `pump` treats
EOF as a normal end and returns `Ok(())` → **exit code 0**
(`client.rs:198-217`). The test enshrines the bare close
(`agmem-server/tests/daemon.rs:214-234`).

The doc comment claims the opposite: "The message is the session's to read, so
it names both sides" (`daemon/mod.rs:141-142`). It never travels.

**Scenario:** agmem is upgraded; `PROTOCOL_VERSION` bumps to 3. A long-lived
v2 daemon (idle_timeout 0, or with an old session still attached) holds the
socket. Every new session connects, is refused, and dies silently with exit 0
— the MCP client shows a server that closed before `initialize`, with no
reason anywhere the user looks. There is no auto-restart of an outdated
daemon (the refusal message says "stop it and let this one start a fresh
daemon", but nobody sees it), and the client never falls back or retries.
`agmem context` (the SessionStart hook) fails the same way with only
"initializing MCP with the shared store" (`oneshot.rs:64-65`).

Fix direction: a one-line JSON ack after the handshake (the read is already
buffered daemon-side), and on version-skew refusal the client should offer —
or perform — the restart.

---

## MEDIUM

### M1. Supersession has no liveness guard — closing an already-closed row rewrites its history, and `as_of` can then see two live claims at once

`FORGET_SOFT` carefully guards `invalid_at IS NONE` "to keep a forget from
rewriting history" (`queries/write.rs:162-165`). The two supersede paths do
not:

- batch created-branch: `UPDATE $old{index} SET superseded_by = …,
  invalid_at = …` (`queries/write.rs:90-95`) — unconditional;
- standalone `SUPERSEDE` (`queries/write.rs:147-153`) — unconditional;
- `ensure_memories_exist` (`repo/mod.rs:38-63`) checks existence, not liveness.

**Scenario:** consolidate offers cluster [A,B,C]; meanwhile another session
(shared daemon) supersedes B with D (B.invalid_at = t1, superseded_by = D).
The merge `remember(W, supersedes: [A,B,C])` lands after: B.superseded_by is
overwritten to W and B.invalid_at moves to t2. B's original closure boundary
is gone; D still lists B in `supersedes`; the chain forks inconsistently. Any
`as_of` between t1 and t2 now returns **both** B's successor-at-the-time (D)
and B itself as live — violating "only one claim is live at a time" for
historical reads (`queries/read.rs:480-482` window logic assumes tiling).

### M2. Reinforcement fires on `as_of` and `include_invalidated` reads

`recall` collects every memory hit it returns and reinforces them regardless
of liveness mode (`crates/agmem-server/src/tools/recall.rs:317-324`; no check
of `liveness`). `REINFORCE` bumps strength/access_count and resets
`last_accessed` (`queries/read.rs:381-384`).

**Scenario:** an audit session asks "what did we believe in March"
(`as_of`) across a `fast`-heavy space. Every live row on the page gets
`strength += 1` and a fresh `last_accessed`, extending its prune horizon —
historical reading keeps working notes alive, which is precisely the drift
`context` refuses to cause for the same reason (`context.rs:11-14` documents
"called on a schedule, not because something was needed" — an as_of audit is
the same shape). Closed rows also get counters mutated, polluting
`access_count`'s meaning as an audit signal in `inspect`.

### M3. Concurrent identical writes through the daemon abort the whole batch with a raw DB error

The guarded-insert design ("an exact duplicate can never be allowed to reach
the index", `queries/write.rs:5-9`) holds only serially. Two sessions of the
shared daemon writing the same content concurrently: both transactions read
`$dup = NONE`, both `CREATE`, one hits the unique `mem_hash` index → its
**entire** transaction aborts — including the episode and every *other*
memory in that batch — surfacing as `INTERNAL_ERROR "database error: …"`
(`tools/mod.rs:213-221`) instead of the promised `Duplicate` outcome.
Evidence: gate outside the transaction (`remember.rs:239`), guard-then-create
race inside it (`repo/write.rs:189-311`). Low frequency, but the failure mode
is the worst available one (whole-batch loss + unreadable error), and nothing
retries.

Related, same root: the near-dup gate runs pre-transaction, so two sessions
writing *near*-identical (not exact) claims concurrently both pass and both
land — acknowledged as consolidate's job (`dedup.rs:76-86`), cleared — but the
exact-hash race is not acknowledged anywhere.

### M4. Read/write space-keyword asymmetry: `remember(space: "current")` creates a literal space named `current`

Reads treat `current`/`all` as keywords (`tools/mod.rs:100-120`); writes
(`resolve_space`, `tools/mod.rs:152-168`) accept any valid slug — and
`SpaceName` accepts `"all"` and `"current"` (`agmem-core/src/model.rs:39-51`).

**Scenario:** an agent that learned the vocabulary from `recall`'s schema
("`current` for this project…") symmetrically passes `space: "current"` to
`remember`. The write is silently registered and stored in a new space
literally named `current` (`resolve_space` even calls `ensure_space` for it).
`recall(space: "current")` then resolves to the *project* space and never
finds it; the rows only surface under `space: "all"`. A write landed somewhere
the caller didn't intend, with no error. Cheap fix: refuse the two reserved
keywords in `resolve_space`.

### M5. Forget-by-query confirms the *scope*, not the *result set* — and OR-term matching makes the scope enormous

The two-call gate compares only `(query, spaces, purge)` (`forget.rs:139-175`).
The executing call **re-runs the search** (`forget.rs:199-215`) and acts on
whatever matches *now*. Under the shared daemon, rows written between dry-run
and confirm are deleted without ever being previewed — the exact guarantee the
dry run exists to give ("read exactly what matched, then send the identical
call").

Compounding it: `by_query` → `search_hybrid` ORs its terms with no stop-word
list (`queries/read.rs:129-163`), and forget acts on **every** candidate in
the pool, not the top-scored. A query like "the old docker notes" matches every
row containing "the" or "old", up to `pool` (default 64) rows across
`current`+`user`. The dry run does show the list, but the design claim ("a
deletion should never reach something that merely resembles what you asked
for", tool description) is not what BM25-OR delivers. Fix direction: confirm
the matched id-set hash, and/or AND-with-quorum semantics for the forget arm.

### M6. One global embedding mutex + unbounded single-batch embeds — a big `remember` starves every session on the daemon

`FastembedBackend` holds one `Mutex<TextEmbedding>` for the process
(`agmem-embed/src/fastembed.rs:30-35`), and `remember` embeds all claims plus
all episode chunks in **one** call (`remember.rs:413-452`). Nothing caps
episode size, chunk count, memory count, or content length
(`remember.rs:355-406` validates only non-emptiness).

**Scenario:** one session remembers a 5 MB session log → ~3,500 chunks
(`chunk.rs`, 1500 chars each) → one `model.embed()` call holding the model
lock for minutes on CPU. Every other session's `recall` queues behind it
(`embed_query` → same mutex) — the shared daemon serializes all memory
operations behind one write. Same path allows a single multi-megabyte "claim"
that is then returned whole in every recall page and context assembly
(context drops it by budget; recall does not). Reindex batches at 128
(`server/reindex.rs:26`) — the interactive path deserves the same courtesy.

---

## LOW

### L1. Min–max RRF normalisation over-amplifies in tiny pools

`rank` stretches the pool to [0,1] (`agmem-core/src/scoring.rs:245-267`).
With 2 candidates whose raw RRF differ by 1e-6, the better one gets the full
0.6 and the other 0.0; retention+importance (≤0.4 combined) can never flip the
order. The exact inverse of issue #34: in a small store, a pinned instruction
that placed one rank behind a scratch note in one arm is guaranteed to lose.
Division-by-zero and all-tied cases are handled correctly
(`scoring.rs:248-253`); the amplification is the residual wart.

### L2. Consolidate buffers O(n²) contradiction structs before truncating to 20

Every pair ≥ 0.75 with a shared entity is pushed — with two cloned
`MemoryView`s including full content — before `truncate(MAX_CONTRADICTIONS)`
(`consolidate.rs:281-291` vs `:227-228`). At MAX_POOL=1000 rows all tagged
with one hub entity, that is up to ~500k structs of cloned strings in memory
for an answer that keeps 20. Also: `compare`'s doc comment claims "the bands
do not overlap, so no pair is ever reported under both names"
(`consolidate.rs:249-253`) — contradicted by the code two screens down and by
`dedup.rs:104-114`, which say the overlap is deliberate. The `truncated` flag
is also a false positive at exactly 1000 live rows (`consolidate.rs:202`).
The O(n²) pass itself is fine (bounded, correct truncated-rows semantics:
weakest-by-strength are the ones not compared, `queries/read.rs:429-441`).

### L3. `memory://<space>` resource index silently truncates at 1000 while claiming completeness

`resources.rs:144-149` sets `lookup.limit = stats.live` with the comment "the
index is complete by construction", but `direct_lookup` clamps every limit to
`MAX_POOL = 1000` (`queries/read.rs:243-244`, `:85`). A space with 1500 live
claims serves `live: 1500` beside 1000 entries — exactly the quiet truncation
the comment promises away.

### L4. Idle-shutdown races: a session arriving at the deadline dies silently; a respawned daemon can lose the lock race and the client hangs 120 s

(a) A connection landing in the listener backlog as `idle_elapsed` fires is
reset when the loop breaks and drops the listener
(`daemon/serve.rs:65-95`); the client already connected, so it skips spawning,
writes its handshake, gets EOF, and exits **0** (H2's pump semantics) — a
silently dead session. (b) A client that arrives just after the timer fires
connect-fails, takes the spawn lock, removes the stale socket, and spawns a
daemon while the dying one still holds the data-dir lock; the new daemon exits
at `lock::acquire` (`serve.rs:34`, `lock.rs:41-57`), and the client polls a
socket that will never appear for the full `READY_DEADLINE` (120 s) before
erroring (`client.rs:177-194`). No spawn retry. The shutdown unlink itself is
safe (still under the lock), so this is a hang-then-error, not corruption.

### L5. `count_matching` is an unindexed COUNT on every filled recall page

Whenever a recall fills `k` (default 10 — i.e. almost always on a real store),
a `SELECT count() … GROUP ALL` over the space runs
(`recall.rs:329-338`, `queries/read.rs:259-262`). `space` has no standalone
index (`v1_schema.surql` — only the compound unique and the `.*` indexes), so
this is O(rows-in-space) per recall. Fine at 10³, a per-call tax at 10⁵.

### L6. `ensure_embedder` first-run race on a shared remote server

Two fresh sessions with different embedders connecting concurrently to an
empty `ws://` store both read no recorded pair and both `UPSERT` theirs —
last writer wins, no transaction or guard (`migrate.rs:70-121`). Vectors from
two models then share the index the guard exists to protect. Embedded engines
are immune (single writer); remote multi-version/multi-config is exactly the
deployment the check was built for. (Schema migrations themselves are
concurrency-safe: `IF NOT EXISTS` + guarded backfills, and `SchemaTooNew`
stops older binaries at connect — but an *already-connected* older daemon
keeps writing v(n-1)-shaped rows until its writes start failing type
coercion, which surfaces as raw DB errors mid-session.)

### L7. The near-dup gate's fixed probe (64) degrades in crowded stores

`NEAR_DUP_PROBE = 64` candidates are drawn bare, then filtered to the space
and to live rows (`queries/read.rs:56-61, 356-370`). In a shared store where
one space's rows are a minority — or where a topic has accumulated many
closed rows near the probe — all 64 nearest can be foreign/dead, the true
in-space nearest neighbour never surfaces, and the gate silently passes a
restatement. Only the exact-hash gate remains, so paraphrase duplicates
accumulate proportionally to store crowding; consolidate is the only
backstop.

---

## Notes against the project's own stated principles (idea.md §3.3 / §4)

- **No server-side LLM / no scheduler / stdout discipline: clean.** No hidden
  heuristics beyond documented thresholds; the only time-triggered actor is
  the startup prune (fast class only, soft-close, no thread); stdout is
  enforced by clippy deny with `oneshot::context` as the single sanctioned
  exception (`main.rs:1-4`, `oneshot.rs:37`).
- **Provenance on every write: enforced** — `source` is schema-required with
  `agent` as the floor; reflect requires citations and validates them
  cross-table (`reflect.rs:298-362`).
- **Poisoning surface:** context collapses whitespace per entry
  (`context.rs:107-111`), so a stored claim cannot smuggle a fake `##` heading
  or extra bullets into the briefing — good. Residual: content is otherwise
  unsanitised and uncapped; a claim can end in a backtick-quoted ULID and
  visually forge an id trailer in the context block, and single-line
  instruction-shaped text ("ignore previous…") flows into the prompt as data.
  Accepted design (agent-gated writes), but the absence of any length cap is
  M6's twin.
- **Unbounded growth:** the prune covers only `fast`; `normal`/`slow` decay is
  a ranking penalty, never removal — by design ("decay as ranking penalty
  rather than deletion"). Episodes and chunks, however, have **no**
  countermeasure at all: never pruned, never consolidated, only exact-hash
  deduped; nothing even surfaces "your episode table is 2 GB" (inspect stats
  show counts, not sizes). The #1 documented failure of LLM-free servers is
  mitigated for claims and merely deferred for episodes.

---

## Checked and cleared

- `scoring::rank` division-by-zero / all-tied / empty pools; `retention` at
  extremes (clamps, exp underflow, future timestamps clamped at 0 days).
- Hop arm: skipped when caller filters entities; copies kinds/tags/liveness;
  `reserve_tail` preserves sort order and never touches the head; hop weight
  provably below mid-page primary hits.
- `supersedes` cross-space: refused by `ensure_memories_exist`; vanished
  targets caught by in-transaction count THROW; self-supersession of the dup
  row excluded (`array::complement`).
- Purge leaving dangling `derived_from`: rendered without fetch
  (`record::table/id` on the link), `inspect` maps a purged source episode to
  "nothing left to quote"; soft-forgotten citations stay readable.
- Forget dry-run gate: per-session (daemon builds one service per
  connection), single-slot, consumed on use, `purge` part of the scope,
  poisoned-lock fails closed.
- Chunking: lossless up to whitespace, budget-respecting, no panic on
  unicode (proptested); chunk `occurred_at` denormalisation and the
  `NONE <= $as_of` falsy fallback.
- as_of window logic itself (half-open, chunk arms dated, filters off the
  KNN scan per issue #40 — pinned by tests).
- Migrations: idempotent bootstrap, guarded backfills, `SchemaTooNew`,
  reindex resumability (meta written before the loop; pending rows as the
  resume marker; progress-or-refuse loop guard).
- Daemon: spawn lock (created+try_lock with deadline, released on crash),
  stale-socket removal under the spawn lock, bind-last-after-migrate,
  `process_group(0)`, 0o700 data dir, per-session tool_desc/pool/max_k/space
  application, buffered-reader handoff to rmcp (no lost `initialize`).
- Env inheritance by the spawned daemon: benign — every session-varying field
  travels in the handshake or as explicit args; credentials only apply to
  remote engines, which never daemonize.
- stdout/stderr discipline and log filtering (allow-list default).
- Reinforcement failure handling: logged, non-fatal, drift bounded by the
  strength cap; `REINFORCE` no-ops on missing ids by design.
- context: budget floor refused, second render pass reserves the TRIMMED
  note, dedupe across sections, no reinforcement (deliberate, documented).
