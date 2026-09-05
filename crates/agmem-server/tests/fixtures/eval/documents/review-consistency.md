# agmem docs-vs-code consistency review — 2026-08-31

Scope: README.md, docs/design.md, docs/idea.md, docs/tool-descriptions.md vs the
four crates at workspace v0.1.3 (Cargo.toml:11). Every claim below carries
file:line for both sides. Verified-clean items are listed at the end so the
next review doesn't re-do them.

## Findings

### 1. HIGH — Daemon handshake never checks the binary version, only a hand-bumped protocol number
- `crates/agmem-server/src/daemon/mod.rs:54` — `pub const PROTOCOL_VERSION: u32 = 2;`
  with the comment "Bumped whenever the handshake or the socket's meaning
  changes" (i.e. deliberately *not* on releases; "v2 added `tool_desc`").
- `daemon/mod.rs:107-108` — the `Handshake` carries only this `version`; no
  `CARGO_PKG_VERSION` anywhere in the server crate (`rg env!` finds none).
- `daemon/mod.rs:144-151` — `accept()` refuses only on `asked.version != self.version`,
  plus db_url/embedder equality.
- Consequence: a v0.1.2 daemon and a v0.1.3 session both speak protocol v2, so
  after an upgrade the already-running daemon keeps serving the **old code**
  for every attaching session — indefinitely with `AGMEM_IDLE_TIMEOUT=0`
  (config.rs:103-111). This is exactly the stale-daemon gotcha the project
  memory records as having cost time. `--doctor` reports "shared daemon
  serving <socket>" (doctor.rs:45) without saying which version it is.
- Fix shape: put `env!("CARGO_PKG_VERSION")` in the handshake and either refuse
  or (better) have the old daemon drain-and-exit so the new session respawns it.

### 2. MED — consolidate.rs's `compare()` doc asserts the exact opposite of the code three lines below it
- `crates/agmem-server/src/tools/consolidate.rs:252-253` — "The bands do not
  overlap, so no pair is ever reported under both names."
- `consolidate.rs:277-280` (in the same function) — "Not an `else`: the bands
  overlap on purpose. A pair above the clustering bar is the *likeliest*
  disagreement…"
- Everything else agrees with the code: `agmem-core/src/dedup.rs:105-111`
  ("the two lists `consolidate` returns do not partition, and cannot"),
  `dedup.rs:112-114` (`is_contradiction_candidate` = `>= CORRECTION_FLOOR`,
  no ceiling), design.md:920-930, README.md:431-434 ("the same pair may also
  appear under `near_duplicates`"). The stale sentence is the pre-#38 design
  that measurement overturned; behaviour is correct, the fn doc lies.

### 3. MED — consolidate's 20/20 output caps are silent, contradicting its own `note` contract
- `consolidate.rs:50-53` — `MAX_CLUSTERS = 20`, `MAX_CONTRADICTIONS = 20`
  (and `MAX_CLUSTER_MEMBERS = 8`); `consolidate.rs:226-228` truncates both
  lists after sorting.
- `consolidate.rs:391-406` — `note()` reports only two limits: no embedder,
  and the `MAX_POOL` scan truncation. The list caps produce **no note**.
- But `consolidate.rs:96-97` documents `note` as "Present only when something
  limited the answer", and design.md:377 says "note? // present only when
  something limited it". A store with 25 clusters silently drops 5 with no
  trace — the same silent-page shape `recall.truncated` (recall.rs:83-113)
  exists to prevent. None of the three caps appears in design.md or README.

### 4. MED — README doctor sample output is stale on two counts
- README.md:60 — sample shows `ok    schema               v1`. The binary
  writes schema **v4**: `agmem-store/src/migrate.rs:20-28` (four migrations,
  `SCHEMA_VERSION = MIGRATIONS.len()`), printed as `v{version}` at
  doctor.rs:80.
- README.md:53-64 — sample has no `vector coverage` line; doctor prints one on
  every run where the embedder loads and the store opened
  (doctor.rs:126, 146-163: "ok vector coverage every row carries a vector").
- Related: README.md:632 — "`--doctor` says `skip` on **three** lines" with a
  daemon running; the code prints exactly **two** skip lines
  (doctor.rs:46-47: "skip single-writer lock", "skip database + schema" —
  the second line merged two former checks).

### 5. MED — README configuration table omits flags that exist
- README.md:531 — `--embedder` row says "`fastembed`, or `none` for BM25-only";
  config.rs:151-160 defines three values (`fastembed | static | none`) and
  README's own install section (README.md:21-22, 36) sells `--embedder static`.
- README.md:525-540 — no row for `--reindex` at all; it exists
  (config.rs:85-90), is the documented remedy in doctor's own FAIL message
  (doctor.rs:154-155) and in design.md:879. README never mentions reindex
  anywhere (`rg reindex README.md` → 0 hits) even though the vector-coverage
  failure tells users to run it.

### 6. LOW/MED — design.md §3.1 `remember` output sketch is two fields behind the code
- design.md:310-312 — sketch: `{ created, duplicates: [{ id, of, similarity }],
  superseded, episode }`. Code returns `content` inside each duplicate
  (remember.rs:170-177) and an entire `related` list (remember.rs:120-133,
  Related at :142-159), both of which the README documents at length
  (README.md:249-271) and design's own §9 items discuss (design.md:1082-1085).
  The section self-describes as a sketch, but it is the tool-contract section
  and it predates issues #38/#41.
- Cosmetic sibling: design.md:438 sketches the context header as
  `# Memory context (space: <name> + user)`; code renders `spaces:`
  (context.rs:303).

### 7. LOW — README "Status: v0.1.2" vs workspace 0.1.3
- README.md:10 — "Status: v0.1.2, backlog empty". Cargo.toml:11 —
  `version = "0.1.3"`. The latest tag is v0.1.2 (`git tag`), so the line
  matches the last *release*, not the tree; the hand-maintained status line
  will silently lag every release-plz merge.

### 8. LOW — design.md §4 code-structure tree is stale
- design.md:586 — `└── tests/  # workspace integration tests (see §7)`. No
  root `tests/` exists; integration tests live in `crates/agmem-server/tests/`,
  `crates/agmem-store/tests/`, `crates/agmem-embed/tests/` (git ls-files).
- design.md:552-556 — lists `queries.rs`, `repo.rs`, `types.rs` as files; the
  store now has `queries/{mod,read,reindex,write}.rs` and `repo/` dirs, no
  `types.rs` at that path.
- design.md:577-582 — tools/ tree lists only five tools (no consolidate.rs,
  reflect.rs) and the server tree omits resources.rs, oneshot.rs, startup.rs,
  lock.rs, doctor.rs, reindex.rs, embedder.rs — all of which exist.

### 9. LOW — design.md §6 config table lags the CLI
- design.md:951-964 — no `--reindex` row (flag: config.rs:89-90), no
  `--db-user`/`--db-pass` flag forms (env-only listed; flags at
  config.rs:37-47), `AGMEM_POOL`/`AGMEM_MAX_K` listed env-only (flags
  `--pool`/`--max-k` at config.rs:60-70), no `--log-file` flag form
  (config.rs:78-79), and no `context` one-shot subcommand (config.rs:113-148,
  issue #46) — README documents the subcommand (README.md:364-375, 540);
  design §6 never mentions it.

### 10. LOW — forget-by-query confirmation covers the scope, not the snapshot
- forget.rs:199-215 — the executing call `confirm()`s the (query, spaces,
  purge) triple and then **re-runs** `by_query`; rows written (or newly
  matching) between the dry run and the execution are closed/purged without
  ever having been shown. forget.rs:150-152 half-acknowledges this
  ("the second execution acts on a store the first one changed") but the
  refusal text ("read what it matched, then send this call again unchanged",
  forget.rs:168-169) and README.md:394-396 present the dry-run list as the
  scope being confirmed. With a shared daemon, another session can widen the
  match set in between. Purge makes this unrecoverable. Design frames scope
  confirmation "by construction" (design.md:424-425) — as-designed, but the
  promise oversells the guarantee.

### 11. LOW — design §2.3 calls reinforcement "batched, fire-and-forget"; the code deliberately awaits it
- design.md:246-248 — "`last_accessed = now` (batched, fire-and-forget)".
- recall.rs:442-453 — one awaited `UPDATE`; failures are logged and dropped
  ("It is awaited rather than detached because … a spawned task would make
  'was this reinforced' untestable"). Error-swallowing matches the spirit;
  "batched/fire-and-forget" no longer describes the mechanism. Failure mode is
  sound: hits still returned, warn-level log (recall.rs:451).

### 12. INFO — near-dup gate: "against top-1 neighbor" vs all 64 probes
- design.md:222 — "cosine ≥ 0.95 against top-1 neighbor". Code fetches up to
  `NEAR_DUP_PROBE = 64` neighbours per probe (queries/read.rs:61, :363) and
  reports **every** one ≥ 0.95 as a duplicate and every one in [0.75, 0.95) as
  related (remember.rs:250-268). Block/allow decision is equivalent to top-1
  (max similarity decides); the reporting is a superset of the doc.

### 13. INFO — history.txt at the repo root
- Untracked and gitignored on purpose: .gitignore:16-17 ("surreal sql shell
  history, dropped wherever the CLI is run"). 34K of manual KNN probe queries
  (`<|64,80|>` — matching NEAR_DUP_PROBE/EF_SEARCH band-probe work, cf.
  scripts/band-probe.nu) with raw embedding vectors. Harmless local artifact;
  safe to delete. `git status --ignored` shows only .DS_Store, .claude,
  history.txt, target/ — nothing tracked that shouldn't be, nothing ignored
  that should be tracked.

## Verified clean (doc claim == code, with locations)

- Decay rates pinned=0/slow=0.005/normal=0.02/fast=0.15: design.md:243 ==
  scoring.rs:55-60. Retention clamp [0.01, 5]: design.md:241 == scoring.rs:22,
  :39, :106. MAX_STABILITY=5 reinforcement cap (issue #52): design.md:247 ==
  scoring.rs:39; sweep clamps identically (queries/write.rs:189,
  repo/write.rs:466-472; consolidate.rs:379-382).
- Rescore `0.6·norm(rrf) + 0.25·retention + 0.15·importance`: design.md:728 ==
  scoring.rs:14-18, :255. Importance per class (fact example 0.5 in
  README.md:292) == scoring.rs:68-73.
- Near-dup ≥ 0.95 (dedup.rs:14), related band [0.75, 0.95) (dedup.rs:53, :68),
  cluster ≥ 0.90 (dedup.rs:86), contradiction ≥ 0.75 ∧ shared entity, no
  ceiling (dedup.rs:112-114; consolidate.rs:281-283) — all == design.md:679-680,
  :920-930.
- Pool default 64 (config.rs:60), max_k 50 (config.rs:64-70), k default 10
  refused-not-clamped above ceiling (recall.rs:27, :416-423) == design.md:317.
- KNN over-fetch 4× → `<|256,80|>` at pool 64: queries/read.rs:30 (EF 80),
  :47 (OVER_FETCH 4), :186-190 == design.md:709. Fulltext term cap 12:
  read.rs:76, :139 (no design number stated beyond §9 narrative — consistent).
- Hop: seeds top-3 (hop.rs:34), cap 5 (:37), hub ≥ 50% of ≥ 8 pool (:42, :45),
  scan 32 (:54), vote 8 (:57), weight 0.5/(60+rank) (:64, :215-217 with
  RRF_K=60 at read.rs:79) == design.md:716-724.
- Startup fast-prune at retention 0.05 (~20 days): scoring.rs:48,
  repo/write.rs:460-476 (fast-only via PRUNE_CLASS), startup prune
  startup.rs:18-23 == design.md:254-255, :885.
- budget_chars: min 200 refused (context.rs:36), default 6000 (:31) ==
  design.md:473, :334. Sections/order/ids/no-reinforce all match §3.2.
- Chunk target ~1500 chars: chunk.rs:13 == design.md:463, :545.
- Schema: SCHEMA_VERSION=4 (migrate.rs:20-28); v1 DDL == design.md:142-208
  field-for-field; v2 derived_from, v3 supersedes→list, v4 chunk occurred_at
  all present as design describes (design.md:173-175, :198, :201-202).
  `--reindex` exists (config.rs:89-90, reindex.rs, tests/reindex.rs) as
  design §5.5:879 claims.
- Tool annotations: all seven set as design.md:281-289 tables, including the
  explicit `destructive_hint=false` on read-only tools (service.rs:189, :215,
  :249, :290, :312, :354, :389; wire-level proof in
  tests/snapshots/protocol__list_tools.snap:369-371 etc.; the
  missing-hint-defaults-destructive trap is pinned by tools/mod.rs:249-271).
- Protocol insta snapshots over list_tools exist as design §7.3 claims
  (tests/protocol.rs:63-89, snapshots/protocol__{initialize,list_prompts,list_tools}.snap).
- `memory://` resources fully implemented (resources.rs, two-form grammar) ==
  design.md:516-524; entity table deliberately unbuilt == README.md:13-15 and
  design phase-3 conditional (design.md:1019-1022) — no half-built remnant.
- Kind→decay defaults fact/normal, lesson/slow, instruction/pinned:
  model.rs:203-209 == design.md:231-236.
- Lock: advisory flock dies with the process (lock.rs:41-46) — no stale-lock
  window; stale *socket* is detected and removed (daemon/client.rs:84-88).
- forget: episode close refused, purge-only (forget.rs:480-488); chunk ids
  refused (forget.rs:512-522); by-query is BM25-only (forget.rs:380-382) ==
  README.md:397-405. Confirmation slot is per-session (service-owned,
  forget.rs:15-16, :146-154) and single-use.
- TODO/FIXME/unimplemented sweep: none in prod code; only three justified
  `dead_code` expects (embedder.rs:42, tests/eval/scenario.rs:28,
  tests/harness/mod.rs:10).
- Space vocabulary table (README.md:515-521) == tools/mod.rs:100-120
  (consolidate's current-alone default: consolidate.rs:186-190 ==
  README.md:456-458, design.md:369).
